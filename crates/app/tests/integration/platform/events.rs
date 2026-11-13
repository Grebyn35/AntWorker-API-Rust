//! The real-time hub (the platform spec's "Events and the real-time hub"): who receives what, in
//! which order, and when a stream closes, through the stream itself (TEST-07).

use std::time::Duration;

use http::StatusCode;
use platform::{telemetry::testing, types::UserId};
use serde_json::json;

use crate::support::{
    Api, Options, Proxy,
    announcements::{self, Note, published::NOTED},
    client,
    events::{self, Event},
    streams::{self, Stream},
    users::{self, SignedIn},
    workspaces,
};

/// How often the hub checks the listener (PLATFORM-061).
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// How often the hub re-checks its streams, by default (PLATFORM-031).
const RECHECK: Duration = Duration::from_secs(60);

/// A note labelled `label`.
fn note(label: &str) -> Note {
    Note {
        label: Some(label.to_owned()),
    }
}

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

/// The name of each of the next `count` events of `stream`.
async fn names(stream: &mut Stream, count: usize) -> Vec<String> {
    let mut names = Vec::new();
    for _ in 0..count {
        names.push(stream.event().await.event.unwrap());
    }
    names
}

/// The next `count` events of `stream`.
async fn frames(stream: &mut Stream, count: usize) -> Vec<streams::Frame> {
    let mut frames = Vec::new();
    for _ in 0..count {
        frames.push(stream.event().await);
    }
    frames
}

#[tokio::test]
async fn platform_029_a_workspace_event_reaches_only_the_members_whose_role_holds_its_permission() {
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
    let mut admin = streams::opened(&api, &ada).await;
    let mut member = streams::opened(&api, &grace).await;
    let mut outsider = streams::opened(&api, &alan).await;

    post(&api, &ada, &workspace, 1, true).await;
    post(&api, &ada, &workspace, 1, false).await;
    // Each user's own events come last, numbered from 1, so another's would show as a number
    // out of its place.
    for (user, count) in [(&ada, 1), (&grace, 2), (&alan, 3)] {
        announce(&api, user, count).await;
    }

    let numbered = |frames: Vec<streams::Frame>| -> Vec<(String, u64)> {
        frames
            .into_iter()
            .map(|frame| {
                let n = frame.payload()["n"].as_u64().unwrap();
                (frame.event.unwrap(), n)
            })
            .collect()
    };
    let made = |n| ("announcement.made".to_owned(), n);
    assert_eq!(
        numbered(frames(&mut admin, 3).await),
        [
            ("announcement.restricted".to_owned(), 1),
            ("announcement.posted".to_owned(), 1),
            made(1),
        ]
    );
    assert_eq!(
        numbered(frames(&mut member, 3).await),
        [("announcement.posted".to_owned(), 1), made(1), made(2)]
    );
    assert_eq!(
        numbered(frames(&mut outsider, 3).await),
        [made(1), made(2), made(3)]
    );
}

#[tokio::test]
async fn platform_029_a_replay_leaves_out_a_workspace_s_events_from_before_its_member_joined() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "owner")]).await;
    let mut first = streams::opened(&api, &grace).await;
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
async fn platform_027_each_event_is_loaded_once_whatever_the_streams_it_reaches() {
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
        open.push(streams::opened(&api, user).await);
        open.push(streams::opened(&api, user).await);
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
    let mut orders = Vec::new();
    for stream in &mut open {
        let frames = [stream.event().await, stream.event().await];
        orders.push(frames.map(|frame| frame.payload()["n"].as_u64().unwrap()));
    }

    assert_eq!(loaded() - before, 2);
    // Every stream received both, in the order they were published and committed.
    assert!(orders.iter().all(|order| *order == [1, 2]), "{orders:?}");
}

#[tokio::test]
async fn platform_031_a_signal_closes_its_streams_before_any_event_that_commits_after_it() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let other = users::new_session(&api, "ada@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "owner")]).await;
    let mut ended = streams::opened(&api, &ada).await;

    let signed_out = api
        .request(http::Method::DELETE, "/v1/users/me/sessions/current")
        .bearer(&ada.access)
        .send()
        .await;
    post(&api, &other, &workspace, 1, false).await;
    announce(&api, &other, 1).await;
    let (before, reason) = ended.closed().await;

    assert_eq!(signed_out.status, StatusCode::NO_CONTENT);
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(reason, "session_ended");
}

#[tokio::test]
async fn platform_028_a_stream_resumes_on_another_replica_with_every_event_after_its_last() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    // A second replica: another application, with a hub of its own, on the same database.
    let (second, _, _) = crate::support::api::application(
        api.pool(),
        Options::routes(announcements::routes()),
        api.clock(),
        platform::lifecycle::Readiness::new(),
    )
    .await;
    let mut first = streams::opened(&api, &ada).await;
    announce(&api, &ada, 1).await;
    first.event().await;
    let last = first.last_event_id.clone();
    drop(first);

    announce(&api, &ada, 2).await;
    let mut resumed = streams::connect_on(&api, second, &ada.access, last.as_deref()).await;
    resumed.opened().await;
    let replayed = [resumed.event().await, resumed.event().await];
    announce(&api, &ada, 1).await;
    let live = resumed.event().await;

    let numbers: Vec<_> = replayed
        .iter()
        .map(|frame| frame.payload()["n"].clone())
        .collect();
    assert_eq!(numbers, [json!(1), json!(2)]);
    assert_eq!(live.payload()["n"], 1);
}

#[tokio::test]
async fn platform_028_events_reach_a_stream_and_its_replay_in_the_order_their_units_of_work_commit()
{
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let user = UserId::from_uuid(ada.user_id.parse().unwrap());
    let mut stream = streams::opened(&api, &ada).await;
    announce(&api, &ada, 1).await;
    stream.event().await;
    let before = stream.last_event_id.clone();

    // The first unit of work publishes first and commits last.
    let mut first = api.pool().begin(&client()).await.unwrap();
    first
        .publish_to_user(&NOTED, user, &note("first"))
        .await
        .unwrap();
    let mut second = api.pool().begin(&client()).await.unwrap();
    second
        .publish_to_user(&NOTED, user, &note("second"))
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
            .map(|frame| frame.payload()["label"].clone())
            .collect();
        assert_eq!(units, [json!("second"), json!("first")]);
    }
    assert_eq!(rest.payload()["label"], "first");
}

#[tokio::test]
async fn platform_031_the_recheck_closes_a_stream_whose_membership_changed_without_a_signal() {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "member")]).await;
    let mut changed = streams::opened(&api, &ada).await;
    let mut unchanged = streams::opened(&api, &grace).await;

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
    let mut admin = streams::opened(&api, &ada).await;
    let mut removed = streams::opened(&api, &grace).await;

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
        names(&mut admin, 2).await,
        ["member.removed", "announcement.posted"]
    );
}

#[tokio::test]
async fn access_006_a_membership_that_starts_closes_its_user_s_streams() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let mut creator = streams::opened(&api, &ada).await;
    let mut other = streams::opened(&api, &grace).await;

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
    let mut stream = streams::opened(&api, &ada).await;

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
    let mut ending = streams::opened(&api, &ada).await;
    let mut lasting = streams::opened(&api, &grace).await;

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
    let again = users::new_session(&api, "grace@example.com").await;
    announce(&api, &again, 1).await;
    // Grace's new session's start reaches her lasting stream first (AUTH-046).
    let started = lasting.event().await;
    let still = lasting.event().await;

    assert_eq!(reason, "session_ended");
    assert_eq!(started.event.as_deref(), Some("session.created"));
    assert_eq!(still.event.as_deref(), Some("announcement.made"));
}

#[tokio::test]
async fn platform_032_a_stream_whose_client_reads_too_slowly_closes_with_lagging() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;

    // Nothing reads the stream while 300 events arrive at once.
    announce(&api, &ada, 300).await;
    let (before, reason) = stream.closed().await;

    assert_eq!(reason, "lagging");
    assert_eq!(before.len(), 256);
    assert_eq!(before[255].payload()["n"], 256);
}

/// Cuts the API's role off the database, so the listener is lost and its next check
/// fails; returns readiness's answer then; and lets the role in again after `meanwhile`, which
/// runs as owner, so the hub connects again at its next check.
async fn lose_the_connection(api: &Api, meanwhile: &str) -> crate::support::api::Response {
    let clock = api.clock();
    api.database().cut_off_app().await;
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_INTERVAL).await;
    let unavailable = api.get("/health/ready").send().await;
    api.database().owner_executes(meanwhile).await;
    api.database()
        .admin_executes(&format!(
            "GRANT CONNECT ON DATABASE \"{}\" TO app",
            api.database().name()
        ))
        .await;
    clock.advance(CHECK_INTERVAL);
    unavailable
}

#[tokio::test]
async fn platform_033_after_a_lost_connection_every_stream_closes_and_replays_what_committed_meanwhile()
 {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;
    announce(&api, &ada, 1).await;
    stream.event().await;
    let last = stream.last_event_id.clone();
    clock.sleeping(CHECK_INTERVAL).await;

    let unavailable = lose_the_connection(
        &api,
        &events::statement(
            &[Event::to_user(
                &ada.user_id,
                "note.made",
                r#"{"label": "away"}"#,
            )],
            false,
        ),
    )
    .await;
    let (before, reason) = stream.closed().await;
    let ready = api.get("/health/ready").send().await;
    let mut again = streams::connect(&api, &ada.access, last.as_deref()).await;
    again.opened().await;
    let replayed = again.event().await;

    assert_eq!(
        unavailable.json(),
        json!({ "status": "database_unavailable" })
    );
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(reason, "interrupted");
    assert_eq!(ready.status, StatusCode::OK);
    assert_eq!(replayed.payload(), &json!({ "label": "away" }));
}

#[tokio::test]
async fn platform_033_after_a_lost_connection_a_reconnect_leaves_out_what_access_lost_meanwhile_withholds()
 {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "member")]).await;
    let mut stream = streams::opened(&api, &ada).await;
    announce(&api, &ada, 1).await;
    stream.event().await;
    let last = stream.last_event_id.clone();
    clock.sleeping(CHECK_INTERVAL).await;

    // While no connection listens, an event of her workspace commits, then her removal, whose
    // signal is lost with the notifications, then another event of the workspace.
    lose_the_connection(
        &api,
        &format!(
            "{}; DELETE FROM workspace_memberships WHERE workspace_id = '{workspace}'; {}",
            events::statement(
                &[Event::to_workspace(
                    &workspace,
                    "workspace.read",
                    "note.made",
                    "{}"
                )],
                false
            ),
            events::statement(
                &[Event::to_workspace(
                    &workspace,
                    "workspace.read",
                    "note.made",
                    "{}"
                )],
                false
            ),
        ),
    )
    .await;
    let (before, reason) = stream.closed().await;
    let mut again = streams::connect(&api, &ada.access, last.as_deref()).await;
    again.opened().await;
    announce(&api, &ada, 1).await;
    let next = again.event().await;

    assert!(before.is_empty(), "{before:?}");
    assert_eq!(reason, "interrupted");
    // Her replay holds neither event of the workspace: she isn't its member now.
    assert_eq!(next.event.as_deref(), Some("announcement.made"));
}

#[tokio::test]
async fn platform_033_a_re_check_that_fails_is_tried_again_at_the_next_check() {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "member")]).await;
    let mut stream = streams::opened(&api, &ada).await;
    api.database()
        .owner_executes(&format!(
            "DELETE FROM workspace_memberships WHERE workspace_id = '{workspace}'"
        ))
        .await;

    // Every pooled connection is held, so the re-check waits out the pool and fails.
    let mut held = Vec::new();
    for _ in 0..crate::support::TestDatabase::POOL_SIZE {
        held.push(api.pool().begin(&client()).await.unwrap());
    }
    clock.sleeping(RECHECK).await;
    clock.advance(RECHECK);
    while !logged("the hub couldn't re-check its streams") {
        tokio::task::yield_now().await;
    }
    drop(held);
    clock.advance(CHECK_INTERVAL);
    let (_, reason) = stream.closed().await;

    assert_eq!(reason, "access_changed");
}

/// How long the listener's check waits for the database.
const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// The instance's connections to its database, as `pg_stat_activity` lists them.
async fn connections(api: &Api) -> usize {
    api.database()
        .admin_reads(&format!(
            "SELECT pid::text FROM pg_stat_activity WHERE datname = '{}' AND usename = 'app'",
            api.database().name()
        ))
        .await
        .len()
}

#[tokio::test]
async fn ops_18_checks_that_time_out_open_no_connection_beyond_the_pool_and_the_listener() {
    let database = crate::support::TestDatabase::migrated().await;
    let proxy = Proxy::to(database.app_url().address()).await;
    let api = Api::through(database, &proxy, Options::default()).await;
    let clock = api.clock();
    let mut held = Vec::new();
    for _ in 0..crate::support::TestDatabase::POOL_SIZE {
        held.push(api.pool().begin(&client()).await.unwrap());
    }
    clock.sleeping(CHECK_INTERVAL).await;
    let budget = connections(&api).await;

    // The database stops answering on every connection the instance holds, but takes new ones.
    // The first check waits out its timeout on the listener; it and the next each try to open
    // another, while the one they dropped still waits for the database.
    proxy.freeze();
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_TIMEOUT).await;
    clock.advance(CHECK_TIMEOUT);
    clock.sleeping(CHECK_INTERVAL).await;
    let after_one = connections(&api).await;
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_INTERVAL).await;
    let after_two = connections(&api).await;
    let ready = api.get("/health/ready").send().await;
    proxy.thaw();
    drop(held);

    assert_eq!(
        budget,
        usize::try_from(api.pool().connections()).unwrap(),
        "every pooled connection and the listener"
    );
    assert_eq!((after_one, after_two), (budget, budget));
    assert_eq!(ready.json(), json!({ "status": "database_unavailable" }));
}

#[tokio::test]
async fn platform_034_the_hub_records_its_streams_why_it_closed_them_and_its_lag() {
    let api = api().await;
    let name = api.database().name().to_owned();
    let ours = |point: &testing::Point| point.labels.get("db.namespace") == Some(&name);
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;
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
    // The hub loaded two events: the start of Ada's session (AUTH-046), and the announcement.
    assert_eq!(lag, Some(2));
}

#[tokio::test]
async fn platform_019_the_sweep_deletes_the_events_older_than_24_hours() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    announce(&api, &ada, 1).await;
    api.database()
        .owner_executes(&format!(
            "INSERT INTO events (scope, user_id, name, payload, position, created_at) \
             SELECT 'user', '{}', 'note.made', '{{}}', nextval('events_position_seq'), \
             now() - interval '25 hours' FROM generate_series(1, 2500)",
            ada.user_id
        ))
        .await;

    for _ in 0..2 {
        api.run_periodic("platform.expire_events").await;
    }

    let left = api
        .database()
        .owner_reads("SELECT name FROM events ORDER BY name")
        .await;
    // The recent events stay: the announcement, and the start of Ada's session (AUTH-046).
    assert_eq!(left, ["announcement.made", "session.created"]);
}

#[tokio::test]
async fn access_006_removing_a_membership_closes_its_user_s_streams() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Analytical",
        &[(&ada.user_id, "owner"), (&grace.user_id, "member")],
    )
    .await;
    let mut removed = streams::opened(&api, &grace).await;

    let response = api
        .request(
            http::Method::DELETE,
            &format!("/v1/workspaces/{workspace}/members/{}", grace.user_id),
        )
        .bearer(&ada.access)
        .send()
        .await;
    let (_, reason) = removed.closed().await;

    assert_eq!(response.status, StatusCode::NO_CONTENT);
    assert_eq!(reason, "access_changed");
}

#[tokio::test]
async fn auth_018_the_recheck_closes_a_stream_whose_session_outlived_its_lifetime_or_is_gone() {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let mut expired = streams::opened(&api, &ada).await;
    let mut deleted = streams::opened(&api, &grace).await;

    // Past its absolute lifetime, and its tokens' expiries with it; and gone, with no signal to
    // tell the hub.
    api.database()
        .owner_executes(&format!(
            "UPDATE sessions SET access_token_expires_at = now() - interval '1 second', \
             refresh_token_expires_at = now() - interval '1 second', \
             expires_at = now() - interval '1 second' WHERE user_id = '{}'; \
             DELETE FROM sessions WHERE user_id = '{}'",
            ada.user_id, grace.user_id
        ))
        .await;
    clock.sleeping(RECHECK).await;
    clock.advance(RECHECK);
    let (_, expired_reason) = expired.closed().await;
    let (_, deleted_reason) = deleted.closed().await;

    assert_eq!(expired_reason, "session_ended");
    assert_eq!(deleted_reason, "session_ended");
}

#[tokio::test]
async fn platform_031_the_recheck_interval_is_a_setting_of_10_to_3600_seconds() {
    for seconds in [9, 3_601] {
        let refused = crate::support::api::refusal(
            &Options::default().set("STREAM_RECHECK_SECONDS", seconds),
        );
        assert!(refused.contains("STREAM_RECHECK_SECONDS"), "{refused}");
        assert!(refused.contains("10 to 3600"), "{refused}");
    }
    let every = Duration::from_secs(10);
    let api = Api::with(
        Options::routes(announcements::routes()).set("STREAM_RECHECK_SECONDS", every.as_secs()),
    )
    .await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "member")]).await;
    let mut stream = streams::opened(&api, &ada).await;

    api.database()
        .owner_executes(&format!(
            "DELETE FROM workspace_memberships WHERE workspace_id = '{workspace}'"
        ))
        .await;
    clock.sleeping(every).await;
    clock.advance(every);
    let (_, reason) = stream.closed().await;

    assert_eq!(reason, "access_changed");
}

#[tokio::test]
async fn sec_19_member_removed_reaches_every_member_and_no_replay_from_before_joining() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let alan = users::register(&api, "alan@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Analytical",
        &[
            (&ada.user_id, "owner"),
            (&grace.user_id, "member"),
            (&bob.user_id, "member"),
        ],
    )
    .await;
    // Every role holds members.read, which member.removed requires: no member lacks it.
    let mut owner = streams::opened(&api, &ada).await;
    let mut member = streams::opened(&api, &grace).await;
    let mut later = streams::opened(&api, &alan).await;
    announce(&api, &alan, 1).await;
    later.event().await;
    let before = later.last_event_id.clone();
    drop(later);

    let removed = api
        .request(
            http::Method::DELETE,
            &format!("/v1/workspaces/{workspace}/members/{}", bob.user_id),
        )
        .bearer(&ada.access)
        .send()
        .await;
    workspaces::join(api.database(), &workspace, &alan.user_id, "member").await;
    let mut joined = streams::connect(&api, &alan.access, before.as_deref()).await;
    joined.opened().await;
    announce(&api, &alan, 1).await;
    let after_joining = joined.event().await;

    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    assert_eq!(names(&mut owner, 1).await, ["member.removed"]);
    assert_eq!(names(&mut member, 1).await, ["member.removed"]);
    // The removal from before Alan joined isn't replayed to him.
    assert_eq!(after_joining.event.as_deref(), Some("announcement.made"));
}

#[tokio::test]
async fn obs_07_an_event_s_delivery_links_to_the_trace_that_published_it() {
    let collector = testing::collector::Collector::start().await.unwrap();
    let database = crate::support::TestDatabase::migrated().await;
    let trace = "4bf92f3577b34da6a3ce929d0e0e4736";
    let _server = database
        .serve(&[
            ("ENVIRONMENT", "production"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", &collector.endpoint()),
        ])
        .await;
    let user = crate::support::users::without_password(&database, "u@example.com").await;

    // An event committed and notified as a unit of work's commit does it, with the trace context
    // of what published it.
    database
        .owner_executes(&events::statement(
            &[Event {
                traceparent: Some(&format!("00-{trace}-00f067aa0ba902b7-01")),
                ..Event::to_user(&user, "note.made", "{}")
            }],
            true,
        ))
        .await;
    let received = collector
        .wait_for(|received| {
            received
                .spans()
                .iter()
                .any(|(_, span)| span.name == "deliver event")
        })
        .await;

    let (_, span) = received
        .spans()
        .into_iter()
        .find(|(_, span)| span.name == "deliver event")
        .unwrap();
    assert_eq!(span.links.len(), 1);
    let hex = |bytes: &[u8]| -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() };
    assert_eq!(hex(&span.links[0].trace_id), trace);
    assert_eq!(hex(&span.links[0].span_id), "00f067aa0ba902b7");
    assert_ne!(hex(&span.trace_id), trace);
}

#[tokio::test]
async fn platform_029_an_event_that_commits_while_its_member_joins_reaches_them_not() {
    let api = api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let alan = users::register(&api, "alan@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "owner")]).await;
    let mut first = streams::opened(&api, &alan).await;
    announce(&api, &alan, 1).await;
    first.event().await;
    let before = first.last_event_id.clone();
    drop(first);

    // Alan's membership is added in a transaction that is still open when the event commits.
    let mut joining = api.database().owner_connection().await;
    crate::support::execute_on(&mut joining, "BEGIN").await;
    crate::support::execute_on(
        &mut joining,
        &format!(
            "INSERT INTO workspace_memberships (workspace_id, user_id, role) \
             VALUES ('{workspace}', '{}', 'member')",
            alan.user_id
        ),
    )
    .await;
    post(&api, &ada, &workspace, 1, false).await;
    crate::support::execute_on(&mut joining, "COMMIT").await;
    post(&api, &ada, &workspace, 2, false).await;
    let mut again = streams::connect(&api, &alan.access, before.as_deref()).await;
    again.opened().await;
    let replayed = [again.event().await, again.event().await];

    let numbers: Vec<_> = replayed
        .iter()
        .map(|frame| frame.payload()["n"].clone())
        .collect();
    // The second post's two events, not the first's, which committed before Alan was a member.
    assert_eq!(numbers, [json!(1), json!(2)]);
    announce(&api, &alan, 1).await;
    assert_eq!(
        again.event().await.event.as_deref(),
        Some("announcement.made")
    );
}

#[tokio::test]
async fn platform_033_a_load_that_fails_is_tried_again_at_the_next_check() {
    let api = api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;
    clock.sleeping(CHECK_INTERVAL).await;

    // Every pooled connection is held, so the hub's load waits out the pool and fails.
    let mut held = Vec::new();
    for _ in 0..crate::support::TestDatabase::POOL_SIZE {
        held.push(api.pool().begin(&client()).await.unwrap());
    }
    api.database()
        .owner_executes(&events::statement(
            &[Event::to_user(
                &ada.user_id,
                "note.made",
                r#"{"label": "loaded"}"#,
            )],
            true,
        ))
        .await;
    while !logged("the hub couldn't load events") {
        tokio::task::yield_now().await;
    }
    drop(held);
    clock.advance(CHECK_INTERVAL);
    let delivered = stream.event().await;

    assert_eq!(delivered.payload(), &json!({ "label": "loaded" }));
}

#[tokio::test]
async fn platform_035_the_hub_records_the_events_it_owes_while_its_loads_fail() {
    let api = api().await;
    let clock = api.clock();
    let name = api.database().name().to_owned();
    let unloaded = || {
        testing::recorded("platform.events_unloaded")
            .into_iter()
            .find(|point| point.labels.get("db.namespace") == Some(&name))
            .map(|point| point.value)
    };
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;
    clock.sleeping(CHECK_INTERVAL).await;

    // Every pooled connection is held, so the hub's load waits out the pool and fails.
    let mut held = Vec::new();
    for _ in 0..crate::support::TestDatabase::POOL_SIZE {
        held.push(api.pool().begin(&client()).await.unwrap());
    }
    api.database()
        .owner_executes(&events::statement(
            &[Event::to_user(&ada.user_id, "note.made", "{}")],
            true,
        ))
        .await;
    while unloaded() != Some(1.0) {
        tokio::task::yield_now().await;
    }
    let owed = unloaded();
    drop(held);
    clock.advance(CHECK_INTERVAL);
    stream.event().await;
    let settled = unloaded();

    assert_eq!(owed, Some(1.0));
    assert_eq!(settled, Some(0.0));
}

/// Whether a hub has logged `message`.
fn logged(message: &str) -> bool {
    testing::all_logged()
        .iter()
        .any(|line| line["message"] == message)
}
