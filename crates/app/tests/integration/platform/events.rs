//! The real-time hub (the platform spec's "Events and the real-time hub"): who receives what, in
//! which order, and when a stream closes, through the stream itself (TEST-07); the trace ID that
//! a stream reads of an event, and the trace that its delivery links to (PLATFORM-095, OBS-07);
//! what the hub loads and records (PLATFORM-027, PLATFORM-034, PLATFORM-035, PLATFORM-069); the
//! connections that its checks open (OPS-18); the sweep of old events (PLATFORM-019); and a wait
//! for a line of the hub, and the head that a test reads (TEST-06).

use std::{sync::Arc, time::Duration};

use http::StatusCode;
use platform::{clock::TestClock, lifecycle::Readiness, telemetry::testing, types::UserId};
use serde_json::json;

use super::spans::POSITION_LOCK;
use crate::support::{
    Api, CHECK_INTERVAL, CHECK_TIMEOUT, Options, Proxy, TestDatabase,
    announcements::{self, Note, published::NOTED},
    client,
    events::{self, Event},
    execute_on, hex, queries_waiting_on_locks, query_waits_on_a_lock,
    streams::{self, Stream},
    users, workspaces,
};

/// How often the hub re-checks its streams, by default (PLATFORM-031).
const RECHECK: Duration = Duration::from_secs(60);

/// A note labelled `label`.
fn note(label: &str) -> Note {
    Note {
        label: Some(label.to_owned()),
    }
}

/// The name of each of the next `count` events of `stream`.
async fn names(stream: &mut Stream, count: usize) -> Vec<String> {
    let mut names = Vec::new();
    for _ in 0..count {
        names.push(stream.event().await.event.unwrap());
    }
    names
}

#[tokio::test]
async fn platform_029_a_workspace_event_reaches_only_the_members_whose_role_holds_its_permission() {
    let api = announcements::api().await;
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

    announcements::announce_in(&api, &ada, &workspace, 1, true).await;
    announcements::announce_in(&api, &ada, &workspace, 1, false).await;
    // Each user's own events come last, numbered from 1, so another's would show as a number
    // out of its place.
    for (user, count) in [(&ada, 1), (&grace, 2), (&alan, 3)] {
        announcements::announce(&api, user, count).await;
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
        numbered(admin.events(3).await),
        [
            ("announcement.restricted".to_owned(), 1),
            ("announcement.posted".to_owned(), 1),
            made(1),
        ]
    );
    assert_eq!(
        numbered(member.events(3).await),
        [("announcement.posted".to_owned(), 1), made(1), made(2)]
    );
    assert_eq!(
        numbered(outsider.events(3).await),
        [made(1), made(2), made(3)]
    );
}

#[tokio::test]
async fn platform_029_a_replay_leaves_out_a_workspace_s_events_from_before_its_member_joined() {
    let api = announcements::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "owner")]).await;
    let mut first = streams::opened(&api, &grace).await;
    announcements::announce(&api, &grace, 1).await;
    first.event().await;
    let last = first.last_event_id.clone();
    drop(first);

    announcements::announce_in(&api, &ada, &workspace, 1, false).await;
    workspaces::join(api.database(), &workspace, &grace.user_id, "member").await;
    announcements::announce_in(&api, &ada, &workspace, 2, false).await;
    let mut again = streams::connect(&api, &grace.access, last.as_deref()).await;
    again.opened().await;
    let replayed = [again.event().await, again.event().await];
    announcements::announce(&api, &grace, 1).await;
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
    let api = announcements::api().await;
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

    announcements::announce_in(&api, &users[0], &workspace, 2, false).await;
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
    let api = announcements::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let other = users::new_session(&api, "ada@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "owner")]).await;
    let mut ended = streams::opened(&api, &ada).await;

    let signed_out = users::sign_out(&api).bearer(&ada.access).send().await;
    announcements::announce_in(&api, &other, &workspace, 1, false).await;
    announcements::announce(&api, &other, 1).await;
    let (before, reason) = ended.closed().await;

    assert_eq!(signed_out.status, StatusCode::NO_CONTENT);
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(reason, "session_ended");
}

#[tokio::test]
async fn platform_028_a_stream_resumes_on_another_replica_with_every_event_after_its_last() {
    let api = announcements::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    // A second replica: another application, with a hub of its own, on the same database.
    let second = crate::support::api::application(
        api.pool(),
        Options::routes(announcements::routes()),
        api.clock(),
        platform::lifecycle::Readiness::new(),
    )
    .await
    .router;
    let mut first = streams::opened(&api, &ada).await;
    announcements::announce(&api, &ada, 1).await;
    first.event().await;
    let last = first.last_event_id.clone();
    drop(first);

    announcements::announce(&api, &ada, 2).await;
    let mut resumed = streams::connect_on(&api, second, &ada.access, last.as_deref()).await;
    resumed.opened().await;
    let replayed = [resumed.event().await, resumed.event().await];
    announcements::announce(&api, &ada, 1).await;
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
    let api = announcements::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let user = UserId::from_uuid(ada.user_id.parse().unwrap());
    let mut stream = streams::opened(&api, &ada).await;
    announcements::announce(&api, &ada, 1).await;
    stream.event().await;
    let before = stream.last_event_id.clone();

    // The first unit of work publishes first and commits last.
    let mut first = api.pool().begin(&client(), None).await.unwrap();
    first
        .publish_to_user(&NOTED, user, &note("first"))
        .await
        .unwrap();
    let mut second = api.pool().begin(&client(), None).await.unwrap();
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
    let api = announcements::api().await;
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
    announcements::announce(&api, &grace, 1).await;
    let still = unchanged.event().await;

    assert_eq!(reason, "access_changed");
    assert_eq!(still.event.as_deref(), Some("announcement.made"));
}

#[tokio::test]
async fn platform_031_the_recheck_closes_a_stream_whose_session_stopped_lasting_without_a_signal() {
    let api = announcements::api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Analytical",
        &[(&grace.user_id, "owner"), (&ada.user_id, "member")],
    )
    .await;
    let mut ended = streams::opened(&api, &ada).await;
    let mut lasting = streams::opened(&api, &grace).await;

    // Gone behind the API's back, so no signal tells the hub.
    api.database()
        .owner_executes(&format!(
            "DELETE FROM sessions WHERE user_id = '{}'",
            ada.user_id
        ))
        .await;
    clock.sleeping(RECHECK).await;
    clock.advance(RECHECK);
    // The hub sleeps again once its re-check has ended; an event that commits after it reaches
    // each stream that stayed open.
    clock.sleeping(RECHECK).await;
    announcements::announce_in(&api, &grace, &workspace, 1, false).await;
    let closed = ended.closing().await;
    let still = lasting.event().await;

    assert_eq!(closed.payload()["reason"], "session_ended");
    assert_eq!(still.event.as_deref(), Some("announcement.posted"));
}

#[tokio::test]
async fn workspaces_005_removing_a_member_closes_their_stream_before_the_next_event() {
    let api = announcements::api().await;
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
    announcements::announce_in(&api, &ada, &workspace, 1, false).await;
    let (before, reason) = removed.closed().await;

    assert_eq!(response.status, StatusCode::NO_CONTENT);
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(reason, "access_changed");
    assert_eq!(
        names(&mut admin, 2).await,
        ["member.removed", "announcement.posted"]
    );
}

// part: workspace_creation
#[tokio::test]
async fn access_006_a_membership_that_starts_closes_its_user_s_streams() {
    let api = announcements::api().await;
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
    announcements::announce(&api, &grace, 1).await;
    let still = other.event().await;

    assert_eq!(response.status, StatusCode::CREATED);
    assert_eq!(reason, "access_changed");
    assert_eq!(still.event.as_deref(), Some("announcement.made"));
}
// end part: workspace_creation

#[tokio::test]
async fn access_007_a_stream_holds_each_workspace_s_permissions_of_its_role_there() {
    let api = announcements::api().await;
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

    announcements::announce_in(&api, &grace, &joined, 1, true).await;
    announcements::announce_in(&api, &grace, &owned, 1, true).await;
    announcements::announce_in(&api, &grace, &joined, 1, false).await;

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
async fn access_007_a_stream_holds_a_workspace_from_the_transaction_that_added_the_membership() {
    let api = announcements::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Joined", &[(&grace.user_id, "owner")]).await;
    let mut first = streams::opened(&api, &ada).await;
    announcements::announce(&api, &ada, 1).await;
    first.event().await;
    let before = first.last_event_id.clone();
    drop(first);
    announcements::announce_in(&api, &grace, &workspace, 1, false).await;
    workspaces::join(api.database(), &workspace, &ada.user_id, "member").await;

    let mut resumed = streams::connect(&api, &ada.access, before.as_deref()).await;
    resumed.opened().await;
    announcements::announce(&api, &ada, 1).await;
    let next = resumed.event().await;

    // The post committed before the transaction that made Ada a member, so it isn't replayed.
    assert_eq!(next.event.as_deref(), Some("announcement.made"));
}

#[tokio::test]
async fn auth_018_a_stream_outlives_its_access_token_but_not_its_session() {
    let api = announcements::api().await;
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
    announcements::announce(&api, &again, 1).await;
    // Grace's new session's start reaches her lasting stream first (AUTH-046).
    let started = lasting.event().await;
    let still = lasting.event().await;

    assert_eq!(reason, "session_ended");
    assert_eq!(started.event.as_deref(), Some("session.created"));
    assert_eq!(still.event.as_deref(), Some("announcement.made"));
}

#[tokio::test]
async fn platform_032_a_stream_whose_client_reads_too_slowly_closes_with_lagging() {
    let api = announcements::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;

    // Nothing reads the stream while 300 events arrive at once.
    announcements::announce(&api, &ada, 300).await;
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
    api.database().let_app_in().await;
    clock.advance(CHECK_INTERVAL);
    unavailable
}

#[tokio::test]
async fn platform_033_after_a_lost_connection_every_stream_closes_and_replays_what_committed_meanwhile()
 {
    let api = announcements::api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;
    announcements::announce(&api, &ada, 1).await;
    stream.event().await;
    let last = stream.last_event_id.clone();
    clock.sleeping(CHECK_INTERVAL).await;

    let unavailable = lose_the_connection(
        &api,
        &events::statement(
            &[Event::to_user(&ada.user_id, r#"{"label": "away"}"#)],
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
    let api = announcements::api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "member")]).await;
    let mut stream = streams::opened(&api, &ada).await;
    announcements::announce(&api, &ada, 1).await;
    stream.event().await;
    let last = stream.last_event_id.clone();
    clock.sleeping(CHECK_INTERVAL).await;

    // While no connection listens, an event of her workspace commits, then her removal, whose
    // signal is lost with the notifications, then another event of the workspace.
    lose_the_connection(
        &api,
        &format!(
            "{}; DELETE FROM workspace_memberships WHERE workspace_id = '{workspace}'; {}",
            events::statement(&[Event::to_workspace(&workspace)], false),
            events::statement(&[Event::to_workspace(&workspace)], false),
        ),
    )
    .await;
    let (before, reason) = stream.closed().await;
    let mut again = streams::connect(&api, &ada.access, last.as_deref()).await;
    again.opened().await;
    announcements::announce(&api, &ada, 1).await;
    let next = again.event().await;

    assert!(before.is_empty(), "{before:?}");
    assert_eq!(reason, "interrupted");
    // Her replay holds neither event of the workspace: she isn't its member now.
    assert_eq!(next.event.as_deref(), Some("announcement.made"));
}

#[tokio::test]
async fn platform_033_a_re_check_that_fails_is_tried_again_at_the_next_check() {
    let api = announcements::api().await;
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
    let held = api.every_connection().await;
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
    let held = api.every_connection().await;
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
    let api = announcements::api().await;
    let name = api.database().name().to_owned();
    let ours = |point: &testing::Point| point.labels.get("db.namespace") == Some(&name);
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;
    let open = testing::recorded("platform.streams")
        .into_iter()
        .find(|point| ours(point))
        .map(|point| point.value);

    announcements::announce(&api, &ada, 1).await;
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
async fn platform_034_the_lag_is_the_seconds_from_an_event_s_commit_to_its_delivery() {
    let api = announcements::api().await;
    let db = api.database();
    let name = db.name().to_owned();
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;

    // An event that committed a minute ago, by the database's clock, which the hub hears of now.
    db.owner_executes(&events::statement(
        &[Event {
            committed_before: Some("1 minute"),
            ..Event::to_user(&ada.user_id, "{}")
        }],
        true,
    ))
    .await;
    let delivered = stream.event().await;

    let lag = testing::recorded("platform.event_delivery_lag")
        .into_iter()
        .find(|point| point.labels.get("db.namespace") == Some(&name))
        .unwrap();
    assert_eq!(delivered.event.as_deref(), Some("note.made"));
    // The start of Ada's session (AUTH-046), loaded as it committed, and the minute-old event.
    assert_eq!(lag.count, Some(2));
    assert!((60.0..70.0).contains(&lag.value), "{}", lag.value);
}

#[tokio::test]
async fn platform_019_the_sweep_deletes_the_events_older_than_24_hours() {
    let api = announcements::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    announcements::announce(&api, &ada, 1).await;
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
    let api = announcements::api().await;
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
    let api = announcements::api().await;
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
    let api = announcements::api().await;
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
    announcements::announce(&api, &alan, 1).await;
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
    announcements::announce(&api, &alan, 1).await;
    let after_joining = joined.event().await;

    assert_eq!(removed.status, StatusCode::NO_CONTENT);
    assert_eq!(names(&mut owner, 1).await, ["member.removed"]);
    assert_eq!(names(&mut member, 1).await, ["member.removed"]);
    // The removal from before Alan joined isn't replayed to him.
    assert_eq!(after_joining.event.as_deref(), Some("announcement.made"));
}

#[tokio::test]
async fn platform_095_an_event_s_trace_id_reaches_its_publisher_s_streams_alone_live_and_replayed()
{
    let api = announcements::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Analytical",
        &[(&ada.user_id, "admin"), (&grace.user_id, "member")],
    )
    .await;
    // Another session of the publisher's, as on another of her devices.
    let elsewhere = users::new_session(&api, "ada@example.com").await;
    let callers = [&ada, &elsewhere, &grace];
    let mut live = Vec::new();
    for caller in callers {
        live.push(streams::opened(&api, caller).await);
    }

    let first = announcements::announce_in(&api, &ada, &workspace, 1, false).await;
    let mut delivered = Vec::new();
    let mut resume = Vec::new();
    for stream in &mut live {
        let frame = stream.event().await;
        delivered.push(frame.data.unwrap()["trace_id"].clone());
        resume.push(frame.id.unwrap());
    }
    // A second event, which each caller's reconnect after the first replays.
    let second = announcements::announce_in(&api, &ada, &workspace, 1, false).await;
    let mut replayed = Vec::new();
    for (caller, after) in callers.iter().zip(&resume) {
        let mut stream = streams::connect(&api, &caller.access, Some(after)).await;
        stream.opened().await;
        replayed.push(stream.event().await.data.unwrap()["trace_id"].clone());
    }

    assert_ne!(first, second);
    assert_eq!(delivered, [json!(first), json!(first), json!(null)]);
    assert_eq!(replayed, [json!(second), json!(second), json!(null)]);
}

#[tokio::test]
async fn platform_095_an_event_of_a_unit_of_work_that_no_principal_opened_has_no_publisher() {
    let api = announcements::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let user = UserId::from_uuid(ada.user_id.parse().unwrap());

    announcements::announce(&api, &ada, 1).await;
    let mut uow = api.pool().begin(&client(), None).await.unwrap();
    uow.publish_to_user(&NOTED, user, &note("unattributed"))
        .await
        .unwrap();
    uow.commit().await.unwrap();

    let publishers = api
        .database()
        .owner_reads("SELECT name || ' ' || coalesce(publisher_id::text, 'no one') FROM events ORDER BY position")
        .await;
    // Registering published `session.created` too, in a request that no session proved the
    // caller of.
    assert_eq!(
        publishers,
        [
            "session.created no one".to_owned(),
            format!("announcement.made {}", ada.user_id),
            "note.made no one".to_owned(),
        ]
    );
}

#[tokio::test]
async fn platform_095_deleting_a_publisher_s_account_leaves_their_events_to_others_without_one() {
    let api = announcements::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Analytical",
        &[(&ada.user_id, "member"), (&grace.user_id, "admin")],
    )
    .await;
    announcements::announce_in(&api, &ada, &workspace, 1, false).await;
    let publishers = async || {
        api.database()
            .owner_reads(
                "SELECT name || ' ' || coalesce(publisher_id::text, 'no one') FROM events \
                 WHERE workspace_id IS NOT NULL ORDER BY position",
            )
            .await
    };
    let before = publishers().await;

    // The deletion's own event, that she left the workspace, loses its publisher with her row.
    let deleted = users::delete(&api, &ada).await;

    assert_eq!(deleted.status, StatusCode::NO_CONTENT, "{}", deleted.json());
    assert_eq!(before, [format!("announcement.posted {}", ada.user_id)]);
    assert_eq!(
        publishers().await,
        ["announcement.posted no one", "member.left no one"]
    );
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
                ..Event::to_user(&user, "{}")
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
    assert_eq!(hex(&span.links[0].trace_id), trace);
    assert_eq!(hex(&span.links[0].span_id), "00f067aa0ba902b7");
    assert_ne!(hex(&span.trace_id), trace);
}

#[tokio::test]
async fn platform_029_an_event_that_commits_while_its_member_joins_reaches_them_not() {
    let api = announcements::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let alan = users::register(&api, "alan@example.com").await;
    let workspace =
        workspaces::arrange(api.database(), "Analytical", &[(&ada.user_id, "owner")]).await;
    let mut first = streams::opened(&api, &alan).await;
    announcements::announce(&api, &alan, 1).await;
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
    announcements::announce_in(&api, &ada, &workspace, 1, false).await;
    crate::support::execute_on(&mut joining, "COMMIT").await;
    announcements::announce_in(&api, &ada, &workspace, 2, false).await;
    let mut again = streams::connect(&api, &alan.access, before.as_deref()).await;
    again.opened().await;
    let replayed = [again.event().await, again.event().await];

    let numbers: Vec<_> = replayed
        .iter()
        .map(|frame| frame.payload()["n"].clone())
        .collect();
    // The second post's two events, not the first's, which committed before Alan was a member.
    assert_eq!(numbers, [json!(1), json!(2)]);
    announcements::announce(&api, &alan, 1).await;
    assert_eq!(
        again.event().await.event.as_deref(),
        Some("announcement.made")
    );
}

#[tokio::test]
async fn platform_033_a_load_that_fails_is_tried_again_at_the_next_check() {
    let api = announcements::api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;
    clock.sleeping(CHECK_INTERVAL).await;

    // Every pooled connection is held, so the hub's load waits out the pool and fails.
    let held = api.every_connection().await;
    api.database()
        .owner_executes(&events::statement(
            &[Event::to_user(&ada.user_id, r#"{"label": "loaded"}"#)],
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
async fn platform_033_a_load_that_fails_is_tried_again_after_a_reconnect_that_cannot_read_the_log()
{
    let api = announcements::api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;
    clock.sleeping(CHECK_INTERVAL).await;

    // Every pooled connection is held, so the hub's load waits out the pool and fails, and so
    // does its read of the log's head once its listener connects again.
    let held = api.every_connection().await;
    api.database()
        .owner_executes(&events::statement(
            &[Event::to_user(&ada.user_id, "{}")],
            true,
        ))
        .await;
    while !logged("the hub couldn't load events") {
        tokio::task::yield_now().await;
    }
    lose_the_connection(&api, "SELECT 1").await;
    stream.closed().await;
    while !logged("the hub couldn't read the log's head") {
        tokio::task::yield_now().await;
    }
    drop(held);
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_INTERVAL).await;
    let name = api.database().name().to_owned();
    let unloaded = testing::recorded("platform.events_unloaded")
        .into_iter()
        .find(|point| point.labels.get("db.namespace") == Some(&name))
        .map(|point| point.value);

    // The hub owes nothing once its load has reached the event (PLATFORM-035).
    assert_eq!(unloaded, Some(0.0));
}

#[tokio::test]
async fn platform_033_a_reconnect_that_cannot_read_the_log_s_head_is_made_again_at_the_next_check()
{
    let api = announcements::api().await;
    let clock = api.clock();
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;
    announcements::announce(&api, &ada, 1).await;
    stream.event().await;
    let last = stream.last_event_id.clone();
    clock.sleeping(CHECK_INTERVAL).await;
    // Every pooled connection is held, so the hub's read of the log's head fails once its
    // listener connects again, while an event that committed without a connection, whose
    // notification is lost, waits past the hub's head.
    let held = api.every_connection().await;
    lose_the_connection(
        &api,
        &events::statement(
            &[Event::to_user(&ada.user_id, r#"{"label": "away"}"#)],
            false,
        ),
    )
    .await;
    stream.closed().await;
    while !logged("the hub couldn't read the log's head") {
        tokio::task::yield_now().await;
    }
    drop(held);
    // A client that reconnects before the next check, whose replay reaches only the head that
    // stayed.
    let bearer = format!("Bearer {}", ada.access);
    let resumed = [
        ("authorization", bearer.as_str()),
        ("last-event-id", last.as_deref().unwrap()),
    ];
    let mut early = streams::open(&api, &resumed).await.stream().unwrap();
    early.opened().await;

    clock.advance(CHECK_INTERVAL);
    let (before, reason) = early.closed().await;
    let mut again = streams::connect(&api, &ada.access, last.as_deref()).await;
    again.opened().await;
    let replayed = again.event().await;

    assert!(before.is_empty(), "{before:?}");
    assert_eq!(reason, "interrupted");
    assert_eq!(replayed.payload(), &json!({ "label": "away" }));
}

#[tokio::test]
async fn platform_035_the_hub_records_the_events_it_owes_while_its_loads_fail() {
    let api = announcements::api().await;
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
    let held = api.every_connection().await;
    api.database()
        .owner_executes(&events::statement(
            &[Event::to_user(&ada.user_id, "{}")],
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

#[tokio::test]
async fn test_06_a_wait_for_a_line_of_the_hub_ends_on_no_line_of_another_test() {
    let _api = announcements::api().await;

    // As another test's hub, which runs on that test's thread, writes it.
    std::thread::spawn(|| tracing::warn!("the hub couldn't load events"))
        .join()
        .unwrap();

    assert!(!logged("the hub couldn't load events"));
}

#[tokio::test]
async fn platform_028_a_commit_that_publishes_waits_for_the_lock_of_the_commits_that_publish() {
    let api = announcements::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    let user = UserId::from_uuid(ada.user_id.parse().unwrap());
    let db = api.database();
    // As a commit that publishes holds it while it positions its events.
    let mut held = db
        .holding(&format!("SELECT pg_advisory_xact_lock({POSITION_LOCK})"))
        .await;
    let pool = api.pool().clone();

    let committing = tokio::spawn(async move {
        let mut uow = pool.begin(&client(), None).await?;
        uow.publish_to_user(&NOTED, user, &note("waits")).await?;
        uow.commit().await
    });
    let waited = loop {
        if committing.is_finished() {
            break false;
        }
        let waiting = queries_waiting_on_locks(db).await;
        if waiting
            .iter()
            .any(|query| query.contains("pg_advisory_xact_lock"))
        {
            break true;
        }
        tokio::task::yield_now().await;
    };
    execute_on(&mut held, "COMMIT").await;
    let committed = committing.await.unwrap();

    assert!(waited, "the commit ended without the lock");
    assert!(committed.is_ok(), "{committed:?}");
}

#[tokio::test]
async fn platform_028_a_stream_that_resumes_past_its_hub_s_head_skips_what_its_client_has() {
    let api = announcements::api().await;
    let ada = users::register(&api, "ada@example.com").await;
    api.hub_holds_what_committed().await;
    // An event that commits with no notification, which no hub loads until a later one comes, as
    // the hub of a replica lags behind the one whose stream delivered it.
    api.database()
        .owner_executes(&events::statement(
            &[Event::to_user(&ada.user_id, r#"{"label": "received"}"#)],
            false,
        ))
        .await;
    let received = api
        .database()
        .owner_reads("SELECT id::text FROM events ORDER BY position DESC LIMIT 1")
        .await
        .remove(0);
    let bearer = format!("Bearer {}", ada.access);
    let resumed = [
        ("authorization", bearer.as_str()),
        ("last-event-id", received.as_str()),
    ];
    let mut stream = streams::open(&api, &resumed).await.stream().unwrap();
    stream.opened().await;

    // The hub loads both events at this one's notification.
    announcements::announce(&api, &ada, 1).await;
    let next = stream.event().await;

    assert_eq!(next.event.as_deref(), Some("announcement.made"));
}

#[tokio::test]
async fn platform_031_a_session_that_ends_while_its_stream_opens_closes_the_stream_before_any_event()
 {
    let api = announcements::api().await;
    let name = api.database().name().to_owned();
    let ended = || {
        testing::recorded("platform.stream_closes")
            .into_iter()
            .find(|point| {
                point.labels.get("db.namespace") == Some(&name)
                    && point.labels["stream.close_reason"] == "session_ended"
            })
            .map(|point| point.value)
    };
    let ada = users::register(&api, "ada@example.com").await;
    let other = users::new_session(&api, "ada@example.com").await;
    announcements::announce(&api, &ada, 1).await;
    let last = api
        .database()
        .owner_reads("SELECT id::text FROM events ORDER BY position DESC LIMIT 1")
        .await
        .remove(0);
    // The stream reads its caller's memberships once the hub holds it, which waits for this lock.
    let mut held = api
        .database()
        .holding("LOCK TABLE workspace_memberships IN ACCESS EXCLUSIVE MODE")
        .await;
    let bearer = format!("Bearer {}", ada.access);
    let resumed = [
        ("authorization", bearer.as_str()),
        ("last-event-id", last.as_str()),
    ];

    let (opened, signed_out) = tokio::join!(streams::open(&api, &resumed), async {
        query_waits_on_a_lock(api.database(), "FROM workspace_memberships").await;
        let signed_out = users::sign_out(&api).bearer(&ada.access).send().await;
        // An event after the session's end, which its replay would hold.
        announcements::announce(&api, &other, 1).await;
        while ended() != Some(1.0) {
            tokio::task::yield_now().await;
        }
        execute_on(&mut held, "ROLLBACK").await;
        signed_out
    });
    let mut stream = opened.stream().unwrap();
    stream.opened().await;
    let (before, reason) = stream.closed().await;

    assert_eq!(signed_out.status, StatusCode::NO_CONTENT);
    assert!(before.is_empty(), "{before:?}");
    assert_eq!(reason, "session_ended");
}

#[tokio::test]
async fn platform_033_a_load_that_fails_is_tried_again_after_a_reconnect_that_finds_no_event_past_the_head()
 {
    let api = announcements::api().await;
    let clock = api.clock();
    let name = api.database().name().to_owned();
    let unloaded = || {
        testing::recorded("platform.events_unloaded")
            .into_iter()
            .find(|point| point.labels.get("db.namespace") == Some(&name))
            .map(|point| point.value)
    };
    let ada = users::register(&api, "ada@example.com").await;
    api.hub_holds_what_committed().await;
    clock.sleeping(CHECK_INTERVAL).await;
    // Every pooled connection is held, so the hub's load waits out the pool and fails.
    let held = api.every_connection().await;
    api.database()
        .owner_executes(&events::statement(
            &[Event::to_user(&ada.user_id, "{}")],
            true,
        ))
        .await;
    while !unloaded().is_some_and(|owed| owed > 0.0) {
        tokio::task::yield_now().await;
    }
    drop(held);

    // The event goes while the listener is lost, as with its user's account, so the log's head
    // that the reconnect reads is the hub's own.
    lose_the_connection(
        &api,
        "DELETE FROM events WHERE position = (SELECT max(position) FROM events)",
    )
    .await;
    clock.sleeping(CHECK_INTERVAL).await;
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_INTERVAL).await;

    // The check after the reconnect makes the load, which owes nothing once it reaches the
    // position that the notification named (PLATFORM-035).
    assert_eq!(unloaded(), Some(0.0));
}

#[tokio::test]
async fn platform_069_the_database_gauge_follows_each_check_once_the_streams_have_closed() {
    let api = announcements::api().await;
    let clock = api.clock();
    let name = api.database().name().to_owned();
    let reachable = || {
        testing::recorded("platform.database_reachable")
            .into_iter()
            .find(|point| point.labels.get("db.namespace") == Some(&name))
            .map(|point| point.value)
    };
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;
    clock.sleeping(CHECK_INTERVAL).await;

    api.readiness().stop();
    let (_, reason) = stream.closed().await;
    api.database().cut_off_app().await;
    clock.advance(CHECK_INTERVAL);
    // A hub whose task ended would sleep no more, and a panic writes its line (ERR-12).
    let checked = tokio::select! {
        () = clock.sleeping(CHECK_INTERVAL) => true,
        () = error_logged() => false,
    };

    assert_eq!(reason, "shutdown");
    assert!(
        checked,
        "the hub stopped checking once it closed its streams"
    );
    assert_eq!(reachable(), Some(0.0));
}

#[tokio::test]
async fn test_06_a_test_reads_the_hub_s_head_as_the_last_position_it_loaded() {
    let db = TestDatabase::migrated().await;
    let application = crate::support::api::application(
        &db.app().await,
        Options::default(),
        Arc::new(TestClock::default()),
        Readiness::new(),
    )
    .await;
    let user = users::without_password(&db, "ada@example.com").await;
    let started = application.hub.head().await;

    db.owner_executes(&events::statement(&[Event::to_user(&user, "{}")], true))
        .await;
    let newest: i64 = db
        .owner_reads("SELECT max(position)::text FROM events")
        .await
        .remove(0)
        .parse()
        .unwrap();
    // The hub loads the event once its notification reaches it.
    let mut head = application.hub.head().await;
    while head.is_some_and(|head| head < newest) {
        tokio::task::yield_now().await;
        head = application.hub.head().await;
    }

    assert_eq!(started, Some(0));
    assert_eq!(head, Some(newest));
}

/// Whether the hub of this test has logged `message`: its runtime runs the hub on the test's own
/// thread, so that a line of another test's hub ends no wait of this one (TEST-06).
fn logged(message: &str) -> bool {
    testing::logged_on_this_thread()
        .iter()
        .any(|line| line["message"] == message)
}

/// Completes once a line at `ERROR` of this test's own is logged, as a panic's is (ERR-12).
async fn error_logged() {
    while !testing::logged_on_this_thread()
        .iter()
        .any(|line| line["level"] == "ERROR")
    {
        tokio::task::yield_now().await;
    }
}
