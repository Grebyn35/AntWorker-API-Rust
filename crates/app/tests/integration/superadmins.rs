//! The deployment's superadmins: the operator's subcommand `superadmin`, what authorizes a
//! superadmin's routes, the list of every workspace, and a superadmin's creation and deletion of
//! one (the superadmins spec, SUPERADMINS-001 to SUPERADMINS-008, the workspaces spec's "A
//! superadmin's routes", WORKSPACES-029 to WORKSPACES-031, and ACCESS-011), with what a
//! subcommand shows its operator (OPS-03).

use http::Method;
use platform::telemetry::testing::collector::Collector;
use serde_json::{Value, json};

use crate::{
    accounts::exports::{built, document, download},
    support::{
        Api,
        api::reads,
        audit, counts, execute_on,
        lists::pages,
        mailbox,
        operator::{self, EMAIL_OFF, SENDS_EMAIL, finished, printed, refused},
        queries_wait_on_locks, second_factors, streams, superadmins,
        users::{self, SignedIn},
        workspaces,
    },
};

/// The subcommand, which a line of its log names as its area (OPS-03).
const AREA: &str = "superadmin";

/// The list of every workspace, a superadmin's route.
const EVERY: &str = "/v1/superadmin/workspaces";

/// A superadmin's deletion of `workspace`, as `caller`.
async fn delete(api: &Api, caller: &SignedIn, workspace: &str) -> crate::support::api::Response {
    api.request(Method::DELETE, &format!("{EVERY}/{workspace}"))
        .bearer(&caller.access)
        .send()
        .await
}

/// A workspace named Acme, which Ada owns, with Grace as an admin and Bob as a member, and Sam,
/// a superadmin with an authenticator app, who belongs to no workspace: the API, the four, and
/// the workspace's ID.
async fn acme_and_a_superadmin() -> (Api, [SignedIn; 4], String) {
    let (api, sam) = a_superadmin().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[
            (&ada.user_id, "owner"),
            (&grace.user_id, "admin"),
            (&bob.user_id, "member"),
        ],
    )
    .await;
    (api, [ada, grace, bob, sam], workspace)
}

/// The user of each superadmin, the oldest first.
async fn superadmins_of(api: &Api) -> Vec<String> {
    reads(api, "SELECT user_id::text FROM superadmins ORDER BY id").await
}

/// The log, actor, target, resource and details of each entry of `action`, the oldest first,
/// with `-` for no actor, no target or no resource ID, and how many of them record a client, an
/// address or a user agent. This part's file keeps its own copy (TEST-09).
async fn entries(api: &Api, action: &str) -> (Vec<String>, usize) {
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
    (entries, audit::with_a_client(api.database(), action).await)
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

/// A body that every route of a superadmin's that takes one parses, so a request with it is
/// refused for its caller alone.
const BODY: &str = r#"{"name": "Acme"}"#;

/// Creates a workspace named `name` as `caller`, a superadmin, with `key` as its idempotency
/// key if given.
async fn create(
    api: &Api,
    caller: &SignedIn,
    name: &str,
    key: Option<&str>,
) -> crate::support::api::Response {
    let mut call = api
        .post(EVERY, &json!({ "name": name }))
        .bearer(&caller.access);
    if let Some(key) = key {
        call = call.header("idempotency-key", key);
    }
    call.send().await
}

/// A superadmin with an authenticator app, who belongs to no workspace, and the API.
async fn a_superadmin() -> (Api, SignedIn) {
    let api = Api::new().await;
    let sam = users::register(&api, "sam@example.com").await;
    superadmins::make_with_app(&api, &sam).await;
    (api, sam)
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

    let added = operator::run(&api, AREA, &SENDS_EMAIL, &["add", &address.to_uppercase()]).await;

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
    assert_eq!(with_client, 0, "an operator's subcommand records no client");
    assert_eq!(standing(&api, &ada).await, json!({ "superadmin": true }));
    assert_eq!(standing(&api, &grace).await, json!({ "superadmin": false }));
}

#[tokio::test]
async fn superadmins_002_add_changes_nothing_for_what_names_no_account() {
    let api = Api::new().await;
    users::register(&api, &mailbox::address("ada")).await;
    let nobody = mailbox::address("nobody");

    let not_an_address = operator::run(&api, AREA, &SENDS_EMAIL, &["add", "ada"]).await;
    let no_account = operator::run(&api, AREA, &SENDS_EMAIL, &["add", &nobody]).await;

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

    let unproved = operator::run(&api, AREA, &SENDS_EMAIL, &["add", &address]).await;

    refused(
        &unproved,
        AREA,
        "the account that holds the address hasn't proved it, so the address may not be the \
         account's user's: nothing changed; run it again once the account has proved the address",
        &[&address],
    );
    assert!(superadmins_of(&api).await.is_empty());
    assert!(entries(&api, "superadmin.added").await.0.is_empty());
}

#[tokio::test]
async fn superadmins_002_with_email_off_an_account_that_never_proved_its_address_becomes_one() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    // No code proves an address with email off, so the account never proved its own.
    let ada = users::register_unverified(&api, &address).await;

    let added = operator::run(&api, AREA, &EMAIL_OFF, &["add", &address]).await;

    assert_eq!(added.status.code(), Some(0));
    assert_eq!(superadmins_of(&api).await, [ada.user_id]);
}

#[tokio::test]
async fn superadmins_002_with_email_off_an_account_that_proved_its_address_becomes_one() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    // The account proved its address while the deployment sent email.
    let ada = users::register(&api, &address).await;

    let added = operator::run(&api, AREA, &EMAIL_OFF, &["add", &address]).await;

    assert_eq!(added.status.code(), Some(0));
    assert_eq!(superadmins_of(&api).await, [ada.user_id]);
}

#[tokio::test]
async fn superadmins_002_an_account_that_is_a_superadmin_already_stays_one() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let first = operator::run(&api, AREA, &SENDS_EMAIL, &["add", &address]).await;
    assert_eq!(first.status.code(), Some(0));

    let again = operator::run(&api, AREA, &SENDS_EMAIL, &["add", &address]).await;

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
    let added = operator::run(&api, AREA, &SENDS_EMAIL, &["add", &address]).await;
    assert_eq!(added.status.code(), Some(0));
    let served = api.get(EVERY).bearer(&ada.access).send().await;
    assert_eq!(served.status, 200, "{}", served.json());

    let removed = operator::run(
        &api,
        AREA,
        &SENDS_EMAIL,
        &["remove", &address.to_uppercase()],
    )
    .await;
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
    assert_eq!(with_client, 0, "an operator's subcommand records no client");
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

    let not_an_address = operator::run(&api, AREA, &SENDS_EMAIL, &["remove", "ada"]).await;
    let no_account = operator::run(&api, AREA, &SENDS_EMAIL, &["remove", &nobody]).await;
    let no_superadmin = operator::run(&api, AREA, &SENDS_EMAIL, &["remove", &address]).await;

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
    assert_eq!(with_client, 1, "the refusal records its request's client");
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
            .body(BODY)
            .send()
            .await;
        let user = api
            .request(method.clone(), path)
            .bearer(&ada.access)
            .header("content-type", "application/json")
            .body(BODY)
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
async fn superadmins_004_each_refusal_counts_as_one_for_a_permission() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    // A `serve` of the test's own, which exports to the stand-in collector, so that no other
    // test's refusals reach what it counts (TEST-07).
    let collector = Collector::start().await.unwrap();
    let endpoint = collector.endpoint();
    let server = db
        .serve(&[
            ("ENVIRONMENT", "test"),
            ("OTEL_EXPORTER_OTLP_ENDPOINT", endpoint.as_str()),
        ])
        .await;

    let mut statuses = Vec::new();
    for caller in [&ada, &grace] {
        let bearer = format!("Bearer {}", caller.access);
        let headers = [("authorization", bearer.as_str())];
        statuses.push(server.send("GET", EVERY, &headers, b"").await.0);
    }
    server.stop().await;

    assert_eq!(statuses, [403, 403]);
    assert_eq!(
        counts::of(
            &collector.received(),
            "platform.authorization_refusals",
            "authorization.reason"
        ),
        ["permission 2"]
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
            .body(BODY)
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
    let api = second_factors::requiring().await;
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
    let api = second_factors::requiring().await;
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
    let api = second_factors::requiring().await;
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

    let deleted = users::delete(&api, &ada).await;

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
        let archive = download(&api, user, &export, &[]).await;
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

    let refused: Vec<String> = response
        .refused_parameters()
        .into_iter()
        .map(|(field, _)| field)
        .collect();
    assert_eq!(refused, ["/limit", "/cursor", "/search"]);
}

#[tokio::test]
async fn superadmins_008_list_shows_each_superadmin_s_address_and_logs_none() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    users::register(&api, "sam@example.com").await;
    // Grace became a superadmin before Ada did.
    superadmins::make(&api, &ada).await;
    superadmins::make(&api, &grace).await;
    api.database()
        .owner_executes(&format!(
            "UPDATE superadmins SET created_at = CASE user_id \
             WHEN '{}' THEN timestamptz '2026-11-01T10:00:00Z' \
             ELSE timestamptz '2026-11-01T11:00:00.5Z' END",
            grace.user_id
        ))
        .await;
    let before = reads(&api, "SELECT count(*)::text FROM audit_entries").await;

    let listed = operator::run(&api, AREA, &SENDS_EMAIL, &["list"]).await;

    assert_eq!(listed.status.code(), Some(0), "{}", printed(&listed));
    // The standard output holds the list, the one made first first.
    let stdout = String::from_utf8_lossy(&listed.stdout).into_owned();
    let shown: Vec<&str> = stdout
        .lines()
        .filter(|line| line.contains("superadmins, with") || line.contains("@example.com"))
        .collect();
    assert_eq!(
        shown,
        [
            "The deployment's superadmins, with the ID of each one's user:".to_owned(),
            format!(
                "  grace@example.com  {}  since 2026-11-01T10:00:00.000000Z",
                grace.user_id
            ),
            format!(
                "  ada@example.com  {}  since 2026-11-01T11:00:00.500000Z",
                ada.user_id
            ),
        ],
        "{stdout}"
    );
    // The log says how many it listed, and holds no address (OPS-03, OBS-04).
    let written = printed(&listed);
    let logged: Vec<&str> = written
        .lines()
        .filter(|line| line.contains("subcommand finished"))
        .collect();
    assert_eq!(logged.len(), 1, "{written}");
    assert!(logged[0].contains(AREA), "{written}");
    assert!(
        logged[0].contains("listed the deployment's superadmins: 2"),
        "{written}"
    );
    assert!(!logged[0].contains("@example.com"), "{written}");
    // It changes nothing and records nothing.
    assert_eq!(superadmins_of(&api).await.len(), 2);
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM audit_entries").await,
        before
    );
}

#[tokio::test]
async fn superadmins_008_list_says_that_a_deployment_has_no_superadmin() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;

    let listed = operator::run(&api, AREA, &EMAIL_OFF, &["list"]).await;

    finished(
        &listed,
        AREA,
        "listed the deployment's superadmins: 0",
        &["@example.com"],
    );
    let stdout = String::from_utf8_lossy(&listed.stdout).into_owned();
    assert!(
        stdout
            .lines()
            .any(|line| line == "The deployment has no superadmin."),
        "{stdout}"
    );
}

#[tokio::test]
async fn workspaces_030_a_superadmin_deletes_any_workspace_with_everything_in_it() {
    let (api, [ada, grace, bob, sam], workspace) = acme_and_a_superadmin().await;
    let db = api.database();
    let other = workspaces::arrange(db, "Other", &[(&ada.user_id, "owner")]).await;
    // Each workspace has an entry in its log, an event and a job enqueued for it.
    for id in [&workspace, &other] {
        workspaces::fill(db, id, &ada.user_id).await;
    }
    let mut signals = db.signals().await;

    let deleted = api
        .request(Method::DELETE, &format!("{EVERY}/{workspace}"))
        .bearer(&sam.access)
        .header("user-agent", "tests/1.0")
        .send()
        .await;

    assert_eq!(deleted.status, 204);
    assert!(deleted.body.is_empty());
    assert_eq!(workspaces::held_by(db, &workspace).await, "0 0 0 0 0");
    assert_eq!(workspaces::held_by(db, &other).await, "1 1 1 1 1");
    // The superadmin's own log keeps the deletion, which says that a superadmin made it.
    let user = &sam.user_id;
    let (recorded, with_client) = entries(&api, "workspace.deleted").await;
    assert_eq!(
        recorded,
        [format!(
            "user {user} {user} - workspace {workspace} {{\"access\": \"superadmin\"}}"
        )]
    );
    assert_eq!(with_client, 1, "the entry records its request's client");
    // Each member's signal names the deletion's transaction: the owner's first, then each rank's.
    let received = signals.received().await;
    let transaction = received[0]["transaction"].as_u64().unwrap();
    let expected = [&ada, &grace, &bob]
        .map(|member| workspaces::signal(&member.user_id, &workspace, transaction));
    assert_eq!(received, expected);
}

#[tokio::test]
async fn workspaces_030_the_deletion_closes_each_member_s_streams() {
    let (api, [ada, grace, _, sam], workspace) = acme_and_a_superadmin().await;
    let mut owner = streams::opened(&api, &ada).await;
    let mut admin = streams::opened(&api, &grace).await;

    let deleted = delete(&api, &sam, &workspace).await;
    let (_, owner_reason) = owner.closed().await;
    let (_, admin_reason) = admin.closed().await;

    assert_eq!(deleted.status, 204);
    assert_eq!(owner_reason, "access_changed");
    assert_eq!(admin_reason, "access_changed");
}

#[tokio::test]
async fn workspaces_030_a_superadmin_who_owns_the_workspace_deletes_it_the_same_way() {
    let (api, sam) = a_superadmin().await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&sam.user_id, "owner")]).await;

    let deleted = delete(&api, &sam, &workspace).await;

    assert_eq!(deleted.status, 204);
    assert_eq!(workspaces::held_by(db, &workspace).await, "0 0 0 0 0");
    let user = &sam.user_id;
    assert_eq!(
        entries(&api, "workspace.deleted").await.0,
        [format!(
            "user {user} {user} - workspace {workspace} {{\"access\": \"superadmin\"}}"
        )]
    );
}

#[tokio::test]
async fn workspaces_030_the_deletion_needs_a_proof_from_the_last_5_minutes() {
    let (api, [.., sam], workspace) = acme_and_a_superadmin().await;
    let db = api.database();
    users::authenticated_ago(&api, &sam, 6).await;

    let unproved = delete(&api, &sam, &workspace).await;
    // A path that names no workspace asks for the proof all the same.
    let unproved_elsewhere = delete(&api, &sam, "nowhere").await;
    users::authenticated_ago(&api, &sam, 4).await;
    let proved = delete(&api, &sam, &workspace).await;

    for refused in [&unproved, &unproved_elsewhere] {
        assert_eq!(refused.status, 403);
        assert_eq!(refused.json()["code"], "request.reauthentication_required");
    }
    assert_eq!(proved.status, 204);
    assert_eq!(workspaces::held_by(db, &workspace).await, "0 0 0 0 0");
    assert_eq!(entries(&api, "workspace.deleted").await.0.len(), 1);
}

#[tokio::test]
async fn workspaces_030_a_path_that_names_no_workspace_answers_404_and_records_nothing() {
    let (api, [.., sam], workspace) = acme_and_a_superadmin().await;
    // The ID of a row that is no workspace's.
    let no_workspace = &sam.user_id;

    let unknown = delete(&api, &sam, no_workspace).await;
    let malformed = delete(&api, &sam, "acme").await;

    for missing in [&unknown, &malformed] {
        assert_eq!(missing.status, 404);
        assert_eq!(missing.json()["code"], "workspaces.not_found");
    }
    assert_eq!(
        workspaces::held_by(api.database(), &workspace).await,
        "1 3 0 0 0"
    );
    assert!(entries(&api, "workspace.deleted").await.0.is_empty());
}

#[tokio::test]
async fn access_011_a_deletion_that_waited_for_a_transfer_ends_the_new_owner_s_membership() {
    let (api, [ada, grace, bob, sam], workspace) = acme_and_a_superadmin().await;
    let db = api.database();
    // A transfer of Ada's, which holds her membership, the owner's, from its authorization on.
    let mut transfer = db
        .holding(&format!(
            "SELECT 1 FROM workspace_memberships \
             WHERE workspace_id = '{workspace}' AND user_id = '{}' FOR UPDATE",
            ada.user_id
        ))
        .await;
    let mut signals = db.signals().await;

    let deletion = api.spawn(
        Method::DELETE,
        &format!("{EVERY}/{workspace}"),
        &sam.access,
        None,
    );
    queries_wait_on_locks(db, 1).await;
    // The transfer makes Grace the owner while the deletion waits at Ada's membership, and holds
    // nothing else of the workspace.
    for (user, role) in [(&ada, "admin"), (&grace, "owner")] {
        execute_on(
            &mut transfer,
            &format!(
                "UPDATE workspace_memberships SET role = '{role}' \
                 WHERE workspace_id = '{workspace}' AND user_id = '{}'",
                user.user_id
            ),
        )
        .await;
    }
    execute_on(&mut transfer, "COMMIT").await;

    assert_eq!(deletion.await.unwrap().0, 204);
    assert_eq!(workspaces::held_by(db, &workspace).await, "0 0 0 0 0");
    // The deletion ended each membership itself, the new owner's first, each with its signal:
    // none went unsignalled with the workspace's row.
    let received = signals.received().await;
    let transaction = received[0]["transaction"].as_u64().unwrap();
    let expected = [&grace, &ada, &bob]
        .map(|member| workspaces::signal(&member.user_id, &workspace, transaction));
    assert_eq!(received, expected);
}

#[tokio::test]
async fn access_011_a_deletion_that_waited_for_the_owner_s_own_finds_no_workspace() {
    let (api, [ada, .., sam], workspace) = acme_and_a_superadmin().await;
    let db = api.database();
    // A deletion of Ada's, the owner's, which holds her membership from its authorization on.
    let mut owner_s = db
        .holding(&format!(
            "SELECT 1 FROM workspace_memberships \
             WHERE workspace_id = '{workspace}' AND user_id = '{}' FOR UPDATE",
            ada.user_id
        ))
        .await;

    let deletion = api.spawn(
        Method::DELETE,
        &format!("{EVERY}/{workspace}"),
        &sam.access,
        None,
    );
    queries_wait_on_locks(db, 1).await;
    execute_on(
        &mut owner_s,
        &format!("DELETE FROM workspaces WHERE id = '{workspace}'"),
    )
    .await;
    execute_on(&mut owner_s, "COMMIT").await;

    let (status, problem) = deletion.await.unwrap();
    assert_eq!(status, 404);
    assert_eq!(problem["code"], "workspaces.not_found");
    // The superadmin deleted nothing, so their log records nothing.
    assert!(entries(&api, "workspace.deleted").await.0.is_empty());
}

#[tokio::test]
async fn workspaces_031_a_superadmin_creates_a_workspace_that_they_own() {
    let (api, sam) = a_superadmin().await;
    let db = api.database();
    // The registration's session published its start (AUTH-046).
    db.owner_executes("DELETE FROM events").await;

    let response = api
        .post(EVERY, &json!({ "name": "Acme" }))
        .bearer(&sam.access)
        .header("user-agent", "tests/1.0")
        .send()
        .await;

    assert_eq!(response.status, 201, "{}", response.json());
    let body = response.json();
    let id = body["id"].as_str().unwrap();
    assert_eq!(response.header("location"), format!("/v1/workspaces/{id}"));
    assert_eq!(response.header("etag"), "\"1\"");
    assert_eq!(body["name"], "Acme");
    assert_eq!(body["role"], "owner");
    assert!(body["created_at"].as_str().unwrap().ends_with('Z'));
    let user = &sam.user_id;
    let members = db
        .owner_reads("SELECT user_id || ' ' || role FROM workspace_memberships")
        .await;
    assert_eq!(members, [format!("{user} owner")]);
    // The workspace's log holds its creation, and so does the superadmin's own, which says that
    // a superadmin made it.
    let (recorded, with_client) = entries(&api, "workspace.created").await;
    assert_eq!(
        recorded,
        [
            format!("workspace {id} {user} - workspace {id} {{}}"),
            format!("user {user} {user} - workspace {id} {{\"access\": \"superadmin\"}}"),
        ]
    );
    // The workspace's entry is read with `workspace.read`, and the superadmin's own needs none.
    assert_eq!(
        reads(
            &api,
            "SELECT coalesce(permission, '-') FROM audit_entries \
             WHERE action = 'workspace.created' ORDER BY id"
        )
        .await,
        ["workspace.read", "-"]
    );
    assert_eq!(with_client, 2, "each entry records its request's client");
    let events = db
        .owner_reads(
            "SELECT name || ' ' || scope || ' ' || user_id || ' ' || (payload->>'workspace_id') \
             || ' ' || (payload->>'session_id') FROM events",
        )
        .await;
    assert_eq!(
        events,
        [format!(
            "membership.created user {user} {id} {}",
            sam.session_id
        )]
    );
}

#[tokio::test]
async fn workspaces_031_a_name_is_stored_normalized_and_must_hold_a_character() {
    let (api, sam) = a_superadmin().await;

    let created = create(&api, &sam, "  Acme \t  Labs ", None).await;
    let refused = create(&api, &sam, " \t ", None).await;

    assert_eq!(created.json()["name"], "Acme Labs");
    assert_eq!(refused.status, 400);
    assert_eq!(refused.json()["code"], "request.invalid_fields");
    assert_eq!(refused.json()["errors"][0]["field"], "/name");
    assert_eq!(refused.json()["errors"][0]["code"], "required");
    assert_eq!(
        reads(&api, "SELECT name FROM workspaces").await,
        ["Acme Labs"]
    );
}

#[tokio::test]
async fn workspaces_031_a_retry_with_the_key_creates_one_workspace() {
    let (api, sam) = a_superadmin().await;

    let first = create(&api, &sam, "Acme", Some("\"k-1\"")).await;
    let retry = create(&api, &sam, "Acme", Some("\"k-1\"")).await;

    assert_eq!(first.status, 201);
    assert_eq!(retry.status, 201);
    assert_eq!(retry.json(), first.json());
    assert_eq!(retry.header("location"), first.header("location"));
    assert_eq!(retry.header("etag"), first.header("etag"));
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM workspaces").await,
        ["1"]
    );
    assert_eq!(entries(&api, "workspace.created").await.0.len(), 2);
}

#[tokio::test]
async fn workspaces_031_the_created_workspace_s_location_reads_it_as_its_owner() {
    let (api, sam) = a_superadmin().await;
    let created = create(&api, &sam, "Acme", None).await;

    let read = api
        .get(created.header("location"))
        .bearer(&sam.access)
        .send()
        .await;

    assert_eq!(read.status, 200);
    assert_eq!(read.json(), created.json());
    assert_eq!(read.header("etag"), created.header("etag"));
}

#[tokio::test]
async fn access_011_a_deletion_that_waited_for_the_deletion_of_the_owner_s_account_finds_none() {
    let (api, sam) = a_superadmin().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    // The lock of Ada's identity, as a sign-in of hers holds it: the deletion of her account
    // waits at it, once it has locked her memberships and ended them.
    let mut held = users::holding_identities(db, &ada).await;

    let account = users::deleting(&api, &ada);
    queries_wait_on_locks(db, 1).await;
    let deletion = api.spawn(
        Method::DELETE,
        &format!("{EVERY}/{workspace}"),
        &sam.access,
        None,
    );
    // The superadmin's deletion waits at the owner's membership, which her account's holds.
    queries_wait_on_locks(db, 2).await;
    execute_on(&mut held, "ROLLBACK").await;

    assert_eq!(account.await.unwrap().status, 204);
    let (status, problem) = deletion.await.unwrap();
    assert_eq!(status, 404);
    assert_eq!(problem["code"], "workspaces.not_found");
    assert!(entries(&api, "workspace.deleted").await.0.is_empty());
}

#[tokio::test]
async fn ops_03_what_a_subcommand_shows_its_operator_is_printed_outside_the_logs() {
    let api = Api::without_email().await;
    let ada = users::register(&api, "ada@example.com").await;
    superadmins::make(&api, &ada).await;
    let settings: Vec<(&str, &str)> = EMAIL_OFF
        .into_iter()
        .chain([("ENVIRONMENT", "production")])
        .collect();

    let listed = operator::run(&api, AREA, &settings, &["list"]).await;

    assert_eq!(listed.status.code(), Some(0), "{}", printed(&listed));
    // Production logs JSON lines (OBS-01): what the subcommand shows is no line of them,
    // and no log line holds what it shows.
    let stdout = String::from_utf8_lossy(&listed.stdout).into_owned();
    let (logged, shown): (Vec<&str>, Vec<&str>) = stdout
        .lines()
        .partition(|line| serde_json::from_str::<Value>(line).is_ok());
    assert_eq!(shown.len(), 2, "{stdout}");
    assert_eq!(
        shown[0],
        "The deployment's superadmins, with the ID of each one's user:"
    );
    assert!(shown[1].starts_with("  ada@example.com  "), "{stdout}");
    let finished: Vec<Value> = logged
        .iter()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line["message"] == "subcommand finished")
        .collect();
    assert_eq!(finished.len(), 1, "{stdout}");
    assert_eq!(finished[0]["startup.area"], AREA);
    assert_eq!(
        finished[0]["startup.outcome"],
        "listed the deployment's superadmins: 1"
    );
    assert!(
        logged.iter().all(|line| !line.contains("ada@example.com")),
        "{stdout}"
    );
}
