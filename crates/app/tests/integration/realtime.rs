//! The stream of server-sent events (the realtime spec), through the API's own routes, with the
//! tests' announcing routes to publish events on demand (TEST-04).

use std::time::Duration;

use http::StatusCode;
use serde_json::json;

use crate::support::{
    Api, Options,
    announcements::{self, Note, published::NOTED},
    streams::{self, Stream},
    users::{self, SignedIn},
    workspaces,
};

/// How often a stream sends its heartbeat, by default (REALTIME-003).
const HEARTBEAT: Duration = Duration::from_secs(15);

/// How often the hub checks the listener (PLATFORM-061).
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

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

/// Has `user` leave `workspace`, which changes their access.
async fn leave(api: &Api, user: &SignedIn, workspace: &str) -> crate::support::api::Response {
    api.request(
        http::Method::POST,
        &format!("/v1/workspaces/{workspace}/leave"),
    )
    .bearer(&user.access)
    .send()
    .await
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
    // The registration's session published its start (AUTH-046).
    api.database().owner_executes("DELETE FROM events").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "member")]).await;
    let mut stream = streams::connect(&api, &ada.access, None).await;
    let opened = stream.opened().await;

    announce(&api, &ada, 2).await;
    post(&api, &ada, &workspace, 1).await;
    announce(&api, &ada, 1).await;

    assert_eq!(
        opened.payload(),
        &json!({ "truncated": false, "heartbeat_seconds": 15 })
    );
    // No event of hers lies before the stream, so a reconnect resumes from none.
    assert_eq!(
        opened.id.as_deref(),
        Some("00000000-0000-0000-0000-000000000000")
    );
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
async fn realtime_001_a_stream_s_opened_id_is_the_newest_event_its_caller_may_receive() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    // The registrations' sessions published their starts (AUTH-046).
    api.database().owner_executes("DELETE FROM events").await;
    announce(&api, &ada, 1).await;
    // A newer event, which only Grace may receive.
    announce(&api, &grace, 1).await;

    let mut stream = streams::connect(&api, &ada.access, None).await;
    let opened = stream.opened().await;

    let hers = api
        .database()
        .owner_reads("SELECT id::text FROM events ORDER BY position LIMIT 1")
        .await;
    assert_eq!(opened.id.as_deref(), Some(hers[0].as_str()));
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
async fn realtime_001_the_first_frame_is_stream_opened_though_a_heartbeat_is_due() {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;

    // Each stream is read only once its first heartbeat is due. Eight of them, since a stream
    // that took whichever of the two came to hand would send the heartbeat first in some.
    let mut firsts = Vec::new();
    for _ in 0..8 {
        let mut stream = streams::connect(&api, &ada.access, None).await;
        clock.sleeping(HEARTBEAT).await;
        clock.advance(HEARTBEAT);
        let first = stream.frame().await.unwrap();
        let second = stream.frame().await.unwrap();
        firsts.push((first.event, second.event));
    }

    for (first, second) in firsts {
        assert_eq!(first.as_deref(), Some("stream.opened"));
        assert_eq!(second.as_deref(), Some(streams::BEAT));
    }
}

#[tokio::test]
async fn realtime_002_an_event_s_frame_carries_its_id_its_name_and_the_trace_of_its_publisher() {
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

    let trace_id = published.trace_id();
    let id = api
        .database()
        .owner_reads("SELECT id::text FROM events WHERE name = 'announcement.made'")
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
async fn realtime_002_an_event_carries_the_trace_id_of_the_traceparent_its_request_sent() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::connect(&api, &ada.access, None).await;
    stream.opened().await;
    // The trace ID that the client chose, so it knows it before any frame arrives.
    let own = "0af7651916cd43dd8448eb211c80319c";

    let published = api
        .post("/v1/announcements", &json!({ "count": 1 }))
        .bearer(&ada.access)
        .header("traceparent", &format!("00-{own}-b7ad6b7169203331-01"))
        .send()
        .await;
    announce(&api, &ada, 1).await;
    let caused = stream.event().await;
    let other = stream.event().await;

    assert_eq!(published.status, StatusCode::NO_CONTENT);
    assert_eq!(caused.data.unwrap()["trace_id"], own);
    assert_ne!(other.data.unwrap()["trace_id"], own);
}

#[tokio::test]
async fn realtime_002_an_event_s_frame_carries_its_trace_id_on_its_publisher_s_streams_alone() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Analytical",
        &[(&ada.user_id, "admin"), (&grace.user_id, "member")],
    )
    .await;
    let mut publisher = streams::opened(&api, &ada).await;
    let mut member = streams::opened(&api, &grace).await;
    // The trace ID that the publisher's client chose, which a member who knew it could send
    // requests of their own under.
    let chosen = "0af7651916cd43dd8448eb211c80319c";
    let traceparent = format!("00-{chosen}-b7ad6b7169203331-01");

    let path = format!("/v1/workspaces/{workspace}/announcements");
    let hers = api
        .post(&path, &json!({ "count": 1 }))
        .bearer(&ada.access)
        .header("traceparent", &traceparent)
        .send()
        .await;
    let theirs = api
        .post(&path, &json!({ "count": 1 }))
        .bearer(&grace.access)
        .header("traceparent", &traceparent)
        .send()
        .await;
    let to_publisher = [publisher.event().await, publisher.event().await];
    let to_member = [member.event().await, member.event().await];

    assert_eq!(hers.status, StatusCode::NO_CONTENT);
    assert_eq!(theirs.status, StatusCode::NO_CONTENT);
    assert_eq!(theirs.trace_id(), chosen);
    let payload = json!({ "workspace_id": workspace, "n": 1 });
    // Each reads the trace ID of her own request's event, and none of the other's, though both
    // events were published under one trace.
    assert_eq!(
        to_publisher.map(|frame| frame.data.unwrap()),
        [
            json!({ "data": payload, "trace_id": chosen }),
            json!({ "data": payload, "trace_id": null }),
        ]
    );
    assert_eq!(
        to_member.map(|frame| frame.data.unwrap()),
        [
            json!({ "data": payload, "trace_id": null }),
            json!({ "data": payload, "trace_id": chosen }),
        ]
    );
}

#[tokio::test]
async fn realtime_003_a_stream_sends_stream_beat_at_every_heartbeat() {
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

    assert_eq!(opened.payload()["heartbeat_seconds"], HEARTBEAT.as_secs());
    let own = stream.trace_id();
    for beat in beats {
        assert_eq!(beat.event.as_deref(), Some(streams::BEAT));
        assert_eq!(beat.id, None);
        assert_eq!(beat.data, Some(json!({ "data": {}, "trace_id": own })));
    }
    assert_eq!(stream.last_event_id, opened.id);
}

#[tokio::test]
async fn realtime_003_the_heartbeat_comes_while_events_come_too() {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::connect(&api, &ada.access, None).await;
    stream.opened().await;

    clock.sleeping(HEARTBEAT).await;
    // An event 5 seconds before the heartbeat is due doesn't put the heartbeat off.
    clock.advance(HEARTBEAT - Duration::from_secs(5));
    announce(&api, &ada, 1).await;
    let event = stream.frame().await.unwrap();
    clock.advance(Duration::from_secs(5));
    let beat = stream.frame().await.unwrap();

    assert_eq!(event.event.as_deref(), Some("announcement.made"));
    assert_eq!(beat.event.as_deref(), Some(streams::BEAT));
    // The heartbeat has no `id`, so a reconnect still resumes after the event.
    assert_eq!(stream.last_event_id, event.id);
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

    assert_eq!(opened.payload()["truncated"], false);
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

    // The live event, then the newest event the replay skipped.
    let newest = api
        .database()
        .owner_reads("SELECT id::text FROM events ORDER BY position DESC LIMIT 2")
        .await;
    assert_eq!(opened.payload()["truncated"], true);
    // The reconnect resumes after the newest event the replay skipped.
    assert_eq!(opened.id.as_deref(), Some(newest[1].as_str()));
    // The first event the stream sends is the live one, which no replayed event is.
    assert_eq!(live.id.as_deref(), Some(newest[0].as_str()));
}

#[tokio::test]
async fn realtime_004_an_id_the_api_doesn_t_hold_truncates_the_replay() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    // The registration's session published its start (AUTH-046).
    api.database().owner_executes("DELETE FROM events").await;

    let mut truncated = Vec::new();
    for id in ["0190f1f3-9a4e-7c2a-9b1d-5e2f6a7b8c9d", "not-an-event"] {
        let mut stream = streams::connect(&api, &ada.access, Some(id)).await;
        truncated.push(stream.opened().await);
    }
    // A header that isn't text at all names no event either.
    let mut unreadable = streams::open_with_bytes(&api, &ada.access, "last-event-id", b"\xff\xfe")
        .await
        .stream()
        .expect("the stream opened");
    truncated.push(unreadable.opened().await);

    for opened in truncated {
        assert_eq!(opened.payload()["truncated"], true);
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
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Analytical",
        &[(&grace.user_id, "owner"), (&ada.user_id, "member")],
    )
    .await;
    // The registrations' sessions published their starts (AUTH-046).
    api.database().owner_executes("DELETE FROM events").await;
    announce(&api, &ada, 1).await;
    let mut stream = streams::connect(&api, &ada.access, None).await;
    let opened = stream.opened().await;

    // Leaving a workspace changes her memberships, which closes the stream before any event.
    let left = leave(&api, &ada, &workspace).await;
    let (before, reason) = stream.closed().await;
    announce(&api, &ada, 1).await;
    let mut again = streams::connect(&api, &ada.access, opened.id.as_deref()).await;
    again.opened().await;
    let replayed = events(&mut again, 1).await;

    let announced = api
        .database()
        .owner_reads("SELECT id::text FROM events ORDER BY position LIMIT 1")
        .await;
    assert_eq!(left.status, StatusCode::NO_CONTENT);
    assert_eq!(opened.id.as_deref(), Some(announced[0].as_str()));
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(reason, "access_changed");
    let names: Vec<Option<String>> = replayed.into_iter().map(|frame| frame.event).collect();
    assert_eq!(names, [Some("announcement.made".to_owned())]);
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
    // The end of the session reaches the stream that stays (AUTH-046).
    let ended = kept.event().await;
    announce(&api, &other, 1).await;
    let still = kept.event().await;
    let reconnect = streams::open_as(&api, &ada.access).await;

    assert_eq!(response.status, StatusCode::NO_CONTENT);
    assert_eq!(reason, "session_ended");
    assert_eq!(ended.event.as_deref(), Some("session.ended"));
    assert_eq!(ended.payload(), &json!({ "session_id": ada.session_id }));
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

    assert_eq!(reason, "shutdown");
}

#[tokio::test]
async fn realtime_005_a_lost_connection_closes_the_stream_with_interrupted() {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::connect(&api, &ada.access, None).await;
    stream.opened().await;
    clock.sleeping(CHECK_INTERVAL).await;

    api.database().cut_off_app().await;
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_INTERVAL).await;
    api.database()
        .admin_executes(&format!(
            "GRANT CONNECT ON DATABASE \"{}\" TO app",
            api.database().name()
        ))
        .await;
    clock.advance(CHECK_INTERVAL);
    let (_, reason) = stream.closed().await;

    assert_eq!(reason, "interrupted");
}

#[tokio::test]
async fn realtime_007_a_replica_that_stops_opens_no_stream_and_answers_503() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;

    api.readiness().stop();
    let refused = streams::open_as(&api, &ada.access).await;

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
    assert!(!refused.headers.contains_key("retry-after"));
    assert!(other_user.stream().is_some());
    assert!(after_one_closed.stream().is_some());
}

#[tokio::test]
async fn realtime_002_an_event_that_no_request_published_has_no_trace_id_and_the_stream_s_frames_its_request_s()
 {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let user = platform::types::UserId::from_uuid(ada.user_id.parse().unwrap());
    let mut stream = streams::connect(&api, &ada.access, None).await;
    let opened = stream.opened().await;

    let mut uow = api
        .pool()
        .begin(&crate::support::client(), None)
        .await
        .unwrap();
    uow.publish_to_user(&NOTED, user, &Note { label: None })
        .await
        .unwrap();
    uow.commit().await.unwrap();
    let untraced = stream.event().await;
    clock.sleeping(HEARTBEAT).await;
    clock.advance(HEARTBEAT);
    let beat = stream.frame().await.unwrap();
    api.readiness().stop();
    let closing = stream.closing().await;

    assert_eq!(untraced.data, Some(json!({ "data": {}, "trace_id": null })));
    let own = stream.trace_id();
    assert_eq!(opened.data.unwrap()["trace_id"], own.as_str());
    assert_eq!(beat.event.as_deref(), Some(streams::BEAT));
    assert_eq!(beat.data.unwrap()["trace_id"], own.as_str());
    assert_eq!(closing.data.unwrap()["trace_id"], own.as_str());
}

#[tokio::test]
async fn realtime_003_the_heartbeat_is_a_setting_of_5_to_300_seconds() {
    for seconds in [4, 301] {
        let refused = crate::support::api::refusal(
            &Options::default().set("STREAM_HEARTBEAT_SECONDS", seconds),
        );
        assert!(refused.contains("STREAM_HEARTBEAT_SECONDS"), "{refused}");
        assert!(refused.contains("5 to 300"), "{refused}");
    }
    let every = Duration::from_secs(5);
    let api = Api::with(Options::default().set("STREAM_HEARTBEAT_SECONDS", every.as_secs())).await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::connect(&api, &ada.access, None).await;
    let opened = stream.opened().await;

    clock.sleeping(every).await;
    clock.advance(every);
    let beat = stream.frame().await.unwrap();

    assert_eq!(opened.payload()["heartbeat_seconds"], every.as_secs());
    assert_eq!(beat.event.as_deref(), Some(streams::BEAT));
}

#[tokio::test]
async fn realtime_005_an_access_change_closes_the_stream_with_access_changed() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Analytical",
        &[(&grace.user_id, "owner"), (&ada.user_id, "member")],
    )
    .await;
    let mut stream = streams::connect(&api, &ada.access, None).await;
    stream.opened().await;

    let left = leave(&api, &ada, &workspace).await;
    let (_, reason) = stream.closed().await;

    assert_eq!(left.status, StatusCode::NO_CONTENT);
    assert_eq!(reason, "access_changed");
}

#[tokio::test]
async fn realtime_005_a_client_that_reads_too_slowly_is_closed_with_lagging() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::connect(&api, &ada.access, None).await;
    stream.opened().await;

    announce(&api, &ada, 300).await;
    let (_, reason) = stream.closed().await;

    assert_eq!(reason, "lagging");
}

/// A job of the tests' own, which publishes an event to `user`, with the trace ID of its own run.
#[derive(serde::Deserialize, serde::Serialize)]
struct Publish {
    user: uuid::Uuid,
}

impl platform::jobs::Job for Publish {
    const KIND: &'static str = "tests.publish";
}

#[tokio::test]
async fn realtime_002_an_event_a_job_publishes_carries_no_trace_id() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut jobs = platform::jobs::Jobs::new(api.pool(), Duration::from_secs(86_400));
    let database = api.pool().clone();
    jobs.queued(move |context, job: Publish| {
        let database = database.clone();
        async move {
            let failed = |error| platform::jobs::JobFailure::Transient(error);
            let mut uow = database.for_job(&context).await.map_err(failed)?;
            let run = platform::telemetry::trace_id();
            let user = platform::types::UserId::from_uuid(job.user);
            uow.publish_to_user(&NOTED, user, &Note { label: Some(run) })
                .await
                .map_err(failed)?;
            uow.commit().await.map_err(failed)
        }
    })
    .unwrap();
    let mut stream = streams::connect(&api, &ada.access, None).await;
    stream.opened().await;

    let mut uow = api
        .pool()
        .begin(&crate::support::client(), None)
        .await
        .unwrap();
    uow.enqueue(
        platform::jobs::JobScope::for_tests(),
        &Publish {
            user: ada.user_id.parse().unwrap(),
        },
    )
    .await
    .unwrap();
    uow.commit().await.unwrap();
    let clock: platform::clock::SharedClock = api.clock();
    jobs.run_pending(api.pool(), &clock).await;
    let published = stream.event().await;

    // The job ran in a trace, whose ID its event's payload holds, and which no stream reads.
    let run = published.payload()["label"].as_str().unwrap().to_owned();
    assert_eq!(run.len(), 32);
    assert_eq!(published.data.unwrap()["trace_id"], json!(null));
}

#[tokio::test]
async fn realtime_004_a_stream_resumes_after_an_event_its_caller_may_not_receive_now() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Analytical",
        &[(&ada.user_id, "owner"), (&grace.user_id, "member")],
    )
    .await;
    let mut stream = streams::connect(&api, &grace.access, None).await;
    stream.opened().await;
    post(&api, &ada, &workspace, 1).await;
    stream.event().await;
    // Her last event is of the workspace that removes her next.
    let last = stream.last_event_id.clone();

    let removed = api
        .request(
            http::Method::DELETE,
            &format!("/v1/workspaces/{workspace}/members/{}", grace.user_id),
        )
        .bearer(&ada.access)
        .send()
        .await;
    stream.closed().await;
    announce(&api, &grace, 1).await;
    let mut again = streams::connect(&api, &grace.access, last.as_deref()).await;
    let opened = again.opened().await;
    let replayed = again.event().await;

    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    assert_eq!(opened.payload()["truncated"], false);
    assert_eq!(replayed.event.as_deref(), Some("announcement.made"));
}

/// The schema of the frame type `event` in the event catalog of `api`'s document.
fn entry<'a>(api: &'a Api, event: &str) -> &'a serde_json::Value {
    let schemas = &api.document()["components"]["schemas"];
    let found = schemas["Frame"]["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| &schemas[entry["$ref"].as_str().unwrap().rsplit('/').next().unwrap()])
        .find(|frame| frame["properties"]["event"]["enum"][0] == event);
    assert!(found.is_some(), "the catalog lists no {event}");
    found.unwrap()
}

#[tokio::test]
async fn realtime_008_the_catalog_lists_each_frame_type_with_its_scope_permission_and_payload() {
    let api = Api::new().await;
    let document = api.document();
    let stream = &document["paths"]["/v1/users/me/events"]["get"]["responses"]["200"]["content"]["text/event-stream"];
    let schemas = &document["components"]["schemas"];
    let payload = |frame: &serde_json::Value| {
        let reference = frame["properties"]["data"]["contentSchema"]["properties"]["data"]["$ref"]
            .as_str()
            .unwrap();
        schemas[reference.rsplit('/').next().unwrap()].clone()
    };

    assert_eq!(
        stream["x-itemSchema"],
        json!({ "$ref": "#/components/schemas/Frame" })
    );
    let membership = entry(&api, "membership.created");
    assert!(
        membership["description"]
            .as_str()
            .unwrap()
            .contains("a user event: it reaches its user's streams")
    );
    assert_eq!(membership["required"], json!(["event", "data", "id"]));
    assert_eq!(
        payload(membership)["required"],
        json!(["workspace_id", "session_id"])
    );
    let removed = entry(&api, "member.removed");
    assert!(
        removed["description"]
            .as_str()
            .unwrap()
            .contains("a workspace event: it reaches the streams of the workspace's members who hold `members.read`")
    );
    assert_eq!(
        payload(removed)["required"],
        json!(["workspace_id", "user_id"])
    );
    let updated = entry(&api, "workspace.updated");
    assert!(
        updated["description"]
            .as_str()
            .unwrap()
            .contains("a workspace event: it reaches the streams of the workspace's members who hold `workspace.read`")
    );
    assert_eq!(payload(updated)["required"], json!(["workspace_id"]));
    // part: invitations
    let joined = entry(&api, "member.joined");
    assert_eq!(
        payload(joined)["required"],
        json!(["workspace_id", "user_id"])
    );
    // end part: invitations
    let opened = entry(&api, "stream.opened");
    assert_eq!(opened["required"], json!(["event", "data"]));
    assert!(opened["properties"]["id"].is_object());
    assert_eq!(
        payload(opened)["required"],
        json!(["truncated", "heartbeat_seconds"])
    );
    let beat = entry(&api, "stream.beat");
    assert!(beat["properties"]["id"].is_null());
    assert_eq!(payload(beat)["type"], "object");
    assert!(payload(beat)["properties"].is_null());
    let closed = entry(&api, "stream.closed");
    assert!(closed["properties"]["id"].is_null());
    let reason = &payload(closed)["properties"]["reason"]["$ref"];
    assert_eq!(
        schemas[reason.as_str().unwrap().rsplit('/').next().unwrap()]["enum"],
        json!([
            "session_ended",
            "access_changed",
            "lagging",
            "shutdown",
            "interrupted"
        ])
    );
}

#[tokio::test]
async fn realtime_008_a_frame_whose_payload_or_type_the_catalog_doesn_t_hold_fails_the_test() {
    let api = Api::new().await;
    let contract = api.contract();
    let id = "0b5b3a59-4a3c-4bde-9b2b-7a25a5b6f0e1";
    let check = |event: &'static str, data: String| {
        let contract = contract.clone();
        std::panic::catch_unwind(move || contract.check_frame(Some(event), Some(id), Some(&data)))
    };
    let payload =
        |payload: serde_json::Value| json!({ "data": payload, "trace_id": null }).to_string();

    let matching = check(
        "membership.created",
        payload(json!({ "workspace_id": id, "session_id": id })),
    );
    let drifted = check(
        "membership.created",
        payload(json!({ "workspace_id": id, "session": id })),
    );
    let unlisted = check("membership.renamed", payload(json!({})));
    // What a comment in the stream parses to: a block with no `event`.
    let unnamed = {
        let contract = contract.clone();
        std::panic::catch_unwind(move || contract.check_frame(None, None, None))
    };

    assert!(matching.is_ok());
    assert!(drifted.is_err());
    assert!(unlisted.is_err());
    assert!(unnamed.is_err());
}

#[tokio::test]
async fn test_06_a_test_that_waits_for_a_stream_s_frame_logs_which_one() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::connect(&api, &ada.access, None).await;
    stream.opened().await;
    let opening = stream.last_event_id.clone();

    announce(&api, &ada, 1).await;
    stream.event().await;

    // What the watch prints of a test that waits for minutes: its last line is its last wait.
    let waits: Vec<serde_json::Value> = platform::telemetry::testing::all_logged()
        .into_iter()
        .filter(|line| {
            line["message"] == "the test waits for a frame of a stream"
                && line["opened_by"] == stream.trace_id()
        })
        .collect();
    let after = |line: Option<&serde_json::Value>| line.map(|line| line["after"].clone());
    assert!(opening.is_some());
    assert_eq!(after(waits.first()), Some(serde_json::Value::Null));
    assert_eq!(after(waits.last()), Some(json!(opening)));
}
