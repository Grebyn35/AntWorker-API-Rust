//! What a workspace member may do (the access spec), and the authorization that `platform`
//! builds on it (the platform spec's "Units of work and authorization").

use http::{Method, StatusCode};

use crate::support::{
    Api, TestDatabase, execute_on,
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
            "SELECT host(client_ip) || ' ' || user_agent || ' ' || trace_id \
             FROM audit_entries WHERE action = 'access.denied'",
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

/// Waits until a query of the test database waits on a lock, as the administrator sees it: other
/// roles see only their own sessions' waits. The request the test sent leads there, so the wait
/// has no bound of its own (TEST-06).
async fn a_query_waits_on_a_lock(db: &TestDatabase) {
    let sql = format!(
        "SELECT count(*)::text FROM pg_stat_activity \
         WHERE datname = '{}' AND wait_event_type = 'Lock'",
        db.name()
    );
    while db.admin_reads(&sql).await == ["0"] {
        tokio::task::yield_now().await;
    }
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
    a_query_waits_on_a_lock(db).await;
    execute_on(&mut removal, "COMMIT").await;

    // The request waited for the removal, so it ran under the membership as it then stood.
    assert_eq!(request.await.unwrap(), StatusCode::NOT_FOUND);
}

/// How the generated test calls a workspace route: the path, given the workspace and a target
/// member below the caller's rank, its body and that body's media type, if it takes one, and the
/// status of a success.
struct Fixture {
    path: fn(&str, &str) -> String,
    body: Option<(&'static str, &'static str)>,
    success: StatusCode,
}

/// The fixture of each workspace route, by its method and path. A route without one fails the
/// test, so every new workspace route joins it (TEST-08).
fn fixture(method: &str, template: &str) -> Option<Fixture> {
    Some(match (method, template) {
        ("get", "/v1/workspaces/{workspace_id}") => Fixture {
            path: |workspace, _| format!("/v1/workspaces/{workspace}"),
            body: None,
            success: StatusCode::OK,
        },
        ("patch", "/v1/workspaces/{workspace_id}") => Fixture {
            path: |workspace, _| format!("/v1/workspaces/{workspace}"),
            body: Some((r#"{"name": "Renamed"}"#, "application/merge-patch+json")),
            success: StatusCode::OK,
        },
        ("get", "/v1/workspaces/{workspace_id}/members") => Fixture {
            path: |workspace, _| format!("/v1/workspaces/{workspace}/members"),
            body: None,
            success: StatusCode::OK,
        },
        ("delete", "/v1/workspaces/{workspace_id}/members/{user_id}") => Fixture {
            path: |workspace, target| format!("/v1/workspaces/{workspace}/members/{target}"),
            body: None,
            success: StatusCode::NO_CONTENT,
        },
        // part: invitations
        // A member is the one role that every role allowed to invite outranks (ACCESS-004).
        ("post", "/v1/workspaces/{workspace_id}/invitations") => Fixture {
            path: |workspace, _| format!("/v1/workspaces/{workspace}/invitations"),
            body: Some((
                r#"{"email": "invitee@example.com", "role": "member"}"#,
                "application/json",
            )),
            success: StatusCode::CREATED,
        },
        // end part: invitations
        _ => return None,
    })
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
                let path = (fixture.path)(&workspace, &target.user_id);

                let response = match fixture.body {
                    None => call(&api, method.clone(), &path, caller).await,
                    Some((body, media_type)) => {
                        api.request(method.clone(), &path)
                            .header("content-type", media_type)
                            .body(body)
                            .bearer(&caller.access)
                            .send()
                            .await
                    }
                };

                let expected = match *role {
                    "none" => StatusCode::NOT_FOUND,
                    role if holders.contains(&role) => fixture.success,
                    _ => StatusCode::FORBIDDEN,
                };
                assert_eq!(response.status, expected, "{method} {template} as {role}");
                checked += 1;
            }
            if template.contains("{user_id}") {
                // A member of another workspace the caller belongs to names nobody here (DATA-16).
                let (_, owner) = &callers[0];
                let here = workspaces::arrange(db, "Here", &[(&owner.user_id, "owner")]).await;
                let there = workspaces::arrange(db, "There", &[(&owner.user_id, "owner")]).await;
                workspaces::join(db, &there, &target.user_id, "member").await;
                let path = (fixture.path)(&here, &target.user_id);
                let foreign = call(&api, method.clone(), &path, owner).await;
                assert_eq!(foreign.status, StatusCode::NOT_FOUND, "{method} {template}");
            }
        }
    }
    assert!(checked >= 16, "only {checked} calls");
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
