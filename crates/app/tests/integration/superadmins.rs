//! The deployment's superadmins: the operator's subcommand `superadmin`, what authorizes a
//! superadmin's routes, and the list of every workspace (the superadmins spec, SUPERADMINS-001
//! to SUPERADMINS-007, and the workspaces spec's "A superadmin's routes", WORKSPACES-029).

use std::process::Output;

use http::Method;
use serde_json::{Value, json};

use crate::{
    accounts::exports::{built, document},
    support::{
        Api, Options, execute_on,
        lists::pages,
        mailbox,
        operator::{EMAIL_OFF, SENDS_EMAIL, finished, refused},
        queries_wait_on_locks, second_factors, superadmins,
        users::{self, SignedIn},
        workspaces,
    },
};

/// The subcommand, which a line of its log names as its area (OPS-03).
const AREA: &str = "superadmin";

/// The list of every workspace, a superadmin's route.
const EVERY: &str = "/v1/superadmin/workspaces";

/// Runs the binary's `superadmin` with `arguments` on `api`'s database, in a deployment with the
/// settings `env`.
async fn run(api: &Api, env: &[(&str, &str)], arguments: &[&str]) -> Output {
    let arguments: Vec<&str> = [AREA]
        .into_iter()
        .chain(arguments.iter().copied())
        .collect();
    api.database().run_api(&arguments, env).await
}

/// The first column of each row of `sql`, as the `owner` reads it.
async fn reads(api: &Api, sql: &str) -> Vec<String> {
    api.database().owner_reads(sql).await
}

/// The user of each superadmin, the oldest first.
async fn superadmins_of(api: &Api) -> Vec<String> {
    reads(api, "SELECT user_id::text FROM superadmins ORDER BY id").await
}

/// The log, actor, target, resource and details of each entry of `action`, the oldest first,
/// with `-` for no actor, no target or no resource ID, and how many of them record a client.
async fn entries(api: &Api, action: &str) -> (Vec<String>, String) {
    let entries = reads(
        api,
        &format!(
            "SELECT scope || ' ' || coalesce(workspace_id, user_id) || ' ' \
             || coalesce(actor_user_id::text, '-') || ' ' || coalesce(target_user_id::text, '-') \
             || ' ' || resource_type || ' ' || coalesce(resource_id::text, '-') || ' ' \
             || details::text \
             FROM audit_entries WHERE action = '{action}' ORDER BY id"
        ),
    )
    .await;
    let clients = reads(
        api,
        &format!(
            "SELECT count(*)::text FROM audit_entry_clients c JOIN audit_entries e \
             ON e.id = c.entry_id WHERE e.action = '{action}' \
             AND c.client_ip IS NOT NULL AND c.user_agent IS NOT NULL"
        ),
    )
    .await
    .remove(0);
    (entries, clients)
}

/// Whether `user` reads that they are a superadmin (SUPERADMINS-006).
async fn standing(api: &Api, user: &SignedIn) -> Value {
    let response = api
        .get("/v1/users/me/superadmin")
        .bearer(&user.access)
        .send()
        .await;
    assert_eq!(response.status, 200, "{}", response.json());
    response.json()
}

/// Every route of the API's document whose access is `superadmin`, as its method and its path.
fn superadmin_routes(api: &Api) -> Vec<(Method, String)> {
    api.operations()
        .into_iter()
        .filter(|(_, _, access)| access == "superadmin")
        .map(|(method, path, _)| (method, path))
        .collect()
}

#[tokio::test]
async fn superadmins_001_add_makes_the_account_that_holds_the_address_a_superadmin() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let grace = users::register(&api, &mailbox::address("grace")).await;

    let added = run(&api, &SENDS_EMAIL, &["add", &address.to_uppercase()]).await;

    finished(
        &added,
        AREA,
        "made the account that holds the address a superadmin; a superadmin's routes serve it \
         once it has an authenticator app",
        &[&address, &address.to_uppercase()],
    );
    let user = &ada.user_id;
    assert_eq!(superadmins_of(&api).await, std::slice::from_ref(user));
    let (recorded, with_client) = entries(&api, "superadmin.added").await;
    assert_eq!(recorded, [format!("user {user} - - user {user} {{}}")]);
    assert_eq!(
        with_client, "0",
        "an operator's subcommand records no client"
    );
    assert_eq!(standing(&api, &ada).await, json!({ "superadmin": true }));
    assert_eq!(standing(&api, &grace).await, json!({ "superadmin": false }));
}

#[tokio::test]
async fn superadmins_002_add_changes_nothing_for_what_names_no_account() {
    let api = Api::new().await;
    users::register(&api, &mailbox::address("ada")).await;
    let nobody = mailbox::address("nobody");

    let not_an_address = run(&api, &SENDS_EMAIL, &["add", "ada"]).await;
    let no_account = run(&api, &SENDS_EMAIL, &["add", &nobody]).await;

    refused(
        &not_an_address,
        AREA,
        "the address given isn't an email address, so no account holds it: nothing changed",
        &[],
    );
    refused(
        &no_account,
        AREA,
        "no account holds the address: nothing changed",
        &[&nobody],
    );
    assert!(superadmins_of(&api).await.is_empty());
    assert!(entries(&api, "superadmin.added").await.0.is_empty());
}

#[tokio::test]
async fn superadmins_002_with_email_on_an_account_that_has_not_proved_its_address_becomes_none() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    users::register_unverified(&api, &address).await;

    let unproved = run(&api, &SENDS_EMAIL, &["add", &address]).await;

    refused(
        &unproved,
        AREA,
        "the account that holds the address hasn't proved it, so it may not be its owner's: \
         nothing changed; run it again once the account has proved the address",
        &[&address],
    );
    assert!(superadmins_of(&api).await.is_empty());
    assert!(entries(&api, "superadmin.added").await.0.is_empty());
}

#[tokio::test]
async fn superadmins_002_with_email_off_an_account_that_never_proved_its_address_becomes_one() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    // Nothing proves an address with email off, so the account never proved its own.
    let ada = users::register_unverified(&api, &address).await;

    let added = run(&api, &EMAIL_OFF, &["add", &address]).await;

    assert_eq!(added.status.code(), Some(0));
    assert_eq!(superadmins_of(&api).await, [ada.user_id]);
}

#[tokio::test]
async fn superadmins_002_with_email_off_an_account_that_proved_its_address_becomes_one() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    // The account proved its address while the deployment sent email.
    let ada = users::register(&api, &address).await;

    let added = run(&api, &EMAIL_OFF, &["add", &address]).await;

    assert_eq!(added.status.code(), Some(0));
    assert_eq!(superadmins_of(&api).await, [ada.user_id]);
}

#[tokio::test]
async fn superadmins_002_an_account_that_is_a_superadmin_already_stays_one() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let first = run(&api, &SENDS_EMAIL, &["add", &address]).await;
    assert_eq!(first.status.code(), Some(0));

    let again = run(&api, &SENDS_EMAIL, &["add", &address]).await;

    refused(
        &again,
        AREA,
        "the account that holds the address is a superadmin already: nothing changed",
        &[&address],
    );
    assert_eq!(superadmins_of(&api).await, [ada.user_id]);
    assert_eq!(entries(&api, "superadmin.added").await.0.len(), 1);
}

#[tokio::test]
async fn superadmins_003_remove_ends_the_account_s_being_a_superadmin() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    second_factors::add_app(&api, &ada).await;
    let added = run(&api, &SENDS_EMAIL, &["add", &address]).await;
    assert_eq!(added.status.code(), Some(0));
    let served = api.get(EVERY).bearer(&ada.access).send().await;
    assert_eq!(served.status, 200, "{}", served.json());

    let removed = run(&api, &SENDS_EMAIL, &["remove", &address.to_uppercase()]).await;
    let refused_now = api.get(EVERY).bearer(&ada.access).send().await;

    finished(
        &removed,
        AREA,
        "the account that holds the address is no superadmin any more",
        &[&address, &address.to_uppercase()],
    );
    assert!(superadmins_of(&api).await.is_empty());
    let user = &ada.user_id;
    let (recorded, with_client) = entries(&api, "superadmin.removed").await;
    assert_eq!(recorded, [format!("user {user} - - user {user} {{}}")]);
    assert_eq!(
        with_client, "0",
        "an operator's subcommand records no client"
    );
    assert_eq!(refused_now.status, 403);
    assert_eq!(refused_now.json()["code"], "request.permission_denied");
    assert_eq!(standing(&api, &ada).await, json!({ "superadmin": false }));
}

#[tokio::test]
async fn superadmins_003_remove_changes_nothing_for_an_account_that_is_no_superadmin() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    users::register(&api, &address).await;
    let grace = users::register(&api, &mailbox::address("grace")).await;
    superadmins::make(&api, &grace).await;
    let nobody = mailbox::address("nobody");

    let not_an_address = run(&api, &SENDS_EMAIL, &["remove", "ada"]).await;
    let no_account = run(&api, &SENDS_EMAIL, &["remove", &nobody]).await;
    let no_superadmin = run(&api, &SENDS_EMAIL, &["remove", &address]).await;

    refused(
        &not_an_address,
        AREA,
        "the address given isn't an email address, so no account holds it: nothing changed",
        &[],
    );
    refused(
        &no_account,
        AREA,
        "no account holds the address: nothing changed",
        &[&nobody],
    );
    refused(
        &no_superadmin,
        AREA,
        "the account that holds the address is no superadmin: nothing changed",
        &[&address],
    );
    assert_eq!(superadmins_of(&api).await, [grace.user_id]);
    assert!(entries(&api, "superadmin.removed").await.0.is_empty());
}

#[tokio::test]
async fn superadmins_004_a_superadmin_s_route_refuses_any_other_caller_and_records_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    // Neither owning a workspace nor an authenticator app makes a superadmin.
    workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;
    second_factors::add_app(&api, &ada).await;

    let response = api
        .get(EVERY)
        .bearer(&ada.access)
        .header("user-agent", "tests/1.0")
        .send()
        .await;

    assert_eq!(response.status, 403);
    assert_eq!(response.json()["code"], "request.permission_denied");
    let user = &ada.user_id;
    let (recorded, with_client) = entries(&api, "access.denied").await;
    assert_eq!(
        recorded,
        [format!(
            "user {user} {user} - deployment - {{\"access\": \"superadmin\"}}"
        )]
    );
    assert_eq!(with_client, "1", "the refusal records its request's client");
}

#[tokio::test]
async fn superadmins_004_a_refusal_whose_caller_s_account_went_meanwhile_is_recorded_nowhere() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    // The lock holds the route's read of whether its caller is a superadmin, once the request
    // is authenticated.
    let mut held = db
        .holding("LOCK TABLE superadmins IN ACCESS EXCLUSIVE MODE")
        .await;

    let refused = api.get(EVERY).bearer(&ada.access).send();
    let delete = async {
        queries_wait_on_locks(db, 1).await;
        // The account goes while the request waits, as a deletion that commits then takes it.
        execute_on(
            &mut held,
            &format!("DELETE FROM users WHERE id = '{}'", ada.user_id),
        )
        .await;
        execute_on(&mut held, "COMMIT").await;
    };
    let (refused, ()) = tokio::join!(refused, delete);

    // The caller is refused all the same, with no log left to hold the refusal.
    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "request.permission_denied");
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM audit_entries").await,
        ["0"]
    );
}

#[tokio::test]
async fn superadmins_004_every_route_of_a_superadmin_s_refuses_a_caller_who_is_none() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    second_factors::add_app(&api, &ada).await;
    let routes = superadmin_routes(&api);

    for (method, path) in &routes {
        let anonymous = api
            .request(method.clone(), path)
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await;
        let user = api
            .request(method.clone(), path)
            .bearer(&ada.access)
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await;

        assert_eq!(anonymous.status, 401, "{method} {path}");
        assert_eq!(anonymous.json()["code"], "request.unauthenticated");
        assert_eq!(user.status, 403, "{method} {path}");
        assert_eq!(user.json()["code"], "request.permission_denied");
    }
    assert!(!routes.is_empty(), "no route of a superadmin's");
    // Each refusal of the signed-in caller is recorded, and none of the anonymous one's.
    assert_eq!(
        entries(&api, "access.denied").await.0.len(),
        routes.len(),
        "{routes:?}"
    );
}

#[tokio::test]
async fn superadmins_005_a_superadmin_without_an_app_is_refused_and_nothing_records_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    superadmins::make(&api, &ada).await;

    let response = api.get(EVERY).bearer(&ada.access).send().await;

    assert_eq!(response.status, 403);
    assert_eq!(
        response.json()["code"],
        "request.second_factor_setup_required"
    );
    // A superadmin who must set an app up was refused no access, so nothing records it.
    assert!(entries(&api, "access.denied").await.0.is_empty());
}

#[tokio::test]
async fn superadmins_005_a_superadmin_is_served_once_they_have_set_an_app_up() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    superadmins::make(&api, &ada).await;
    second_factors::add_app(&api, &ada).await;

    let response = api.get(EVERY).bearer(&ada.access).send().await;

    assert_eq!(response.status, 200, "{}", response.json());
}

#[tokio::test]
async fn superadmins_005_every_route_of_a_superadmin_s_asks_a_superadmin_for_an_app() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    superadmins::make(&api, &ada).await;
    let routes = superadmin_routes(&api);

    for (method, path) in &routes {
        let response = api
            .request(method.clone(), path)
            .bearer(&ada.access)
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await;

        assert_eq!(response.status, 403, "{method} {path}");
        assert_eq!(
            response.json()["code"],
            "request.second_factor_setup_required",
            "{method} {path}"
        );
    }
    assert!(!routes.is_empty(), "no route of a superadmin's");
}

#[tokio::test]
async fn superadmins_005_a_deployment_that_requires_an_app_of_every_user_answers_the_same() {
    let api = Api::with(Options::default().set("SECOND_FACTOR_REQUIRED", true)).await;
    let ada = users::register(&api, "ada@example.com").await;
    superadmins::make(&api, &ada).await;

    let response = api.get(EVERY).bearer(&ada.access).send().await;

    assert_eq!(response.status, 403);
    assert_eq!(
        response.json()["code"],
        "request.second_factor_setup_required"
    );
}

#[tokio::test]
async fn superadmins_005_such_a_deployment_serves_a_superadmin_who_has_an_app() {
    let api = Api::with(Options::default().set("SECOND_FACTOR_REQUIRED", true)).await;
    let ada = users::register(&api, "ada@example.com").await;
    superadmins::make_with_app(&api, &ada).await;

    let response = api.get(EVERY).bearer(&ada.access).send().await;

    assert_eq!(response.status, 200, "{}", response.json());
}

#[tokio::test]
async fn superadmins_006_a_caller_reads_whether_they_are_a_superadmin() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let sam = users::register(&api, "sam@example.com").await;
    // A superadmin is one with an authenticator app, and without.
    superadmins::make_with_app(&api, &ada).await;
    superadmins::make(&api, &grace).await;

    let with_app = standing(&api, &ada).await;
    let without_app = standing(&api, &grace).await;
    let user = standing(&api, &sam).await;

    assert_eq!(with_app, json!({ "superadmin": true }));
    assert_eq!(without_app, json!({ "superadmin": true }));
    assert_eq!(user, json!({ "superadmin": false }));
    // Asking is no refused request.
    assert!(entries(&api, "access.denied").await.0.is_empty());
}

#[tokio::test]
async fn superadmins_006_a_deployment_that_requires_an_app_refuses_an_account_without_one() {
    let api = Api::with(Options::default().set("SECOND_FACTOR_REQUIRED", true)).await;
    let ada = users::register(&api, "ada@example.com").await;
    superadmins::make(&api, &ada).await;

    let response = api
        .get("/v1/users/me/superadmin")
        .bearer(&ada.access)
        .send()
        .await;

    // As any route of a user's refuses the account, superadmin or not (AUTH-070).
    assert_eq!(response.status, 403);
    assert_eq!(
        response.json()["code"],
        "request.second_factor_setup_required"
    );
}

#[tokio::test]
async fn superadmins_007_deleting_a_superadmin_s_account_ends_their_being_one() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    superadmins::make(&api, &ada).await;
    superadmins::make(&api, &grace).await;

    let deleted = api
        .request(Method::DELETE, "/v1/users/me")
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(deleted.status, 204);
    assert_eq!(superadmins_of(&api).await, [grace.user_id]);
}

#[tokio::test]
async fn superadmins_007_an_export_holds_when_its_user_became_a_superadmin() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    superadmins::make(&api, &ada).await;
    let since = reads(
        &api,
        r#"SELECT to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"')
        FROM superadmins"#,
    )
    .await
    .remove(0);

    let mut archives = Vec::new();
    for user in [&ada, &grace] {
        let export = built(&api, user).await;
        let archive = api
            .get(&format!("/v1/users/me/exports/{export}/content"))
            .bearer(&user.access)
            .send()
            .await;
        assert_eq!(archive.status, 200);
        archives.push(document(&archive.body, "superadmins/superadmins.json"));
    }

    assert_eq!(archives[0], [json!({ "created_at": since })]);
    assert!(archives[1].is_empty(), "{:?}", archives[1]);
}

#[tokio::test]
async fn workspaces_029_a_superadmin_lists_every_workspace_with_its_owner_and_member_count() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    // The superadmin belongs to no workspace.
    let sam = users::register(&api, "sam@example.com").await;
    superadmins::make_with_app(&api, &sam).await;
    let db = api.database();
    let acme = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    workspaces::join(db, &acme, &grace.user_id, "admin").await;
    let globex = workspaces::arrange(db, "Globex", &[(&grace.user_id, "owner")]).await;
    db.owner_executes(&format!(
        "UPDATE workspaces SET created_at = CASE id \
         WHEN '{acme}' THEN timestamptz '2026-11-01T10:00:00Z' \
         ELSE timestamptz '2026-11-01T11:00:00.5Z' END;
        UPDATE users SET name = 'Grace Hopper' WHERE id = '{}'",
        grace.user_id
    ))
    .await;

    let response = api.get(EVERY).bearer(&sam.access).send().await;

    assert_eq!(response.status, 200, "{}", response.json());
    // An item holds exactly this of its workspace, and so nothing of its content.
    assert_eq!(
        response.json(),
        json!({
            "items": [
                {
                    "id": globex,
                    "name": "Globex",
                    "created_at": "2026-11-01T11:00:00.500000Z",
                    "member_count": 1,
                    "owner": {
                        "user_id": grace.user_id,
                        "name": "Grace Hopper",
                        "email": "grace@example.com",
                    },
                },
                {
                    "id": acme,
                    "name": "Acme",
                    "created_at": "2026-11-01T10:00:00.000000Z",
                    "member_count": 2,
                    "owner": {
                        "user_id": ada.user_id,
                        "name": "Ada Lovelace",
                        "email": "ada@example.com",
                    },
                },
            ],
            "next_cursor": null,
        })
    );
}

#[tokio::test]
async fn workspaces_029_paging_returns_each_workspace_once_across_a_shared_creation_time() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    superadmins::make_with_app(&api, &ada).await;
    let db = api.database();
    let mut arranged = Vec::new();
    for name in ["A", "B", "C", "D", "E"] {
        arranged.push(workspaces::arrange(db, name, &[(&ada.user_id, "owner")]).await);
    }
    db.owner_executes("UPDATE workspaces SET created_at = '2026-11-01T10:00:00Z'")
        .await;

    let pages = pages(&api, &ada, &format!("{EVERY}?limit=2"), "id").await;

    // Among workspaces that share a creation time, the higher ID comes first.
    arranged.reverse();
    assert_eq!(
        pages,
        [
            arranged[..2].to_vec(),
            arranged[2..4].to_vec(),
            arranged[4..].to_vec()
        ]
    );
}

#[tokio::test]
async fn workspaces_029_a_search_pages_through_the_workspaces_whose_name_contains_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    superadmins::make_with_app(&api, &ada).await;
    let db = api.database();
    for name in ["Team one", "Other", "Team two", "TEAM three"] {
        workspaces::arrange(db, name, &[(&ada.user_id, "owner")]).await;
    }

    let pages = pages(&api, &ada, &format!("{EVERY}?search=team&limit=2"), "name").await;

    assert_eq!(pages, [vec!["TEAM three", "Team two"], vec!["Team one"]]);
}

#[tokio::test]
async fn workspaces_029_a_limit_a_cursor_and_a_search_that_are_none_are_refused_together() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    superadmins::make_with_app(&api, &ada).await;

    let response = api
        .get(&format!("{EVERY}?limit=0&cursor=x&search=%20"))
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 400);
    let body = response.json();
    assert_eq!(body["code"], "request.invalid_query");
    let refused: Vec<&str> = body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|error| error["field"].as_str().unwrap())
        .collect();
    assert_eq!(refused, ["/limit", "/cursor", "/search"]);
}
