//! Reading a workspace's log and one's own (the audit spec's Routes).

use http::Method;
use serde_json::{Value, json};

use crate::support::{
    Api, Options, TestDatabase,
    audit::{Logged, in_own_log},
    lists,
    users::{self, SignedIn},
    workspaces,
};

/// The path of `workspace`'s log.
fn log_of(workspace: &str) -> String {
    format!("/v1/workspaces/{workspace}/audit-entries")
}

/// The path of the caller's own entries.
const OWN: &str = "/v1/users/me/audit-entries";

/// The body of the page at `path`, read as `reader`, which answers 200.
async fn listed(api: &Api, reader: &SignedIn, path: &str) -> Value {
    let response = api.get(path).bearer(&reader.access).send().await;
    assert_eq!(response.status, 200, "{path}");
    response.json()
}

/// The `field` of each item of a list's body, with `-` for null.
fn column(body: &Value, field: &str) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item[field].as_str().unwrap_or("-").to_owned())
        .collect()
}

/// The action of each entry of the page at `path`, read as `reader`.
async fn actions(api: &Api, reader: &SignedIn, path: &str) -> Vec<String> {
    column(&listed(api, reader, path).await, "action")
}

/// The actions of `user`'s own log, the latest first.
async fn own_log(db: &TestDatabase, user: &str) -> Vec<String> {
    db.owner_reads(&format!(
        "SELECT action FROM audit_entries WHERE user_id = '{user}' \
         ORDER BY created_at DESC, id DESC"
    ))
    .await
}

/// Renames `workspace` as `member`, which records `workspace.renamed`, with `headers`.
async fn rename(api: &Api, member: &SignedIn, workspace: &str, headers: &[(&str, &str)]) {
    let mut call = api
        .patch(
            &format!("/v1/workspaces/{workspace}"),
            &json!({ "name": "Renamed" }),
        )
        .bearer(&member.access);
    for (name, value) in headers {
        call = call.header(name, value);
    }
    assert_eq!(call.send().await.status, 200);
}

/// Sends `method` to `path` as `caller`, with `headers`, which answers `status`.
async fn act(
    api: &Api,
    caller: &SignedIn,
    (method, path): (Method, &str),
    headers: &[(&str, &str)],
    status: u16,
) {
    let mut call = api.request(method, path).bearer(&caller.access);
    for (name, value) in headers {
        call = call.header(name, value);
    }
    assert_eq!(call.send().await.status, status, "{path}");
}

#[tokio::test]
async fn audit_007_the_owner_and_an_admin_read_the_workspace_s_log_the_latest_first() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let db = api.database();
    let acme = workspaces::arrange(
        db,
        "Acme",
        &[
            (&ada.user_id, "owner"),
            (&grace.user_id, "admin"),
            (&carol.user_id, "member"),
        ],
    )
    .await;
    let globex = workspaces::arrange(db, "Globex", &[(&ada.user_id, "owner")]).await;
    rename(&api, &grace, &acme, &[]).await;
    rename(&api, &ada, &globex, &[]).await;
    // A member's refusal is recorded in the workspace's log (AUDIT-004).
    act(&api, &carol, (Method::GET, &log_of(&acme)), &[], 403).await;
    let removal = format!("/v1/workspaces/{acme}/members/{}", carol.user_id);
    act(&api, &ada, (Method::DELETE, &removal), &[], 204).await;

    for reader in [&ada, &grace] {
        let body = listed(&api, reader, &log_of(&acme)).await;

        assert_eq!(
            column(&body, "action"),
            ["member.removed", "access.denied", "workspace.renamed"]
        );
        assert_eq!(column(&body, "workspace_id"), [acme.as_str(); 3]);
        assert_eq!(
            column(&body, "actor_user_id"),
            [ada.user_id.as_str(), &carol.user_id, &grace.user_id]
        );
        assert_eq!(column(&body, "target_user_id"), [&carol.user_id, "-", "-"]);
        assert_eq!(body["next_cursor"], Value::Null);
        let stored = db
            .owner_reads(
                "SELECT id || ' ' || resource_type || ' ' || resource_id || ' ' || details::text \
                 || ' ' || trace_id || ' ' \
                 || to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
                 FROM audit_entries WHERE action = 'member.removed'",
            )
            .await;
        let item = &body["items"][0];
        let shown = format!(
            "{} {} {} {} {} {}",
            item["id"].as_str().unwrap(),
            item["resource_type"].as_str().unwrap(),
            item["resource_id"].as_str().unwrap(),
            item["details"],
            item["trace_id"].as_str().unwrap(),
            item["created_at"].as_str().unwrap(),
        );
        assert_eq!(stored, [shown]);
    }
    // Each entry of a workspace's log records the permission that its action declares, and a
    // refusal none (the audit spec's "Reading a workspace's log").
    let recorded = db
        .owner_reads(&format!(
            "SELECT action || ' ' || coalesce(permission, '-') FROM audit_entries \
             WHERE workspace_id = '{acme}' ORDER BY created_at, id"
        ))
        .await;
    assert_eq!(
        recorded,
        [
            "workspace.renamed workspace.read",
            "access.denied -",
            "member.removed members.read",
        ]
    );
}

#[tokio::test]
async fn audit_007_an_entry_whose_permission_the_reader_s_role_lacks_is_left_out() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let acme = workspaces::arrange(
        db,
        "Acme",
        &[(&ada.user_id, "owner"), (&grace.user_id, "admin")],
    )
    .await;
    // `workspace.delete` is the owner's alone, and `members.read` every role's (the access spec).
    for (action, permission) in [
        ("thing.listed", Some("members.read")),
        ("thing.deleted", Some("workspace.delete")),
        ("thing.refused", None),
    ] {
        Logged {
            permission,
            ..Logged::new(&acme, &ada.user_id, action)
        }
        .record(db)
        .await;
    }

    assert_eq!(
        actions(&api, &grace, &log_of(&acme)).await,
        ["thing.refused", "thing.listed"]
    );
    assert_eq!(
        actions(&api, &ada, &log_of(&acme)).await,
        ["thing.refused", "thing.deleted", "thing.listed"]
    );
}

#[tokio::test]
async fn audit_007_a_page_boundary_between_entries_recorded_at_once_loses_none() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let acme = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    for action in ["thing.one", "thing.two", "thing.three"] {
        Logged::new(&acme, &ada.user_id, action).record(db).await;
    }
    db.owner_executes("UPDATE audit_entries SET created_at = '2026-11-01T12:00:00Z'")
        .await;

    let pages = lists::pages(&api, &ada, &format!("{}?limit=1", log_of(&acme)), "action").await;

    assert_eq!(pages.len(), 3);
    let mut seen: Vec<_> = pages.into_iter().flatten().collect();
    seen.sort();
    assert_eq!(seen, ["thing.one", "thing.three", "thing.two"]);
}

#[tokio::test]
async fn audit_008_a_user_reads_their_own_log_and_what_was_done_to_them() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let db = api.database();
    let acme = workspaces::arrange(
        db,
        "Acme",
        &[
            (&ada.user_id, "owner"),
            (&grace.user_id, "admin"),
            (&carol.user_id, "member"),
        ],
    )
    .await;
    let globex = workspaces::arrange(
        db,
        "Globex",
        &[(&ada.user_id, "owner"), (&grace.user_id, "admin")],
    )
    .await;
    // Grace acts in the workspace, and the owner acts on another member: neither is done to her.
    rename(&api, &grace, &acme, &[]).await;
    let carol_removed = format!("/v1/workspaces/{acme}/members/{}", carol.user_id);
    act(&api, &ada, (Method::DELETE, &carol_removed), &[], 204).await;
    let grace_removed = format!("/v1/workspaces/{acme}/members/{}", grace.user_id);
    act(&api, &ada, (Method::DELETE, &grace_removed), &[], 204).await;
    // In a workspace she still belongs to, an entry done to her whose permission her role,
    // admin, lacks: `workspace.delete` is the owner's alone (the access spec).
    Logged {
        target: Some(&grace.user_id),
        permission: Some("workspace.delete"),
        ..Logged::new(&globex, &ada.user_id, "thing.kept")
    }
    .record(db)
    .await;

    let body = listed(&api, &grace, OWN).await;

    // She is no member of Acme, and still reads what was done to her there, and she reads what
    // was done to her in Globex whatever its permission.
    let mut expected = vec!["thing.kept".to_owned(), "member.removed".to_owned()];
    expected.extend(own_log(db, &grace.user_id).await);
    assert!(expected.len() > 2, "her own log holds her registration");
    assert_eq!(column(&body, "action"), expected);
    let mut logs = vec![globex.clone(), acme.clone()];
    logs.resize(expected.len(), "-".to_owned());
    assert_eq!(column(&body, "workspace_id"), logs);
    assert_eq!(body["items"][1]["target_user_id"], grace.user_id.as_str());
    assert_eq!(body["items"][1]["actor_user_id"], ada.user_id.as_str());
    assert_eq!(body["next_cursor"], Value::Null);
}

#[tokio::test]
async fn audit_008_a_page_boundary_between_entries_of_both_reads_recorded_at_once_loses_none() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let acme = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    let grace_id = &grace.user_id;
    for action in ["own.one", "own.two"] {
        in_own_log(db, grace_id, grace_id, action, None).await;
    }
    for action in ["done.one", "done.two"] {
        Logged {
            target: Some(grace_id),
            ..Logged::new(&acme, &ada.user_id, action)
        }
        .record(db)
        .await;
    }
    db.owner_executes(&format!(
        "UPDATE audit_entries SET created_at = '2026-11-01T12:00:00Z' \
         WHERE user_id = '{grace_id}' OR target_user_id = '{grace_id}'"
    ))
    .await;
    let mut expected = own_log(db, grace_id).await;
    expected.extend(["done.one".to_owned(), "done.two".to_owned()]);
    expected.sort();

    let pages = lists::pages(&api, &grace, &format!("{OWN}?limit=1"), "action").await;

    assert_eq!(pages.len(), expected.len());
    let mut seen: Vec<_> = pages.into_iter().flatten().collect();
    seen.sort();
    assert_eq!(seen, expected);
}

/// Arranges, in `acme`'s log, four entries of one day, an hour apart from 10:00 to 13:00: two of
/// things, a resource of the test's own, acted by `ada` and by `grace`, one that `ada` did to
/// `grace`, and one of the workspace, acted by `grace`.
async fn four_entries(db: &TestDatabase, acme: &str, ada: &str, grace: &str) {
    let first = "00000000-0000-4000-8000-000000000001";
    let second = "00000000-0000-4000-8000-000000000002";
    let entries = [
        Logged {
            resource: ("thing", first),
            ..Logged::new(acme, ada, "thing.added")
        },
        Logged {
            resource: ("thing", second),
            ..Logged::new(acme, grace, "thing.removed")
        },
        Logged {
            target: Some(grace),
            ..Logged::new(acme, ada, "member.removed")
        },
        Logged::new(acme, grace, "workspace.renamed"),
    ];
    for (hour, entry) in (10..).zip(entries) {
        let id = entry.record(db).await;
        db.owner_executes(&format!(
            "UPDATE audit_entries SET created_at = '2026-11-01T{hour}:00:00Z' WHERE id = '{id}'"
        ))
        .await;
    }
}

#[tokio::test]
async fn audit_009_each_filter_keeps_the_entries_that_match_one_of_its_values() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let acme = workspaces::arrange(
        db,
        "Acme",
        &[(&ada.user_id, "owner"), (&grace.user_id, "admin")],
    )
    .await;
    four_entries(db, &acme, &ada.user_id, &grace.user_id).await;
    let (ada_id, grace_id) = (&ada.user_id, &grace.user_id);
    let first = "00000000-0000-4000-8000-000000000001";
    let second = "00000000-0000-4000-8000-000000000002";

    for (query, expected) in [
        (
            "action=thing.added,thing.removed".to_owned(),
            vec!["thing.removed", "thing.added"],
        ),
        (
            "resource_type=workspace".to_owned(),
            vec!["workspace.renamed", "member.removed"],
        ),
        (
            format!("resource_id={first},{second}"),
            vec!["thing.removed", "thing.added"],
        ),
        (
            format!("actor_user_id={grace_id}"),
            vec!["workspace.renamed", "thing.removed"],
        ),
        (
            format!("target_user_id={grace_id},{ada_id}"),
            vec!["member.removed"],
        ),
        (
            "from=2026-11-01T11:00:00Z&to=2026-11-01T13:00:00.000Z".to_owned(),
            vec!["member.removed", "thing.removed"],
        ),
        (
            "from=2026-11-01T11:00:00.000001Z".to_owned(),
            vec!["workspace.renamed", "member.removed"],
        ),
        (
            format!("action=thing.added,workspace.renamed&actor_user_id={grace_id}"),
            vec!["workspace.renamed"],
        ),
        ("action=nothing.happened".to_owned(), vec![]),
    ] {
        let path = format!("{}?{query}", log_of(&acme));
        assert_eq!(actions(&api, &ada, &path).await, expected, "{query}");
    }
}

/// Gives the entry `id` the time `at`, an SQL timestamp.
async fn recorded_at(db: &TestDatabase, id: &str, at: &str) {
    db.owner_executes(&format!(
        "UPDATE audit_entries SET created_at = '{at}' WHERE id = '{id}'"
    ))
    .await;
}

#[tokio::test]
async fn audit_009_each_filter_of_one_s_own_list_keeps_the_entries_of_both_its_reads() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let acme = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    let (ada_id, grace_id) = (ada.user_id.as_str(), grace.user_id.as_str());
    let first = "00000000-0000-4000-8000-000000000001";
    let second = "00000000-0000-4000-8000-000000000002";
    // Two entries of her own log, then two done to her in Acme's log, an hour apart; her
    // registration's entries are of today, after them all.
    let entries = [
        Logged {
            resource: ("session", first),
            ..Logged::own(grace_id, "thing.started")
        },
        Logged {
            target: Some(ada_id),
            ..Logged::own(grace_id, "thing.shared")
        },
        Logged {
            target: Some(grace_id),
            ..Logged::new(&acme, ada_id, "thing.done")
        },
        Logged {
            target: Some(grace_id),
            resource: ("thing", second),
            ..Logged::new(&acme, ada_id, "thing.started")
        },
    ];
    for (hour, entry) in (10..).zip(entries) {
        let id = entry.record(db).await;
        recorded_at(db, &id, &format!("2026-11-01T{hour}:00:00Z")).await;
    }
    // Neither is hers: one done to Ada, and one of Ada's own log.
    for entry in [
        Logged {
            target: Some(ada_id),
            ..Logged::new(&acme, ada_id, "thing.started")
        },
        Logged::own(ada_id, "thing.started"),
    ] {
        entry.record(db).await;
    }
    let before = "to=2026-11-02T00:00:00Z";

    for (query, expected) in [
        ("action=thing.started".to_owned(), vec!["thing.started"; 2]),
        (
            "resource_type=session,thing".to_owned(),
            vec!["thing.started"; 2],
        ),
        (
            format!("resource_id={first},{second}"),
            vec!["thing.started"; 2],
        ),
        (
            format!("actor_user_id={ada_id}"),
            vec!["thing.started", "thing.done"],
        ),
        (
            format!("actor_user_id={grace_id}&{before}"),
            vec!["thing.shared", "thing.started"],
        ),
        (format!("target_user_id={ada_id}"), vec!["thing.shared"]),
        (
            format!("target_user_id={grace_id}"),
            vec!["thing.started", "thing.done"],
        ),
        (
            "from=2026-11-01T11:00:00Z&to=2026-11-01T13:00:00Z".to_owned(),
            vec!["thing.done", "thing.shared"],
        ),
        (
            format!("from=2026-11-01T12:30:00Z&{before}"),
            vec!["thing.started"],
        ),
    ] {
        let body = listed(&api, &grace, &format!("{OWN}?{query}")).await;
        assert_eq!(column(&body, "action"), expected, "{query}");
    }
    // Each read gives its own: the same action in her own log and done to her.
    let both = listed(&api, &grace, &format!("{OWN}?action=thing.started")).await;
    assert_eq!(column(&both, "workspace_id"), [acme.as_str(), "-"]);
}

#[tokio::test]
async fn audit_009_the_values_a_list_refuses_are_named_together() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let acme = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;
    let too_many = vec![ada.user_id.as_str(); 51].join(",");
    let ada_id = &ada.user_id;
    // Two filters given as they should be, which no answer names, and then all seven refused.
    let some = format!(
        "action=Member.Removed&resource_type=file,&resource_id=nope&actor_user_id={too_many}\
         &target_user_id={ada_id}&from=2026-11-01&to=2026-11-01T00:00:00Z"
    );
    let every = format!(
        "action=member&resource_type=a.b&resource_id={ada_id},&actor_user_id=nope\
         &target_user_id={ada_id}x&from=2026-11-01T00:00:00%2B00:00&to=2026-11-01T24:00:00Z"
    );

    for (query, expected) in [
        (
            some,
            vec![
                "/action invalid",
                "/resource_type invalid",
                "/resource_id invalid",
                "/actor_user_id too_many",
                "/from invalid",
            ],
        ),
        (
            every,
            vec![
                "/action invalid",
                "/resource_type invalid",
                "/resource_id invalid",
                "/actor_user_id invalid",
                "/target_user_id invalid",
                "/from invalid",
                "/to invalid",
            ],
        ),
    ] {
        for path in [log_of(&acme), OWN.to_owned()] {
            let response = api
                .get(&format!("{path}?{query}"))
                .bearer(&ada.access)
                .send()
                .await;

            assert_eq!(response.status, 400, "{path}?{query}");
            let body = response.json();
            assert_eq!(body["code"], "request.invalid_query");
            let refused: Vec<String> = body["errors"]
                .as_array()
                .unwrap()
                .iter()
                .map(|error| {
                    format!(
                        "{} {}",
                        error["field"].as_str().unwrap(),
                        error["code"].as_str().unwrap()
                    )
                })
                .collect();
            assert_eq!(refused, expected, "{path}?{query}");
        }
    }
}

/// An API whose clients' addresses come from `X-Forwarded-For`, as the tests' proxy sends it
/// (SEC-13).
async fn behind_a_proxy() -> Api {
    Api::with(Options::default().set("TRUSTED_PROXIES", "198.51.100.0/24")).await
}

/// The names of the roles that hold `permission`, as the matrix of `access` gives them.
fn holders(permission: &str) -> Vec<&'static str> {
    app::permissions()
        .into_iter()
        .find(|(name, _)| *name == permission)
        .expect("a permission of the matrix")
        .1
}

// With the template's roles, every role that reads a workspace's log holds `audit.read_clients`
// (D166), so the test shows the log without its clients only once a matrix has a role that reads
// the log and doesn't hold it (TEST-08).
#[tokio::test]
async fn audit_010_a_role_that_reads_the_log_sees_its_clients_only_with_the_permission() {
    let api = behind_a_proxy().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let db = api.database();
    let members = [
        (ada.user_id.as_str(), "owner"),
        (grace.user_id.as_str(), "admin"),
        (carol.user_id.as_str(), "member"),
    ];
    let acme = workspaces::arrange(db, "Acme", &members).await;
    let client = [
        ("user-agent", "Desktop/1.2"),
        ("x-forwarded-for", "203.0.113.9"),
    ];
    rename(&api, &grace, &acme, &client).await;
    // An entry that no request recorded has no client.
    Logged::new(&acme, &grace.user_id, "thing.done")
        .record(db)
        .await;
    let (readers, seeing) = (holders("audit.read"), holders("audit.read_clients"));
    let mut read = 0;

    for (reader, role) in [(&ada, "owner"), (&grace, "admin"), (&carol, "member")] {
        if !readers.contains(&role) {
            continue;
        }
        let body = listed(&api, reader, &log_of(&acme)).await;

        assert_eq!(column(&body, "action"), ["thing.done", "workspace.renamed"]);
        assert_eq!(body["items"][0]["client"], Value::Null);
        let shown = if seeing.contains(&role) {
            json!({ "ip": "203.0.113.9", "user_agent": "Desktop/1.2" })
        } else {
            Value::Null
        };
        assert_eq!(body["items"][1]["client"], shown, "{role}");
        read += 1;
    }
    assert!(read >= 1, "no role reads the log");
}

#[tokio::test]
async fn audit_010_a_user_s_own_read_shows_only_the_clients_that_were_theirs() {
    let api = behind_a_proxy().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let db = api.database();
    let acme = workspaces::arrange(
        db,
        "Acme",
        &[
            (&ada.user_id, "owner"),
            (&grace.user_id, "admin"),
            (&carol.user_id, "member"),
        ],
    )
    .await;
    let removal = format!("/v1/workspaces/{acme}/members/{}", grace.user_id);
    let ada_client = [("user-agent", "Ada/1"), ("x-forwarded-for", "203.0.113.1")];
    act(&api, &ada, (Method::DELETE, &removal), &ada_client, 204).await;
    let leaving = format!("/v1/workspaces/{acme}/leave");
    // Carol's client sends no user agent.
    let carol_client = [("x-forwarded-for", "203.0.113.3")];
    act(&api, &carol, (Method::POST, &leaving), &carol_client, 204).await;
    // Grace signs in with a wrong password: the attempt has no actor, and its client is hers to
    // see, as the user's whose log holds it (AUDIT-006).
    let attempt = api
        .post(
            "/v1/sessions",
            &json!({ "email": "grace@example.com", "password": "wrong-password-123" }),
        )
        .header("user-agent", "Someone/9")
        .header("x-forwarded-for", "203.0.113.7")
        .send()
        .await;
    assert_eq!(attempt.status, 401);

    let graces = listed(&api, &grace, OWN).await;
    let carols = listed(&api, &carol, OWN).await;

    // What Ada did to Grace shows no client: it is Ada's.
    assert_eq!(graces["items"][1]["action"], "member.removed");
    assert_eq!(graces["items"][1]["client"], Value::Null);
    assert_eq!(graces["items"][0]["action"], "sign_in.failed");
    assert_eq!(
        graces["items"][0]["client"],
        json!({ "ip": "203.0.113.7", "user_agent": "Someone/9" })
    );
    // Carol did her leaving, so its client is hers, without the user agent it didn't send.
    assert_eq!(carols["items"][0]["action"], "member.left");
    assert_eq!(
        carols["items"][0]["client"],
        json!({ "ip": "203.0.113.3", "user_agent": null })
    );
}

#[tokio::test]
async fn audit_010_a_user_agent_is_kept_to_512_characters_and_none_that_holds_a_byte_outside_ascii()
{
    let api = behind_a_proxy().await;
    let ada = users::register(&api, "ada@example.com").await;
    let long = "A".repeat(600);
    for user_agent in [long.as_str(), "Caf\u{e9}/1"] {
        let attempt = api
            .post(
                "/v1/sessions",
                &json!({ "email": "ada@example.com", "password": "wrong-password-123" }),
            )
            .header("user-agent", user_agent)
            .header("x-forwarded-for", "203.0.113.7")
            .send()
            .await;
        assert_eq!(attempt.status, 401);
    }

    let own = listed(&api, &ada, OWN).await;

    assert_eq!(own["items"][1]["action"], "sign_in.failed");
    assert_eq!(own["items"][1]["client"]["user_agent"], "A".repeat(512));
    assert_eq!(own["items"][0]["action"], "sign_in.failed");
    assert_eq!(
        own["items"][0]["client"],
        json!({ "ip": "203.0.113.7", "user_agent": null })
    );
}
