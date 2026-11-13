//! What a workspace member may do (the access spec), and the authorization that `platform`
//! builds on it (the platform spec's "Units of work and authorization").

use futures_util::future::BoxFuture;
use http::{Method, StatusCode};

use crate::support::{
    Api, TestDatabase,
    api::Response,
    execute_on, queries_wait_on_locks,
    users::{self, SignedIn},
    workspaces,
};

/// Sends `method` to `path` as `caller`.
async fn call(
    api: &Api,
    method: Method,
    path: &str,
    caller: &SignedIn,
) -> crate::support::api::Response {
    api.request(method, path)
        .bearer(&caller.access)
        .send()
        .await
}

#[tokio::test]
async fn access_001_a_workspace_has_exactly_one_owner() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;

    let second = db
        .try_execute(
            &db.owner_url(),
            &format!(
                "INSERT INTO workspace_memberships (workspace_id, user_id, role) \
                 VALUES ('{workspace}', '{}', 'owner')",
                grace.user_id
            ),
        )
        .await;

    assert!(second.is_err_and(|error| error.contains("workspace_memberships_workspace_id_key")));
}

#[tokio::test]
async fn access_003_a_non_member_gets_404_and_a_member_without_the_permission_403() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let member = users::register(&api, "member@example.com").await;
    let stranger = users::register(&api, "stranger@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&owner.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &member.user_id, "member").await;
    let members = format!("/v1/workspaces/{workspace}/members");
    let remove = format!("{members}/{}", owner.user_id);

    let allowed = call(&api, Method::GET, &members, &member).await;
    let stranger_reads = call(&api, Method::GET, &members, &stranger).await;
    let member_removes = call(&api, Method::DELETE, &remove, &member).await;
    let unknown = call(
        &api,
        Method::GET,
        "/v1/workspaces/01900000-0000-7000-8000-000000000000/members",
        &owner,
    )
    .await;

    assert_eq!(allowed.status, 200);
    for not_found in [&stranger_reads, &unknown] {
        assert_eq!(not_found.status, 404);
        assert_eq!(not_found.json()["code"], "request.not_found");
    }
    assert_eq!(member_removes.status, 403);
    assert_eq!(member_removes.json()["code"], "request.permission_denied");
    // Each refusal is reported: the stranger's, the member's, and the one of a workspace that
    // doesn't exist.
    let reported = api
        .database()
        .owner_reads(
            "SELECT action || ' ' || resource_type FROM audit_entries \
             WHERE action = 'access.denied' ORDER BY created_at, id",
        )
        .await;
    assert_eq!(reported, ["access.denied workspace"; 3]);
}

#[tokio::test]
async fn audit_004_a_refusal_is_recorded_in_the_member_s_workspace_or_the_caller_s_own_log() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let member = users::register(&api, "member@example.com").await;
    let stranger = users::register(&api, "stranger@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&owner.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &member.user_id, "member").await;
    let members = format!("/v1/workspaces/{workspace}/members");

    call(&api, Method::GET, &members, &stranger).await;
    call(
        &api,
        Method::DELETE,
        &format!("{members}/{}", owner.user_id),
        &member,
    )
    .await;

    let entries = db
        .owner_reads(
            "SELECT scope || ' ' || coalesce(user_id::text, workspace_id::text) || ' ' || \
             actor_user_id || ' ' || resource_type || ' ' || resource_id || ' ' || details::text \
             FROM audit_entries WHERE action = 'access.denied' ORDER BY created_at, id",
        )
        .await;
    assert_eq!(
        entries,
        [
            format!(
                "user {0} {0} workspace {workspace} {{\"permission\": \"members.read\"}}",
                stranger.user_id
            ),
            format!(
                "workspace {workspace} {} workspace {workspace} {{\"permission\": \"members.remove\"}}",
                member.user_id
            ),
        ]
    );
}

#[tokio::test]
async fn audit_003_a_refusal_records_the_client_and_the_trace_of_its_request() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let stranger = users::register(&api, "stranger@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&owner.user_id, "owner")]).await;

    let response = api
        .get(&format!("/v1/workspaces/{workspace}/members"))
        .bearer(&stranger.access)
        .header("user-agent", "Desktop/1.2")
        .send()
        .await;

    let trace_id = response.header("server-timing").split('-').nth(1).unwrap();
    let entry = api
        .database()
        .owner_reads(
            "SELECT host(client.client_ip) || ' ' || client.user_agent || ' ' || entry.trace_id \
             FROM audit_entries entry JOIN audit_entry_clients client ON client.entry_id = entry.id \
             WHERE entry.action = 'access.denied'",
        )
        .await;
    assert_eq!(entry, [format!("198.51.100.20 Desktop/1.2 {trace_id}")]);
}

#[tokio::test]
async fn access_003_a_workspace_id_that_isn_t_a_uuid_answers_404_and_records_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = call(&api, Method::GET, "/v1/workspaces/acme/members", &ada).await;

    assert_eq!(response.status, StatusCode::NOT_FOUND);
    assert_eq!(response.json()["code"], "request.not_found");
    let refusals = api
        .database()
        .owner_reads("SELECT count(*)::text FROM audit_entries WHERE action = 'access.denied'")
        .await;
    assert_eq!(refusals, ["0"]);
}

#[tokio::test]
async fn data_20_a_removal_waits_on_no_member_it_may_not_remove() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let first = users::register(&api, "first@example.com").await;
    let second = users::register(&api, "second@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&owner.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &first.user_id, "admin").await;
    workspaces::join(db, &workspace, &second.user_id, "admin").await;
    // The second admin's own request, such as removing the first, holds their row as
    // `authorize` does.
    let mut held = db.owner_connection().await;
    execute_on(&mut held, "BEGIN").await;
    execute_on(
        &mut held,
        &format!(
            "SELECT 1 FROM workspace_memberships \
             WHERE workspace_id = '{workspace}' AND user_id = '{}' FOR SHARE",
            second.user_id
        ),
    )
    .await;

    let refused = call(
        &api,
        Method::DELETE,
        &format!("/v1/workspaces/{workspace}/members/{}", second.user_id),
        &first,
    )
    .await;
    execute_on(&mut held, "ROLLBACK").await;

    // Refused at once, without waiting on the row it may not change, so two admins removing
    // each other can't deadlock.
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn access_004_a_member_acts_only_on_members_below_their_rank() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let admin = users::register(&api, "admin@example.com").await;
    let member = users::register(&api, "member@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&owner.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &admin.user_id, "admin").await;
    workspaces::join(db, &workspace, &member.user_id, "member").await;
    let remove = |target: &str| format!("/v1/workspaces/{workspace}/members/{target}");

    let up = call(&api, Method::DELETE, &remove(&owner.user_id), &admin).await;
    let down = call(&api, Method::DELETE, &remove(&member.user_id), &admin).await;

    assert_eq!(up.status, StatusCode::FORBIDDEN);
    assert_eq!(up.json()["code"], "request.permission_denied");
    assert_eq!(down.status, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn platform_010_a_rank_refusal_records_itself_and_rolls_the_operation_back() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let admin = users::register(&api, "admin@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&owner.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &admin.user_id, "admin").await;

    let refused = call(
        &api,
        Method::DELETE,
        &format!("/v1/workspaces/{workspace}/members/{}", owner.user_id),
        &admin,
    )
    .await;

    assert_eq!(refused.json()["code"], "request.permission_denied");
    let members = db
        .owner_reads("SELECT count(*)::text FROM workspace_memberships")
        .await;
    assert_eq!(members, ["2"]);
    let actions = db
        .owner_reads("SELECT action FROM audit_entries WHERE workspace_id IS NOT NULL")
        .await;
    assert_eq!(actions, ["access.denied"]);
}

#[tokio::test]
async fn platform_026_an_operation_that_rolls_back_publishes_nothing() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let admin = users::register(&api, "admin@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&owner.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &admin.user_id, "admin").await;
    // The registrations' sessions published their starts (AUTH-046).
    db.owner_executes("DELETE FROM events").await;

    call(
        &api,
        Method::DELETE,
        &format!("/v1/workspaces/{workspace}/members/{}", owner.user_id),
        &admin,
    )
    .await;

    let events = db.owner_reads("SELECT name FROM events").await;
    assert!(events.is_empty(), "{events:?}");
}

#[tokio::test]
async fn platform_009_authorization_waits_for_a_change_to_the_caller_s_membership() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let member = users::register(&api, "member@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&owner.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &member.user_id, "member").await;
    let mut removal = db.owner_connection().await;
    execute_on(&mut removal, "BEGIN").await;
    execute_on(
        &mut removal,
        &format!(
            "DELETE FROM workspace_memberships WHERE workspace_id = '{workspace}' AND user_id = '{}'",
            member.user_id
        ),
    )
    .await;

    let request = tokio::spawn({
        let api = api.clone();
        let path = format!("/v1/workspaces/{workspace}/members");
        async move { call(&api, Method::GET, &path, &member).await.status }
    });
    queries_wait_on_locks(db, 1).await;
    execute_on(&mut removal, "COMMIT").await;

    // The request waited for the removal, so it ran under the membership as it then stood.
    assert_eq!(request.await.unwrap(), StatusCode::NOT_FOUND);
}

/// Arranges the row a route addresses in the workspace it is given, through the `owner`
/// connection, and gives its ID: a builder of the test support (TEST-09).
type Arrange = for<'a> fn(&'a TestDatabase, &'a str) -> BoxFuture<'a, String>;

/// How the generated test calls a workspace route, which its method and path name: the path,
/// given the workspace and the ID the route addresses, its body and that body's media type, if it
/// takes one, and the status of a success. The ID is a target member's, below the caller's rank,
/// unless `row` arranges the row the route addresses; a body names it as `{target}`.
struct Fixture {
    method: &'static str,
    template: &'static str,
    path: fn(&str, &str) -> String,
    row: Option<Arrange>,
    body: Option<(&'static str, &'static str)>,
    headers: &'static [(&'static str, &'static str)],
    success: StatusCode,
}

/// The fixture of each workspace route. A route without one fails the test, so every new
/// workspace route joins it (TEST-08), each module's after the others.
const FIXTURES: &[Fixture] = &[
    Fixture {
        method: "get",
        template: "/v1/workspaces/{workspace_id}",
        path: |workspace, _| format!("/v1/workspaces/{workspace}"),
        row: None,
        body: None,
        headers: &[],
        success: StatusCode::OK,
    },
    Fixture {
        method: "patch",
        template: "/v1/workspaces/{workspace_id}",
        path: |workspace, _| format!("/v1/workspaces/{workspace}"),
        row: None,
        body: Some((r#"{"name": "Renamed"}"#, "application/merge-patch+json")),
        headers: &[],
        success: StatusCode::OK,
    },
    Fixture {
        method: "delete",
        template: "/v1/workspaces/{workspace_id}",
        path: |workspace, _| format!("/v1/workspaces/{workspace}"),
        row: None,
        body: None,
        headers: &[],
        success: StatusCode::NO_CONTENT,
    },
    // The owner's target is an admin, the one role a transfer goes to (SEC-09).
    Fixture {
        method: "post",
        template: "/v1/workspaces/{workspace_id}/transfer",
        path: |workspace, _| format!("/v1/workspaces/{workspace}/transfer"),
        row: None,
        body: Some((r#"{"user_id": "{target}"}"#, "application/json")),
        headers: &[],
        success: StatusCode::NO_CONTENT,
    },
    Fixture {
        method: "post",
        template: "/v1/workspaces/{workspace_id}/leave",
        path: |workspace, _| format!("/v1/workspaces/{workspace}/leave"),
        row: None,
        body: None,
        headers: &[],
        success: StatusCode::NO_CONTENT,
    },
    Fixture {
        method: "get",
        template: "/v1/workspaces/{workspace_id}/members",
        path: |workspace, _| format!("/v1/workspaces/{workspace}/members"),
        row: None,
        body: None,
        headers: &[],
        success: StatusCode::OK,
    },
    Fixture {
        method: "get",
        template: "/v1/workspaces/{workspace_id}/members/{user_id}",
        path: |workspace, target| format!("/v1/workspaces/{workspace}/members/{target}"),
        row: None,
        body: None,
        headers: &[],
        success: StatusCode::OK,
    },
    // A member is the one role that every role allowed to change roles may assign (ACCESS-004).
    Fixture {
        method: "patch",
        template: "/v1/workspaces/{workspace_id}/members/{user_id}",
        path: |workspace, target| format!("/v1/workspaces/{workspace}/members/{target}"),
        row: None,
        body: Some((r#"{"role": "member"}"#, "application/merge-patch+json")),
        headers: &[],
        success: StatusCode::OK,
    },
    Fixture {
        method: "delete",
        template: "/v1/workspaces/{workspace_id}/members/{user_id}",
        path: |workspace, target| format!("/v1/workspaces/{workspace}/members/{target}"),
        row: None,
        body: None,
        headers: &[],
        success: StatusCode::NO_CONTENT,
    },
    // part: invitations
    // A member is the one role that every role allowed to invite outranks (ACCESS-004).
    Fixture {
        method: "post",
        template: "/v1/workspaces/{workspace_id}/invitations",
        path: |workspace, _| format!("/v1/workspaces/{workspace}/invitations"),
        row: None,
        body: Some((
            r#"{"email": "invitee@example.com", "role": "member"}"#,
            "application/json",
        )),
        headers: &[],
        success: StatusCode::CREATED,
    },
    // end part: invitations
    // part: files
    Fixture {
        method: "post",
        template: "/v1/workspaces/{workspace_id}/files",
        path: |workspace, _| format!("/v1/workspaces/{workspace}/files"),
        row: None,
        body: Some(("hello", "application/octet-stream")),
        headers: &[("content-disposition", "attachment; filename=\"blue.txt\"")],
        success: StatusCode::CREATED,
    },
    Fixture {
        method: "get",
        template: "/v1/workspaces/{workspace_id}/files",
        path: |workspace, _| format!("/v1/workspaces/{workspace}/files"),
        row: None,
        body: None,
        headers: &[],
        success: StatusCode::OK,
    },
    Fixture {
        method: "get",
        template: "/v1/workspaces/{workspace_id}/files/{file_id}",
        path: |workspace, id| format!("/v1/workspaces/{workspace}/files/{id}"),
        row: Some(|db, workspace| Box::pin(crate::support::files::arrange(db, workspace, "Blue"))),
        body: None,
        headers: &[],
        success: StatusCode::OK,
    },
    Fixture {
        method: "get",
        template: "/v1/workspaces/{workspace_id}/files/{file_id}/content",
        path: |workspace, id| format!("/v1/workspaces/{workspace}/files/{id}/content"),
        row: Some(|db, workspace| Box::pin(crate::support::files::arrange(db, workspace, "Blue"))),
        body: None,
        headers: &[],
        success: StatusCode::OK,
    },
    Fixture {
        method: "delete",
        template: "/v1/workspaces/{workspace_id}/files/{file_id}",
        path: |workspace, id| format!("/v1/workspaces/{workspace}/files/{id}"),
        row: Some(|db, workspace| Box::pin(crate::support::files::arrange(db, workspace, "Blue"))),
        body: None,
        headers: &[],
        success: StatusCode::NO_CONTENT,
    },
    // end part: files
];

/// The fixture of the route `method` `template`, if it has one.
fn fixture(method: &str, template: &str) -> Option<&'static Fixture> {
    FIXTURES
        .iter()
        .find(|fixture| fixture.method == method && fixture.template == template)
}

/// Sends `method` to `path` as `caller`, with the fixture's body, in its media type, when it has
/// one, which names `addressed` where it holds `{target}`, and its headers. The caller's session
/// has just proved who its user is, as a route that needs the proof asks (AUTH-023).
async fn send(
    api: &Api,
    method: Method,
    path: &str,
    (caller, addressed): (&SignedIn, &str),
    fixture: &Fixture,
) -> crate::support::api::Response {
    users::authenticated_ago(api, caller, 0).await;
    match fixture.body {
        None => call(api, method, path, caller).await,
        Some((body, media_type)) => {
            let mut request = api
                .request(method, path)
                .header("content-type", media_type)
                .body(body.replace("{target}", addressed))
                .bearer(&caller.access);
            for (name, value) in fixture.headers {
                request = request.header(name, value);
            }
            request.send().await
        }
    }
}

/// Sends the route `method` `template`, as `owner`, the ID of a member, `target`, or of a row, of
/// another workspace that `owner` belongs to, where the route addresses one by ID, in its path
/// or its body, and expects that it names nothing here (DATA-16).
async fn names_nothing_elsewhere(
    api: &Api,
    (method, template): (&Method, &str),
    fixture: &Fixture,
    owner: &SignedIn,
    target: &SignedIn,
) {
    let in_body = fixture
        .body
        .is_some_and(|(body, _)| body.contains("{target}"));
    if !template.contains("{user_id}") && fixture.row.is_none() && !in_body {
        return;
    }
    let db = api.database();
    let here = workspaces::arrange(db, "Here", &[(&owner.user_id, "owner")]).await;
    let there = workspaces::arrange(db, "There", &[(&owner.user_id, "owner")]).await;
    let foreign = match fixture.row {
        Some(arrange) => arrange(db, &there).await,
        None => {
            workspaces::join(db, &there, &target.user_id, "member").await;
            target.user_id.clone()
        }
    };
    let path = (fixture.path)(&here, &foreign);
    let response = send(api, method.clone(), &path, (owner, &foreign), fixture).await;
    assert_eq!(
        response.status,
        StatusCode::NOT_FOUND,
        "{method} {template}"
    );
}

#[tokio::test]
async fn test_08_every_workspace_route_answers_every_role_as_the_matrix_says() {
    let api = Api::new().await;
    let db = api.database();
    let mut callers = Vec::new();
    for role in ["owner", "admin", "member", "none"] {
        callers.push((
            role,
            users::register(&api, &format!("{role}@example.com")).await,
        ));
    }
    let target = users::register(&api, "target@example.com").await;
    let other_owner = users::register(&api, "other@example.com").await;
    let matrix = app::permissions();
    let paths = api.document()["paths"].as_object().unwrap().clone();
    let mut checked = 0;

    for (template, item) in &paths {
        for (method, operation) in item.as_object().unwrap() {
            if operation["x-access"] != "workspace" {
                continue;
            }
            let permission = operation["x-permission"].as_str().unwrap();
            let holders = &matrix
                .iter()
                .find(|(name, _)| *name == permission)
                .unwrap()
                .1;
            let fixture = fixture(method, template);
            assert!(
                fixture.is_some(),
                "{method} {template} has no fixture in TEST-08's test"
            );
            let fixture = fixture.unwrap();
            let method = Method::from_bytes(method.to_ascii_uppercase().as_bytes()).unwrap();
            for (role, caller) in &callers {
                let workspace = workspaces::arrange(db, "Acme", &[]).await;
                let below = if *role == "owner" { "admin" } else { "member" };
                if *role != "owner" {
                    workspaces::join(db, &workspace, &other_owner.user_id, "owner").await;
                }
                if *role != "none" {
                    workspaces::join(db, &workspace, &caller.user_id, role).await;
                }
                workspaces::join(db, &workspace, &target.user_id, below).await;
                let addressed = match fixture.row {
                    Some(arrange) => arrange(db, &workspace).await,
                    None => target.user_id.clone(),
                };
                let path = (fixture.path)(&workspace, &addressed);

                let response =
                    send(&api, method.clone(), &path, (caller, &addressed), fixture).await;

                let expected = match *role {
                    "none" => StatusCode::NOT_FOUND,
                    role if holders.contains(&role) => fixture.success,
                    _ => StatusCode::FORBIDDEN,
                };
                assert_eq!(response.status, expected, "{method} {template} as {role}");
                checked += 1;
            }
            let (_, owner) = &callers[0];
            names_nothing_elsewhere(&api, (&method, template), fixture, owner, &target).await;
        }
    }
    assert!(checked >= 36, "only {checked} calls");
}

// part: invitations
#[tokio::test]
async fn access_006_accepting_an_invitation_closes_the_new_member_s_streams() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;
    let invitation =
        crate::support::invitations::invite(&api, &ada, &workspace, "carol@example.com", "member")
            .await
            .json();
    let mut stream = crate::support::streams::connect(&api, &carol.access, None).await;
    stream.opened().await;

    let accepted = call(
        &api,
        Method::POST,
        &format!(
            "/v1/users/me/invitations/{}/accept",
            invitation["id"].as_str().unwrap()
        ),
        &carol,
    )
    .await;
    let (_, reason) = stream.closed().await;

    assert_eq!(accepted.status, StatusCode::OK);
    assert_eq!(reason, "access_changed");
}
// end part: invitations

/// The statement that locks the identity of `user`, as a sign-in does, so a deletion of their
/// account that starts meanwhile waits at it, once it has locked their memberships (ACCESS-008,
/// the accounts spec's "Deleting an account").
fn identity_of(user: &SignedIn) -> String {
    format!(
        "SELECT 1 FROM identities WHERE user_id = '{}' FOR UPDATE",
        user.user_id
    )
}

/// Ada, a member of Bob's workspace, reads it while the deletion of her account holds her
/// memberships; returns the API, Ada, the deletion's response and the read's.
async fn reading_while_deleted() -> (Api, SignedIn, Response, Response) {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(
        db,
        "Acme",
        &[(&bob.user_id, "owner"), (&ada.user_id, "member")],
    )
    .await;
    let mut held = db.holding(&identity_of(&ada)).await;

    let delete = call(&api, Method::DELETE, "/v1/users/me", &ada);
    let path = format!("/v1/workspaces/{workspace}");
    let read = async {
        // The deletion holds Ada's memberships, and waits at her identity.
        queries_wait_on_locks(db, 1).await;
        let read = call(&api, Method::GET, &path, &ada);
        let release = async {
            queries_wait_on_locks(db, 2).await;
            execute_on(&mut held, "ROLLBACK").await;
        };
        tokio::join!(read, release).0
    };
    let (deleted, read) = tokio::join!(delete, read);
    (api, ada, deleted, read)
}

#[tokio::test]
async fn access_008_the_user_s_operations_in_their_workspaces_wait_for_their_deletion() {
    let (_, _, deleted, read) = reading_while_deleted().await;

    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    // The read waited for the deletion, which ended her membership.
    assert_eq!(read.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn audit_004_a_refusal_whose_caller_s_account_went_meanwhile_is_recorded_nowhere() {
    let (api, _, _, read) = reading_while_deleted().await;

    assert_eq!(read.status, StatusCode::NOT_FOUND);
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM audit_entries WHERE action = 'access.denied'")
            .await,
        ["0"]
    );
}

#[tokio::test]
async fn access_008_erasable_refuses_while_a_workspace_the_user_owns_has_another_member() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let db = api.database();
    workspaces::arrange(
        db,
        "Acme",
        &[(&ada.user_id, "owner"), (&bob.user_id, "member")],
    )
    .await;
    workspaces::arrange(db, "Mine", &[(&ada.user_id, "owner")]).await;

    let refused = call(&api, Method::DELETE, "/v1/users/me", &ada).await;

    assert_eq!(refused.status, StatusCode::CONFLICT);
}

/// The statement that locks `user`'s membership in `workspace` `FOR SHARE`, as a request of
/// theirs in the workspace holds it until it ends (PLATFORM-009).
fn membership_of(workspace: &str, user: &SignedIn) -> String {
    format!(
        "SELECT 1 FROM workspace_memberships \
         WHERE workspace_id = '{workspace}' AND user_id = '{}' FOR SHARE",
        user.user_id
    )
}

/// What two requests of `caller`, each `method` to `path` with `body`, answer when both are sent
/// while another request of theirs in `workspace` runs, and so wait for it: their statuses,
/// sorted, and the code of the problem that the refused one answers.
async fn sent_at_once(
    api: &Api,
    workspace: &str,
    caller: &SignedIn,
    (method, path): (Method, &str),
    body: Option<serde_json::Value>,
) -> (Vec<StatusCode>, String) {
    let db = api.database();
    let mut running = db.holding(&membership_of(workspace, caller)).await;
    let first = api.spawn(method.clone(), path, &caller.access, body.clone());
    queries_wait_on_locks(db, 1).await;
    let second = api.spawn(method, path, &caller.access, body);
    queries_wait_on_locks(db, 2).await;
    execute_on(&mut running, "ROLLBACK").await;

    let answers = [first.await.unwrap(), second.await.unwrap()];
    let mut statuses: Vec<StatusCode> = answers.iter().map(|(status, _)| *status).collect();
    statuses.sort();
    let code = answers
        .iter()
        .find_map(|(_, problem)| problem["code"].as_str())
        .unwrap_or_default()
        .to_owned();
    (statuses, code)
}

#[tokio::test]
async fn access_009_two_leaves_sent_at_once_take_turns_and_the_second_finds_no_membership() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let member = users::register(&api, "member@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[(&owner.user_id, "owner"), (&member.user_id, "member")],
    )
    .await;
    let path = format!("/v1/workspaces/{workspace}/leave");

    let (statuses, code) =
        sent_at_once(&api, &workspace, &member, (Method::POST, &path), None).await;

    assert_eq!(statuses, [StatusCode::NO_CONTENT, StatusCode::NOT_FOUND]);
    assert_eq!(code, "request.not_found");
}

#[tokio::test]
async fn access_009_two_transfers_sent_at_once_take_turns_and_the_second_finds_an_admin() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let admin = users::register(&api, "admin@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[(&owner.user_id, "owner"), (&admin.user_id, "admin")],
    )
    .await;
    let path = format!("/v1/workspaces/{workspace}/transfer");
    let body = serde_json::json!({ "user_id": admin.user_id });

    let (statuses, code) =
        sent_at_once(&api, &workspace, &owner, (Method::POST, &path), Some(body)).await;

    assert_eq!(statuses, [StatusCode::NO_CONTENT, StatusCode::FORBIDDEN]);
    assert_eq!(code, "request.permission_denied");
    let owners = api
        .database()
        .owner_reads("SELECT user_id::text FROM workspace_memberships WHERE role = 'owner'")
        .await;
    assert_eq!(owners, std::slice::from_ref(&admin.user_id));
}

#[tokio::test]
async fn access_009_two_deletions_sent_at_once_take_turns_and_the_second_finds_no_workspace() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&owner.user_id, "owner")]).await;
    let path = format!("/v1/workspaces/{workspace}");

    let (statuses, code) =
        sent_at_once(&api, &workspace, &owner, (Method::DELETE, &path), None).await;

    assert_eq!(statuses, [StatusCode::NO_CONTENT, StatusCode::NOT_FOUND]);
    assert_eq!(code, "request.not_found");
}

#[tokio::test]
async fn data_20_a_deletion_waits_for_a_member_before_it_locks_the_members_below_them() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let admin = users::register(&api, "admin@example.com").await;
    let member = users::register(&api, "member@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(
        db,
        "Acme",
        &[
            (&owner.user_id, "owner"),
            (&admin.user_id, "admin"),
            (&member.user_id, "member"),
        ],
    )
    .await;
    // A request of the admin's, which holds their membership, and goes on to remove the member.
    let mut removal = db.holding(&membership_of(&workspace, &admin)).await;

    let deletion = api.spawn(
        Method::DELETE,
        &format!("/v1/workspaces/{workspace}"),
        &owner.access,
        None,
    );
    queries_wait_on_locks(db, 1).await;
    // The deletion waits at the admin's membership, and holds none below it: the removal takes
    // the member's, which a deletion that held it would keep from it for good.
    execute_on(
        &mut removal,
        &format!(
            "DELETE FROM workspace_memberships WHERE workspace_id = '{workspace}' AND user_id = '{}'",
            member.user_id
        ),
    )
    .await;
    execute_on(&mut removal, "COMMIT").await;

    assert_eq!(deletion.await.unwrap().0, StatusCode::NO_CONTENT);
    let left = db
        .owner_reads("SELECT count(*)::text FROM workspace_memberships")
        .await;
    assert_eq!(left, ["0"]);
}

#[tokio::test]
async fn audit_004_a_member_s_refusal_whose_workspace_went_meanwhile_is_recorded_nowhere() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let member = users::register(&api, "member@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(
        db,
        "Acme",
        &[(&owner.user_id, "owner"), (&member.user_id, "member")],
    )
    .await;
    // A deletion of the workspace, which holds its row.
    let mut deletion = db
        .holding(&format!(
            "SELECT 1 FROM workspaces WHERE id = '{workspace}' FOR UPDATE"
        ))
        .await;

    // A member may not delete the workspace: the refusal's entry waits at the workspace's row.
    let refused = api.spawn(
        Method::DELETE,
        &format!("/v1/workspaces/{workspace}"),
        &member.access,
        None,
    );
    queries_wait_on_locks(db, 1).await;
    execute_on(
        &mut deletion,
        &format!("DELETE FROM workspaces WHERE id = '{workspace}'"),
    )
    .await;
    execute_on(&mut deletion, "COMMIT").await;
    let (status, problem) = refused.await.unwrap();

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(problem["code"], "request.permission_denied");
    assert_eq!(
        db.owner_reads("SELECT count(*)::text FROM audit_entries WHERE action = 'access.denied'")
            .await,
        ["0"]
    );
}

// part: invitations
/// Bob accepts Ada's invitation to her workspace while her request, `method` to `path` with
/// `body`, waits for a request of Grace's, an admin of the workspace, having taken the lock of
/// Ada's own membership, so the acceptance waits for Ada's request; returns the API, Bob, the
/// workspace, the status of Ada's request, and the acceptance's answer.
async fn joining_while_the_owner_acts(
    method: Method,
    path: fn(&str) -> String,
    body: fn(&SignedIn) -> Option<serde_json::Value>,
) -> (
    Api,
    SignedIn,
    String,
    StatusCode,
    (StatusCode, serde_json::Value),
) {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(
        db,
        "Acme",
        &[(&ada.user_id, "owner"), (&grace.user_id, "admin")],
    )
    .await;
    let invited =
        crate::support::invitations::invite(&api, &ada, &workspace, "bob@example.com", "member")
            .await;
    let id = invited.json()["id"].as_str().unwrap().to_owned();
    let mut running = db.holding(&membership_of(&workspace, &grace)).await;

    let acted = api.spawn(method, &path(&workspace), &ada.access, body(&grace));
    queries_wait_on_locks(db, 1).await;
    let accepted = api.spawn(
        Method::POST,
        &format!("/v1/users/me/invitations/{id}/accept"),
        &bob.access,
        None,
    );
    queries_wait_on_locks(db, 2).await;
    execute_on(&mut running, "ROLLBACK").await;

    let (acted, _) = acted.await.unwrap();
    let accepted = accepted.await.unwrap();
    (api, bob, workspace, acted, accepted)
}

/// Bob's membership in `workspace`, as his role, if he has one.
async fn joined(api: &Api, bob: &SignedIn, workspace: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT role::text FROM workspace_memberships \
             WHERE workspace_id = '{workspace}' AND user_id = '{}'",
            bob.user_id
        ))
        .await
}

#[tokio::test]
async fn access_010_a_join_that_waited_for_a_transfer_joins_under_the_new_owner() {
    let (api, bob, workspace, transferred, (status, _)) = joining_while_the_owner_acts(
        Method::POST,
        |workspace| format!("/v1/workspaces/{workspace}/transfer"),
        |grace| Some(serde_json::json!({ "user_id": grace.user_id })),
    )
    .await;

    assert_eq!(transferred, StatusCode::NO_CONTENT);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(joined(&api, &bob, &workspace).await, ["member"]);
}

#[tokio::test]
async fn access_010_a_join_that_waited_for_the_workspace_s_deletion_adds_no_member() {
    let (api, bob, workspace, deleted, _) = joining_while_the_owner_acts(
        Method::DELETE,
        |workspace| format!("/v1/workspaces/{workspace}"),
        |_| None,
    )
    .await;

    assert_eq!(deleted, StatusCode::NO_CONTENT);
    assert!(joined(&api, &bob, &workspace).await.is_empty());
}

#[tokio::test]
async fn invitations_006_accepting_while_the_owner_deletes_the_workspace_answers_404() {
    let (.., (status, problem)) = joining_while_the_owner_acts(
        Method::DELETE,
        |workspace| format!("/v1/workspaces/{workspace}"),
        |_| None,
    )
    .await;

    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(problem["code"], "invitations.not_found");
}
/// Bob accepts Ada's invitation to her workspace, whose only member she is, while the deletion of
/// her account holds its owner's membership; returns the acceptance's response, with the database.
async fn joining_while_the_owner_is_deleted() -> (Api, SignedIn, crate::support::api::Response) {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    let invited =
        crate::support::invitations::invite(&api, &ada, &workspace, "bob@example.com", "member")
            .await;
    let id = invited.json()["id"].as_str().unwrap().to_owned();
    let mut held = db.holding(&identity_of(&ada)).await;

    let delete = call(&api, Method::DELETE, "/v1/users/me", &ada);
    let path = format!("/v1/users/me/invitations/{id}/accept");
    let accept = async {
        queries_wait_on_locks(db, 1).await;
        let accept = call(&api, Method::POST, &path, &bob);
        let release = async {
            queries_wait_on_locks(db, 2).await;
            execute_on(&mut held, "ROLLBACK").await;
        };
        tokio::join!(accept, release).0
    };
    let (deleted, accepted) = tokio::join!(delete, accept);
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    (api, bob, accepted)
}

#[tokio::test]
async fn access_008_a_join_that_waited_for_the_owner_s_deletion_adds_no_member() {
    let (api, bob, _) = joining_while_the_owner_is_deleted().await;

    let memberships = api
        .database()
        .owner_reads(&format!(
            "SELECT count(*)::text FROM workspace_memberships WHERE user_id = '{}'",
            bob.user_id
        ))
        .await;

    assert_eq!(memberships, ["0"]);
}

#[tokio::test]
async fn invitations_006_accepting_while_the_workspace_goes_with_its_owner_answers_404() {
    let (_, _, accepted) = joining_while_the_owner_is_deleted().await;

    assert_eq!(accepted.status, StatusCode::NOT_FOUND);
    assert_eq!(accepted.json()["code"], "invitations.not_found");
}
// end part: invitations
