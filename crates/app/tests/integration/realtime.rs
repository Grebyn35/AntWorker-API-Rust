//! The stream of server-sent events (the realtime spec), through the API's own routes, with the
//! tests' announcing routes to publish events on demand (TEST-05).

use std::time::Duration;

use http::StatusCode;
use serde_json::json;

use crate::support::{
    Api, Options, announcements,
    streams::{self, Stream},
    users::{self, SignedIn},
    workspaces,
};

/// How often a stream sends a comment, by default (REALTIME-003).
const HEARTBEAT: Duration = Duration::from_secs(15);

/// The API, with the announcing routes added.
async fn api() -> Api {
    Api::with(Options::routes(announcements::routes())).await
}

/// Publishes `count` events to `user`, in one unit of work.
async fn announce(api: &Api, user: &SignedIn, count: u32) {
    let response = api
        .post("/v1/announcements", &json!({ "count": count }))
        .bearer(&user.access)
        .send()
        .await;
    assert_eq!(
        response.status,
        StatusCode::NO_CONTENT,
        "{}",
        response.json()
    );
}

/// Publishes `count` events to the members of `workspace`, in one unit of work, as `user`.
async fn post(api: &Api, user: &SignedIn, workspace: &str, count: u32) {
    let response = api
        .post(
            &format!("/v1/workspaces/{workspace}/announcements"),
            &json!({ "count": count }),
        )
        .bearer(&user.access)
        .send()
        .await;
    assert_eq!(
        response.status,
        StatusCode::NO_CONTENT,
        "{}",
        response.json()
    );
}

/// The `n` of each frame's payload, with its type.
fn numbered(frames: &[streams::Frame]) -> Vec<(String, u64)> {
    frames
        .iter()
        .map(|frame| {
            (
                frame.event.clone().unwrap(),
                frame.payload()["n"].as_u64().unwrap(),
            )
        })
        .collect()
}

/// The next `count` events of `stream`.
async fn events(stream: &mut Stream, count: usize) -> Vec<streams::Frame> {
    let mut frames = Vec::new();
    for _ in 0..count {
        frames.push(stream.event().await);
    }
    frames
}

#[tokio::test]
async fn realtime_001_a_stream_delivers_its_caller_s_events_and_their_workspaces_in_commit_order() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "member")]).await;
    let mut stream = streams::connect(&api, &ada.access, None).await;
    let opened = stream.opened().await;

    announce(&api, &ada, 2).await;
    post(&api, &ada, &workspace, 1).await;
    announce(&api, &ada, 1).await;

    assert_eq!(opened.payload(), &json!({ "truncated": false }));
    assert_eq!(
        numbered(&events(&mut stream, 4).await),
        [
            ("announcement.made".to_owned(), 1),
            ("announcement.made".to_owned(), 2),
            ("announcement.posted".to_owned(), 1),
            ("announcement.made".to_owned(), 1),
        ]
    );
}

#[tokio::test]
async fn realtime_001_a_browser_opens_its_stream_with_its_access_cookie() {
    let api =
        Api::with(Options::routes(announcements::routes()).set("CORS_ALLOWED_ORIGINS", users::WEB))
            .await;
    let signed_in = users::browser_sign_in(&api).await;
    let cookies = users::cookies(&signed_in);

    let opened = streams::open(&api, &[("cookie", &cookies), ("origin", users::WEB)]).await;

    let mut stream = opened.stream().expect("the browser's stream opens");
    stream.opened().await;
}

#[tokio::test]
async fn realtime_001_a_request_without_a_session_opens_no_stream() {
    let api = api().await;

    let refused = streams::open(&api, &[]).await;

    let response = refused
        .refused()
        .expect("no stream opens without a session");
    assert_eq!(response.status, StatusCode::UNAUTHORIZED);
    assert_eq!(response.json()["code"], "request.unauthenticated");
}

#[tokio::test]
async fn realtime_002_an_event_s_frame_carries_its_id_its_type_and_the_trace_of_its_publisher() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::connect(&api, &ada.access, None).await;
    let opened = stream.opened().await;

    let published = api
        .post("/v1/announcements", &json!({ "count": 1 }))
        .bearer(&ada.access)
        .send()
        .await;
    let frame = stream.event().await;

    // `server-timing: trace;desc=00-<trace ID>-<span ID>-<flags>` (API-16).
    let trace_id = published
        .header("server-timing")
        .split('-')
        .nth(1)
        .unwrap()
        .to_owned();
    let id = api
        .database()
        .owner_reads("SELECT id::text FROM events WHERE type = 'announcement.made'")
        .await;
    assert_eq!(frame.id.as_deref(), Some(id[0].as_str()));
    assert_eq!(frame.event.as_deref(), Some("announcement.made"));
    assert_eq!(
        frame.data,
        Some(json!({ "data": { "n": 1 }, "trace_id": trace_id }))
    );
    let own = opened.data.unwrap()["trace_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(own, stream.trace_id());
}

#[tokio::test]
async fn realtime_003_a_stream_sends_a_comment_at_every_heartbeat() {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::connect(&api, &ada.access, None).await;
    let opened = stream.opened().await;

    let mut beats = Vec::new();
    for _ in 0..2 {
        clock.sleeping(HEARTBEAT).await;
        clock.advance(HEARTBEAT);
        beats.push(stream.frame().await.unwrap());
    }

    for beat in beats {
        assert!(beat.comment, "{beat:?}");
        assert_eq!(beat.id, None);
    }
    assert_eq!(stream.last_event_id, opened.id);
}

#[tokio::test]
async fn realtime_004_a_reconnect_replays_what_the_client_missed_then_goes_on_live() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut first = streams::connect(&api, &ada.access, None).await;
    first.opened().await;
    announce(&api, &ada, 1).await;
    first.event().await;
    let last = first.last_event_id.clone();
    drop(first);

    announce(&api, &ada, 3).await;
    let mut again = streams::connect(&api, &ada.access, last.as_deref()).await;
    let opened = again.opened().await;
    let replayed = events(&mut again, 3).await;
    announce(&api, &ada, 1).await;
    let live = again.event().await;

    assert_eq!(opened.payload(), &json!({ "truncated": false }));
    assert_eq!(opened.id, None);
    assert_eq!(
        numbered(&replayed),
        [
            ("announcement.made".to_owned(), 1),
            ("announcement.made".to_owned(), 2),
            ("announcement.made".to_owned(), 3),
        ]
    );
    assert_eq!(live.payload()["n"], 1);
}

#[tokio::test]
async fn realtime_004_more_than_1000_waiting_events_truncate_the_replay() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut first = streams::connect(&api, &ada.access, None).await;
    first.opened().await;
    announce(&api, &ada, 1).await;
    first.event().await;
    let last = first.last_event_id.clone();
    drop(first);

    announce(&api, &ada, 1_001).await;
    let mut again = streams::connect(&api, &ada.access, last.as_deref()).await;
    let opened = again.opened().await;
    announce(&api, &ada, 1).await;
    let live = again.event().await;

    let newest = api
        .database()
        .owner_reads("SELECT id::text FROM events ORDER BY position DESC LIMIT 1 OFFSET 1")
        .await;
    assert_eq!(opened.payload(), &json!({ "truncated": true }));
    // The reconnect resumes after the newest event the replay skipped.
    assert_eq!(opened.id.as_deref(), Some(newest[0].as_str()));
    assert_eq!(live.payload()["n"], 1);
}

#[tokio::test]
async fn realtime_004_an_id_the_api_doesn_t_hold_truncates_the_replay() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;

    let mut truncated = Vec::new();
    for id in ["0190f1f3-9a4e-7c2a-9b1d-5e2f6a7b8c9d", "not-an-event"] {
        let mut stream = streams::connect(&api, &ada.access, Some(id)).await;
        truncated.push(stream.opened().await);
    }

    for opened in truncated {
        assert_eq!(opened.payload(), &json!({ "truncated": true }));
        // No event of hers exists, so a reconnect names none.
        assert_eq!(
            opened.id.as_deref(),
            Some("00000000-0000-0000-0000-000000000000")
        );
    }
}

#[tokio::test]
async fn realtime_004_a_stream_that_closes_before_its_first_event_resumes_where_it_started() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    announce(&api, &ada, 1).await;
    let mut stream = streams::connect(&api, &ada.access, None).await;
    let opened = stream.opened().await;

    // Her own workspace changes her memberships, which closes the stream before any event.
    let created = api
        .post("/v1/workspaces", &json!({ "name": "Analytical" }))
        .bearer(&ada.access)
        .send()
        .await;
    let (before, reason) = stream.closed().await;
    announce(&api, &ada, 1).await;
    let mut again = streams::connect(&api, &ada.access, opened.id.as_deref()).await;
    again.opened().await;
    let replayed = events(&mut again, 2).await;

    let announced = api
        .database()
        .owner_reads("SELECT id::text FROM events ORDER BY position LIMIT 1")
        .await;
    assert_eq!(created.status, StatusCode::CREATED);
    assert_eq!(opened.id.as_deref(), Some(announced[0].as_str()));
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(reason, "access_changed");
    let types: Vec<Option<String>> = replayed.into_iter().map(|frame| frame.event).collect();
    assert_eq!(
        types,
        [
            Some("membership.created".to_owned()),
            Some("announcement.made".to_owned())
        ]
    );
}

#[tokio::test]
async fn realtime_005_signing_out_closes_that_session_s_streams_and_a_reconnect_is_refused() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let other = users::new_session(&api, "ada@example.com").await;
    let mut signed_out = streams::connect(&api, &ada.access, None).await;
    signed_out.opened().await;
    let mut kept = streams::connect(&api, &other.access, None).await;
    kept.opened().await;

    let response = api
        .request(http::Method::DELETE, "/v1/users/me/sessions/current")
        .bearer(&ada.access)
        .send()
        .await;
    let (_, reason) = signed_out.closed().await;
    announce(&api, &other, 1).await;
    let still = kept.event().await;
    let reconnect = streams::open_as(&api, &ada.access).await;

    assert_eq!(response.status, StatusCode::NO_CONTENT);
    assert_eq!(reason, "session_ended");
    assert_eq!(still.event.as_deref(), Some("announcement.made"));
    let refused = reconnect
        .refused()
        .expect("a session that ended opens no stream");
    assert_eq!(refused.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn realtime_005_a_replica_that_stops_closes_its_streams_with_shutdown() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::connect(&api, &ada.access, None).await;
    stream.opened().await;

    api.readiness().stop();
    let (_, reason) = stream.closed().await;
    let refused = streams::open_as(&api, &ada.access).await;

    assert_eq!(reason, "shutdown");
    let response = refused
        .refused()
        .expect("a stopping replica opens no stream");
    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.json()["code"], "realtime.unavailable");
    assert_eq!(response.header("retry-after"), "1");
}

#[tokio::test]
async fn realtime_006_a_user_holds_at_most_10_streams_on_a_replica() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let mut open = Vec::new();
    for _ in 0..10 {
        open.push(streams::connect(&api, &ada.access, None).await);
    }

    let eleventh = streams::open_as(&api, &ada.access).await;
    let other_user = streams::open_as(&api, &grace.access).await;
    drop(open.pop());
    let after_one_closed = streams::open_as(&api, &ada.access).await;

    let refused = eleventh.refused().expect("an eleventh stream is refused");
    assert_eq!(refused.status, StatusCode::CONFLICT);
    assert_eq!(refused.json()["code"], "realtime.too_many_streams");
    assert!(other_user.stream().is_some());
    assert!(after_one_closed.stream().is_some());
}
