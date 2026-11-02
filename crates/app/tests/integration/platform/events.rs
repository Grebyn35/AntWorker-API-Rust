//! The real-time hub (the platform spec's "Events and the real-time hub"): who receives what, in
//! which order, and when a stream closes, through the stream itself (TEST-08).

use std::time::Duration;

use http::StatusCode;
use platform::{events::UserEvent, telemetry::testing, types::UserId};
use serde_json::json;

use crate::support::{
    Api, Options, announcements, client,
    streams::{self, Stream},
    users::{self, SignedIn},
    workspaces,
};

/// How often the hub checks its connection (PLATFORM-037).
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// How often the hub re-checks its streams, by default (PLATFORM-017).
const RECHECK: Duration = Duration::from_secs(60);

/// The event the tests publish from units of work of their own.
#[expect(
    clippy::disallowed_methods,
    reason = "a test declares the event it publishes from its own units of work"
)]
const NOTED: UserEvent = UserEvent::new("note.made");

async fn api() -> Api {
    Api::with(Options::routes(announcements::routes())).await
}

/// Publishes `count` events to the members of `workspace` as `user`, to every member, or only to
/// those who may remove members when `restricted`.
async fn post(api: &Api, user: &SignedIn, workspace: &str, count: u32, restricted: bool) {
    let response = api
        .post(
            &format!("/v1/workspaces/{workspace}/announcements"),
            &json!({ "count": count, "restricted": restricted }),
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

/// Publishes `count` events to `user`.
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

/// The type of each of the next `count` events of `stream`.
async fn types(stream: &mut Stream, count: usize) -> Vec<String> {
    let mut types = Vec::new();
    for _ in 0..count {
        types.push(stream.event().await.event.unwrap());
    }
    types
}

/// A stream of `user`'s, open and past its `stream.opened`.
async fn opened(api: &Api, user: &SignedIn) -> Stream {
    let mut stream = streams::connect(api, &user.access, None).await;
    stream.opened().await;
    stream
}

#[tokio::test]
async fn platform_016_a_workspace_event_reaches_only_the_members_whose_role_holds_its_permission() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let alan = users::register(&api, "alan@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Analytical",
        &[(&ada.user_id, "admin"), (&grace.user_id, "member")],
    )
    .await;
    let mut admin = opened(&api, &ada).await;
    let mut member = opened(&api, &grace).await;
    let mut outsider = opened(&api, &alan).await;

    post(&api, &ada, &workspace, 1, true).await;
    post(&api, &ada, &workspace, 1, false).await;
    // Each stream's last event is its own, so what it reads before shows what reached it.
    for user in [&ada, &grace, &alan] {
        announce(&api, user, 1).await;
    }

    assert_eq!(
        types(&mut admin, 3).await,
        [
            "announcement.restricted",
            "announcement.posted",
            "announcement.made"
        ]
    );
    assert_eq!(
        types(&mut member, 2).await,
        ["announcement.posted", "announcement.made"]
    );
    assert_eq!(types(&mut outsider, 1).await, ["announcement.made"]);
}

#[tokio::test]
async fn platform_016_a_replay_leaves_out_a_workspace_s_events_from_before_its_member_joined() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "owner")]).await;
    let mut first = opened(&api, &grace).await;
    announce(&api, &grace, 1).await;
    first.event().await;
    let last = first.last_event_id.clone();
    drop(first);

    post(&api, &ada, &workspace, 1, false).await;
    workspaces::join(api.database(), &workspace, &grace.user_id, "member").await;
    post(&api, &ada, &workspace, 2, false).await;
    let mut again = streams::connect(&api, &grace.access, last.as_deref()).await;
    again.opened().await;
    let replayed = [again.event().await, again.event().await];
    announce(&api, &grace, 1).await;
    let next = again.event().await;

    let numbers: Vec<_> = replayed
        .iter()
        .map(|frame| frame.payload()["n"].clone())
        .collect();
    assert_eq!(numbers, [json!(1), json!(2)]);
    assert_eq!(next.event.as_deref(), Some("announcement.made"));
}

#[tokio::test]
async fn platform_057_each_event_is_loaded_once_whatever_the_streams_it_reaches() {
    let api = api().await;
    let users = [
        users::register(&api, "ada@example.com").await,
        users::register(&api, "grace@example.com").await,
        users::register(&api, "alan@example.com").await,
    ];
    let members: Vec<(&str, &str)> = users
        .iter()
        .map(|user| (user.user_id.as_str(), "member"))
        .collect();
    let workspace = workspaces::arrange(api.database(), "Analytical", &members).await;
    let mut open = Vec::new();
    for user in &users {
        open.push(opened(&api, user).await);
        open.push(opened(&api, user).await);
    }
    let loaded = || {
        testing::recorded("platform.event_delivery_lag")
            .into_iter()
            .find(|point| {
                point.labels.get("db.namespace") == Some(&api.database().name().to_owned())
            })
            .and_then(|point| point.count)
            .unwrap_or_default()
    };
    let before = loaded();

    post(&api, &users[0], &workspace, 2, false).await;
    for stream in &mut open {
        types(stream, 2).await;
    }

    assert_eq!(loaded() - before, 2);
}

#[tokio::test]
async fn platform_071_events_reach_a_stream_and_its_replay_in_the_order_their_units_of_work_commit()
{
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let user = UserId::from_uuid(ada.user_id.parse().unwrap());
    let mut stream = opened(&api, &ada).await;
    announce(&api, &ada, 1).await;
    stream.event().await;
    let before = stream.last_event_id.clone();

    // The first unit of work publishes first and commits last.
    let mut first = api.pool().begin(&client()).await.unwrap();
    first
        .publish_to_user(&NOTED, user, &json!({ "unit": "first" }))
        .await
        .unwrap();
    let mut second = api.pool().begin(&client()).await.unwrap();
    second
        .publish_to_user(&NOTED, user, &json!({ "unit": "second" }))
        .await
        .unwrap();
    second.commit().await.unwrap();
    first.commit().await.unwrap();
    let live = [stream.event().await, stream.event().await];
    let mut resumed = streams::connect(&api, &ada.access, before.as_deref()).await;
    resumed.opened().await;
    let replayed = [resumed.event().await, resumed.event().await];
    // A resume from the second, which committed first, still gets the first.
    let mut after_second = streams::connect(&api, &ada.access, live[0].id.as_deref()).await;
    after_second.opened().await;
    let rest = after_second.event().await;

    for frames in [&live, &replayed] {
        let units: Vec<_> = frames
            .iter()
            .map(|frame| frame.payload()["unit"].clone())
            .collect();
        assert_eq!(units, [json!("second"), json!("first")]);
    }
    assert_eq!(rest.payload()["unit"], "first");
}

#[tokio::test]
async fn platform_017_the_recheck_closes_a_stream_whose_membership_changed_without_a_signal() {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "member")]).await;
    let mut changed = opened(&api, &ada).await;
    let mut unchanged = opened(&api, &grace).await;

    // Changed behind the API's back, so no signal tells the hub.
    api.database()
        .owner_executes(&format!(
            "DELETE FROM workspace_memberships WHERE workspace_id = '{workspace}'"
        ))
        .await;
    clock.sleeping(RECHECK).await;
    clock.advance(RECHECK);
    let (_, reason) = changed.closed().await;
    clock.sleeping(RECHECK).await;
    announce(&api, &grace, 1).await;
    let still = unchanged.event().await;

    assert_eq!(reason, "access_changed");
    assert_eq!(still.event.as_deref(), Some("announcement.made"));
}

#[tokio::test]
async fn workspaces_005_removing_a_member_closes_their_stream_before_the_next_event() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Analytical",
        &[(&ada.user_id, "admin"), (&grace.user_id, "member")],
    )
    .await;
    let mut admin = opened(&api, &ada).await;
    let mut removed = opened(&api, &grace).await;

    let response = api
        .request(
            http::Method::DELETE,
            &format!("/v1/workspaces/{workspace}/members/{}", grace.user_id),
        )
        .bearer(&ada.access)
        .send()
        .await;
    post(&api, &ada, &workspace, 1, false).await;
    let (before, reason) = removed.closed().await;

    assert_eq!(response.status, StatusCode::NO_CONTENT);
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(reason, "access_changed");
    assert_eq!(
        types(&mut admin, 2).await,
        ["member.removed", "announcement.posted"]
    );
}

#[tokio::test]
async fn access_006_a_membership_that_starts_closes_its_user_s_streams() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let mut creator = opened(&api, &ada).await;
    let mut other = opened(&api, &grace).await;

    let response = api
        .post("/v1/workspaces", &json!({ "name": "Analytical" }))
        .bearer(&ada.access)
        .send()
        .await;
    let (_, reason) = creator.closed().await;
    announce(&api, &grace, 1).await;
    let still = other.event().await;

    assert_eq!(response.status, StatusCode::CREATED);
    assert_eq!(reason, "access_changed");
    assert_eq!(still.event.as_deref(), Some("announcement.made"));
}

#[tokio::test]
async fn access_007_a_stream_holds_each_workspace_s_permissions_of_its_role_there() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let owned = workspaces::arrange(
        api.database(),
        "Owned",
        &[(&ada.user_id, "owner"), (&grace.user_id, "admin")],
    )
    .await;
    let joined = workspaces::arrange(
        api.database(),
        "Joined",
        &[(&grace.user_id, "owner"), (&ada.user_id, "member")],
    )
    .await;
    let mut stream = opened(&api, &ada).await;

    post(&api, &grace, &joined, 1, true).await;
    post(&api, &grace, &owned, 1, true).await;
    post(&api, &grace, &joined, 1, false).await;

    let frames = [stream.event().await, stream.event().await];
    let reached: Vec<_> = frames
        .iter()
        .map(|frame| {
            (
                frame.event.clone().unwrap(),
                frame.payload()["workspace_id"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        reached,
        [
            ("announcement.restricted".to_owned(), owned),
            ("announcement.posted".to_owned(), joined),
        ]
    );
}

#[tokio::test]
async fn auth_018_a_stream_outlives_its_access_token_but_not_its_session() {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let mut ending = opened(&api, &ada).await;
    let mut lasting = opened(&api, &grace).await;

    api.database()
        .owner_executes(&format!(
            "UPDATE sessions SET access_token_expires_at = now() - interval '1 second' \
             WHERE user_id IN ('{}', '{}'); \
             UPDATE sessions SET refresh_token_expires_at = now() - interval '1 second' \
             WHERE user_id = '{}'",
            ada.user_id, grace.user_id, ada.user_id
        ))
        .await;
    clock.sleeping(RECHECK).await;
    clock.advance(RECHECK);
    let (_, reason) = ending.closed().await;
    clock.sleeping(RECHECK).await;
    announce(
        &api,
        &users::new_session(&api, "grace@example.com").await,
        1,
    )
    .await;
    let still = lasting.event().await;

    assert_eq!(reason, "session_ended");
    assert_eq!(still.event.as_deref(), Some("announcement.made"));
}

#[tokio::test]
async fn platform_072_a_stream_whose_client_reads_too_slowly_closes_with_lagging() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = opened(&api, &ada).await;

    // Nothing reads the stream while 300 events arrive at once.
    announce(&api, &ada, 300).await;
    let (before, reason) = stream.closed().await;

    assert_eq!(reason, "lagging");
    assert_eq!(before.len(), 256);
    assert_eq!(before[255].payload()["n"], 256);
}

#[tokio::test]
async fn platform_073_after_a_lost_connection_the_hub_delivers_what_committed_meanwhile() {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = opened(&api, &ada).await;
    clock.sleeping(CHECK_INTERVAL).await;

    api.database().cut_off_app().await;
    // The next check fails, and the hub waits for the one after.
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_INTERVAL).await;
    let unavailable = api.get("/health/ready").send().await;
    // Committed while no connection of the hub's listens: its notification is lost.
    api.database()
        .owner_executes(&format!(
            "INSERT INTO events (scope, user_id, type, payload, position, created_at) VALUES \
             ('user', '{}', 'note.made', '{{\"while\": \"away\"}}', \
             nextval('events_position_seq'), clock_timestamp())",
            ada.user_id
        ))
        .await;
    api.database()
        .admin_executes(&format!(
            "GRANT CONNECT ON DATABASE \"{}\" TO app",
            api.database().name()
        ))
        .await;
    clock.advance(CHECK_INTERVAL);
    let caught_up = stream.event().await;
    let ready = api.get("/health/ready").send().await;

    assert_eq!(
        unavailable.json(),
        json!({ "status": "database_unavailable" })
    );
    assert_eq!(caught_up.payload(), &json!({ "while": "away" }));
    assert_eq!(ready.status, StatusCode::OK);
}

#[tokio::test]
async fn platform_073_after_a_lost_connection_a_stream_that_lost_access_meanwhile_closes_first() {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "member")]).await;
    let mut stream = opened(&api, &ada).await;
    clock.sleeping(CHECK_INTERVAL).await;

    api.database().cut_off_app().await;
    api.database()
        .owner_executes(&format!(
            "DELETE FROM workspace_memberships WHERE workspace_id = '{workspace}'; \
             INSERT INTO events (scope, workspace_id, type, permission, payload, position, \
             created_at) VALUES ('workspace', '{workspace}', 'note.made', 'workspace.read', \
             '{{}}', nextval('events_position_seq'), clock_timestamp())"
        ))
        .await;
    api.database()
        .admin_executes(&format!(
            "GRANT CONNECT ON DATABASE \"{}\" TO app",
            api.database().name()
        ))
        .await;
    clock.advance(CHECK_INTERVAL);
    let (before, reason) = stream.closed().await;

    assert!(before.is_empty(), "{before:?}");
    assert_eq!(reason, "access_changed");
}

#[tokio::test]
async fn platform_074_the_hub_records_its_streams_why_it_closed_them_and_its_lag() {
    let api = api().await;
    let name = api.database().name().to_owned();
    let ours = |point: &testing::Point| point.labels.get("db.namespace") == Some(&name);
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = opened(&api, &ada).await;
    let open = testing::recorded("platform.streams")
        .into_iter()
        .find(|point| ours(point))
        .map(|point| point.value);

    announce(&api, &ada, 1).await;
    stream.event().await;
    api.readiness().stop();
    stream.closed().await;

    let closed = testing::recorded("platform.streams")
        .into_iter()
        .find(|point| ours(point))
        .map(|point| point.value);
    let closes: Vec<_> = testing::recorded("platform.stream_closes")
        .into_iter()
        .filter(|point| ours(point))
        .map(|point| (point.labels["stream.close_reason"].clone(), point.value))
        .collect();
    let lag = testing::recorded("platform.event_delivery_lag")
        .into_iter()
        .find(|point| ours(point))
        .and_then(|point| point.count);
    assert_eq!(open, Some(1.0));
    assert_eq!(closed, Some(0.0));
    assert_eq!(closes, [("shutdown".to_owned(), 1.0)]);
    assert_eq!(lag, Some(1));
}

#[tokio::test]
async fn platform_064_the_sweep_deletes_the_events_older_than_24_hours() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    announce(&api, &ada, 1).await;
    api.database()
        .owner_executes(&format!(
            "INSERT INTO events (scope, user_id, type, payload, position, created_at) \
             SELECT 'user', '{}', 'note.made', '{{}}', nextval('events_position_seq'), \
             now() - interval '25 hours' FROM generate_series(1, 2500)",
            ada.user_id
        ))
        .await;

    for _ in 0..2 {
        api.run_periodic("platform.expire_events").await;
    }

    let left = api.database().owner_reads("SELECT type FROM events").await;
    assert_eq!(left, ["announcement.made"]);
}
