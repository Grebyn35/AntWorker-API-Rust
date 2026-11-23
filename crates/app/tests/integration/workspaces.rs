//! Workspaces and their members (the workspaces spec), with the lists' mechanisms they are the
//! first to use (the platform spec).

use serde_json::{Value, json};

use crate::support::{
    Api,
    api::call,
    events::{self, Event},
    lists::{field, pages},
    streams,
    users::{self, SignedIn},
    workspaces,
};

/// The names of the permissions that the matrix gives `role`, in its order (ACCESS-002).
fn held(role: &str) -> Vec<&'static str> {
    app::permissions()
        .into_iter()
        .filter(|(_, roles)| roles.contains(&role))
        .map(|(name, _)| name)
        .collect()
}

#[tokio::test]
async fn workspaces_003_lists_the_caller_s_workspaces_the_latest_joined_first() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let first = workspaces::arrange(db, "First", &[(&ada.user_id, "owner")]).await;
    let second = workspaces::arrange(db, "Second", &[(&grace.user_id, "owner")]).await;
    workspaces::join(db, &second, &ada.user_id, "member").await;
    workspaces::arrange(db, "Grace's", &[(&grace.user_id, "owner")]).await;
    db.owner_executes(&format!(
        "UPDATE workspace_memberships SET created_at = CASE workspace_id \
         WHEN '{first}' THEN timestamptz '2026-11-01T10:00:00Z' \
         ELSE timestamptz '2026-11-01T11:00:00.5Z' END WHERE user_id = '{}'",
        ada.user_id
    ))
    .await;

    let response = api.get("/v1/workspaces").bearer(&ada.access).send().await;

    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(field(&body, "id"), [second.clone(), first]);
    assert_eq!(field(&body, "name"), ["Second", "First"]);
    assert_eq!(field(&body, "role"), ["member", "owner"]);
    // Each with the permissions that the caller's role holds in it (WORKSPACES-007).
    assert_eq!(body["items"][0]["permissions"], json!(held("member")));
    assert_eq!(body["items"][1]["permissions"], json!(held("owner")));
    assert_eq!(
        field(&body, "joined_at"),
        ["2026-11-01T11:00:00.500000Z", "2026-11-01T10:00:00.000000Z"]
    );
    assert_eq!(body["next_cursor"], Value::Null);
}

#[tokio::test]
async fn platform_046_paging_returns_each_row_once_across_a_shared_joining_time() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let mut arranged = Vec::new();
    for name in ["A", "B", "C", "D", "E"] {
        arranged.push(workspaces::arrange(db, name, &[(&ada.user_id, "owner")]).await);
    }
    db.owner_executes("UPDATE workspace_memberships SET created_at = '2026-11-01T10:00:00Z'")
        .await;

    let mut seen = Vec::new();
    let mut path = "/v1/workspaces?limit=2".to_owned();
    loop {
        let body = api.get(&path).bearer(&ada.access).send().await.json();
        seen.extend(field(&body, "id"));
        match body["next_cursor"].as_str() {
            Some(cursor) => path = format!("/v1/workspaces?limit=2&cursor={cursor}"),
            None => break,
        }
    }

    // Among memberships that share a joining time, the higher membership ID comes first.
    let by_membership = db
        .owner_reads(&format!(
            "SELECT workspace_id::text FROM workspace_memberships \
             WHERE user_id = '{}' ORDER BY id DESC",
            ada.user_id
        ))
        .await;
    assert_eq!(seen, by_membership);
    let mut once = seen.clone();
    once.sort();
    arranged.sort();
    assert_eq!(once, arranged);
}

#[tokio::test]
async fn platform_046_a_limit_out_of_range_and_a_cursor_of_another_route_are_refused_together() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    let grace = users::register(&api, "grace@example.com").await;
    workspaces::join(db, &workspace, &grace.user_id, "member").await;
    let members = api
        .get(&format!("/v1/workspaces/{workspace}/members?limit=1"))
        .bearer(&ada.access)
        .send()
        .await
        .json();
    let cursor = members["next_cursor"].as_str().unwrap();

    let response = api
        .get(&format!("/v1/workspaces?limit=0&cursor={cursor}"))
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 400);
    let body = response.json();
    assert_eq!(body["code"], "request.invalid_query");
    let refused: Vec<(&str, &str)> = body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|error| {
            (
                error["field"].as_str().unwrap(),
                error["code"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        refused,
        [("/limit", "out_of_range"), ("/cursor", "invalid")]
    );
}

#[tokio::test]
async fn platform_047_a_search_matches_any_part_ignoring_case_in_every_script() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    for name in ["Åsa's team", "Acme 50% off", "Acme_Labs", "AcmeXLabs"] {
        workspaces::arrange(db, name, &[(&ada.user_id, "owner")]).await;
    }
    let names = |search: &'static str| {
        let api = &api;
        let access = ada.access.clone();
        async move {
            let path = format!("/v1/workspaces?search={search}");
            let mut names = field(&api.get(&path).bearer(&access).send().await.json(), "name");
            names.sort();
            names
        }
    };

    assert_eq!(names("%C3%A5SA").await, ["Åsa's team"]);
    assert_eq!(names("50%25").await, ["Acme 50% off"]);
    assert_eq!(names("e_l").await, ["Acme_Labs"]);
    assert_eq!(
        names("%20%20acme%20").await,
        ["Acme 50% off", "AcmeXLabs", "Acme_Labs"]
    );
}

#[tokio::test]
async fn platform_047_a_search_that_holds_a_control_character_answers_400() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    // Postgres takes no U+0000 in a text, so the search must be refused before it reaches it.
    let response = api
        .get("/v1/workspaces?search=ac%00me")
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 400);
    let body = response.json();
    assert_eq!(body["code"], "request.invalid_query");
    assert_eq!(body["errors"][0]["field"], "/search");
    assert_eq!(body["errors"][0]["code"], "control_character");
}

#[tokio::test]
async fn access_005_a_search_reaches_every_workspace_the_caller_belongs_to_and_no_other() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let mut expected = Vec::new();
    for name in ["Team one", "Team two", "Team three"] {
        expected.push(workspaces::arrange(db, name, &[(&ada.user_id, "member")]).await);
    }
    workspaces::arrange(db, "Team four", &[(&grace.user_id, "owner")]).await;

    let body = api
        .get("/v1/workspaces?search=team")
        .bearer(&ada.access)
        .send()
        .await
        .json();

    let mut ids = field(&body, "id");
    ids.sort();
    expected.sort();
    assert_eq!(ids, expected);
}

#[tokio::test]
async fn access_005_a_rename_publishes_in_each_workspace_the_caller_belongs_to_and_in_no_other() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let own = workspaces::arrange(db, "Team one", &[(&ada.user_id, "member")]).await;
    workspaces::arrange(db, "Team four", &[(&grace.user_id, "owner")]).await;
    // The registrations' sessions published their starts (AUTH-046).
    db.owner_executes("DELETE FROM events").await;

    // Each workspace of `workspace_ids` gets the rename's event (ACCOUNTS-007).
    let renamed = api
        .patch("/v1/users/me", &json!({ "name": "Ada King" }))
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(renamed.status, 200);
    let reached = db
        .owner_reads("SELECT workspace_id::text FROM events WHERE workspace_id IS NOT NULL")
        .await;
    assert_eq!(reached, [own]);
}

#[tokio::test]
async fn workspaces_004_lists_the_members_with_their_profiles_the_latest_joined_first() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &grace.user_id, "admin").await;
    db.owner_executes(&format!(
        "UPDATE workspace_memberships SET created_at = CASE user_id \
         WHEN '{}' THEN timestamptz '2026-11-01T10:00:00Z' \
         ELSE timestamptz '2026-11-01T11:00:00Z' END",
        ada.user_id
    ))
    .await;

    let response = api
        .get(&format!("/v1/workspaces/{workspace}/members"))
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(
        field(&body, "user_id"),
        [grace.user_id.clone(), ada.user_id.clone()]
    );
    assert_eq!(
        field(&body, "email"),
        ["grace@example.com", "ada@example.com"]
    );
    assert_eq!(field(&body, "name"), ["Ada Lovelace", "Ada Lovelace"]);
    assert_eq!(field(&body, "role"), ["admin", "owner"]);
    assert_eq!(
        field(&body, "joined_at"),
        ["2026-11-01T11:00:00.000000Z", "2026-11-01T10:00:00.000000Z"]
    );
}

#[tokio::test]
async fn workspaces_004_a_search_matches_a_name_or_an_email_and_role_keeps_one_role() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let linus = users::register(&api, "linus@example.com").await;
    let db = api.database();
    db.owner_executes(&format!(
        "UPDATE users SET name = 'Grace Hopper' WHERE id = '{}'",
        grace.user_id
    ))
    .await;
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &grace.user_id, "member").await;
    workspaces::join(db, &workspace, &linus.user_id, "member").await;
    let emails = |query: &'static str| {
        let api = &api;
        let access = ada.access.clone();
        let workspace = workspace.clone();
        async move {
            let path = format!("/v1/workspaces/{workspace}/members?{query}");
            let mut emails = field(&api.get(&path).bearer(&access).send().await.json(), "email");
            emails.sort();
            emails
        }
    };

    assert_eq!(emails("search=HOPPER").await, ["grace@example.com"]);
    assert_eq!(emails("search=linus%40").await, ["linus@example.com"]);
    assert_eq!(
        emails("role=member").await,
        ["grace@example.com", "linus@example.com"]
    );
    assert_eq!(
        emails("role=member&search=lovelace").await,
        ["linus@example.com"]
    );
}

/// A workspace of Ada's alone, and another of a user whose name and email also match "ada".
async fn ada_and_an_outsider(api: &Api) -> (SignedIn, String) {
    let ada = users::register(api, "ada@example.com").await;
    let outsider = users::register(api, "ada.outside@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    workspaces::arrange(db, "Other", &[(&outsider.user_id, "owner")]).await;
    (ada, workspace)
}

#[tokio::test]
async fn accounts_005_member_profiles_are_exactly_the_workspace_s_members() {
    let api = Api::new().await;
    let (ada, workspace) = ada_and_an_outsider(&api).await;

    let body = api
        .get(&format!("/v1/workspaces/{workspace}/members"))
        .bearer(&ada.access)
        .send()
        .await
        .json();

    assert_eq!(field(&body, "email"), ["ada@example.com"]);
}

#[tokio::test]
async fn accounts_006_a_member_search_matches_only_the_workspace_s_members() {
    let api = Api::new().await;
    let (ada, workspace) = ada_and_an_outsider(&api).await;

    let body = api
        .get(&format!("/v1/workspaces/{workspace}/members?search=ADA"))
        .bearer(&ada.access)
        .send()
        .await
        .json();

    assert_eq!(field(&body, "email"), ["ada@example.com"]);
}

#[tokio::test]
async fn accounts_006_a_member_search_leaves_out_the_members_whose_name_and_email_miss_it() {
    let api = Api::new().await;
    let (ada, workspace) = ada_and_an_outsider(&api).await;
    // A member whose email and name, Ada Lovelace as every test user's, miss the search.
    let grace = users::register(&api, "grace@example.com").await;
    workspaces::join(api.database(), &workspace, &grace.user_id, "member").await;

    let body = api
        .get(&format!(
            "/v1/workspaces/{workspace}/members?search=ADA@EXAMPLE"
        ))
        .bearer(&ada.access)
        .send()
        .await
        .json();

    assert_eq!(field(&body, "email"), ["ada@example.com"]);
}

#[tokio::test]
async fn accounts_006_a_member_search_finds_a_member_by_their_name_alone() {
    let api = Api::new().await;
    let (ada, workspace) = ada_and_an_outsider(&api).await;
    let grace = users::register(&api, "grace@example.com").await;
    workspaces::join(api.database(), &workspace, &grace.user_id, "member").await;
    // Grace's name holds the search, and no member's email does.
    api.patch("/v1/users/me", &json!({ "name": "Grace Hopper" }))
        .bearer(&grace.access)
        .send()
        .await;

    let body = api
        .get(&format!("/v1/workspaces/{workspace}/members?search=HOPPER"))
        .bearer(&ada.access)
        .send()
        .await
        .json();

    assert_eq!(field(&body, "email"), ["grace@example.com"]);
}

#[tokio::test]
async fn workspaces_005_removing_a_member_deletes_their_membership_and_signals_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &grace.user_id, "member").await;
    // The registrations' sessions published their starts (AUTH-046).
    db.owner_executes("DELETE FROM events").await;
    let mut signals = db.signals().await;

    let response = api
        .request(
            http::Method::DELETE,
            &format!("/v1/workspaces/{workspace}/members/{}", grace.user_id),
        )
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 204);
    let members = db
        .owner_reads("SELECT user_id::text FROM workspace_memberships")
        .await;
    assert_eq!(members, std::slice::from_ref(&ada.user_id));
    let entries = db
        .owner_reads(
            "SELECT scope || ' ' || workspace_id || ' ' || actor_user_id || ' ' || target_user_id \
             || ' ' || resource_type || ' ' || resource_id || ' ' || details::text \
             || ' ' || coalesce(permission, '-') FROM audit_entries WHERE action = 'member.removed'",
        )
        .await;
    assert_eq!(
        entries,
        [format!(
            "workspace {workspace} {} {} workspace {workspace} {{}} members.read",
            ada.user_id, grace.user_id
        )]
    );
    let events = db
        .owner_reads(
            "SELECT name || ' ' || scope || ' ' || workspace_id || ' ' || permission FROM events",
        )
        .await;
    assert_eq!(
        events,
        [format!("member.removed workspace {workspace} members.read")]
    );
    // The signal names its transaction, the event's.
    let transaction: u64 = db.owner_reads("SELECT xact::text FROM events").await[0]
        .parse()
        .unwrap();
    assert_eq!(
        signals.received().await,
        [json!({
            "kind": "membership_changed",
            "user_id": grace.user_id,
            "workspace_id": workspace,
            "transaction": transaction
        })]
    );
}

#[tokio::test]
async fn workspaces_006_a_caller_can_t_remove_themselves_or_a_rank_at_or_above_their_own() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let admin = users::register(&api, "admin@example.com").await;
    let peer = users::register(&api, "peer@example.com").await;
    let stranger = users::register(&api, "stranger@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&owner.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &admin.user_id, "admin").await;
    workspaces::join(db, &workspace, &peer.user_id, "admin").await;
    let remove = |caller: &SignedIn, target: &str| {
        let path = format!("/v1/workspaces/{workspace}/members/{target}");
        api.request(http::Method::DELETE, &path)
            .bearer(&caller.access)
            .send()
    };

    let myself = remove(&admin, &admin.user_id).await;
    let equal = remove(&admin, &peer.user_id).await;
    let higher = remove(&admin, &owner.user_id).await;
    let not_member = remove(&admin, &stranger.user_id).await;
    let not_an_id = remove(&admin, "nobody").await;

    assert_eq!(myself.status, 422);
    assert_eq!(myself.json()["code"], "workspaces.cannot_remove_self");
    for refused in [&equal, &higher] {
        assert_eq!(refused.status, 403);
        assert_eq!(refused.json()["code"], "request.permission_denied");
    }
    for missing in [&not_member, &not_an_id] {
        assert_eq!(missing.status, 404);
        assert_eq!(missing.json()["code"], "workspaces.member_not_found");
    }
    let members = db
        .owner_reads("SELECT count(*)::text FROM workspace_memberships")
        .await;
    assert_eq!(members, ["3"]);
}

#[tokio::test]
async fn platform_072_a_row_id_that_isn_t_a_uuid_answers_as_a_missing_row_once_authorized() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let outsider = users::register(&api, "outsider@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&owner.user_id, "owner")]).await;
    let path = format!("/v1/workspaces/{workspace}/members/not-a-uuid");

    let member = api
        .request(http::Method::DELETE, &path)
        .bearer(&owner.access)
        .send()
        .await;
    let non_member = api
        .request(http::Method::DELETE, &path)
        .bearer(&outsider.access)
        .send()
        .await;

    assert_eq!(member.status, 404);
    assert_eq!(member.json()["code"], "workspaces.member_not_found");
    // The caller's membership is checked first, so a non-member learns nothing of the workspace.
    assert_eq!(non_member.status, 404);
    assert_eq!(non_member.json()["code"], "request.not_found");
}

#[tokio::test]
async fn platform_072_a_path_segment_that_isn_t_utf_8_answers_as_a_missing_row_once_authorized() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let outsider = users::register(&api, "outsider@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&owner.user_id, "owner")]).await;
    // `%FF` percent-decodes to a byte that no UTF-8 text holds.
    let path = format!("/v1/workspaces/{workspace}/members/%FF");

    let member = api
        .request(http::Method::DELETE, &path)
        .bearer(&owner.access)
        .send()
        .await;
    let non_member = api
        .request(http::Method::DELETE, &path)
        .bearer(&outsider.access)
        .send()
        .await;

    assert_eq!(member.status, 404);
    assert_eq!(member.json()["code"], "workspaces.member_not_found");
    // The caller's membership is checked first, so a non-member learns nothing of the workspace.
    assert_eq!(non_member.status, 404);
    assert_eq!(non_member.json()["code"], "request.not_found");
}

#[tokio::test]
async fn platform_046_a_page_holds_50_items_without_a_limit() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    workspaces::member_of_many(api.database(), &ada.user_id, 51, "member").await;

    let body = api
        .get("/v1/workspaces")
        .bearer(&ada.access)
        .send()
        .await
        .json();

    assert_eq!(body["items"].as_array().unwrap().len(), 50);
    assert!(body["next_cursor"].is_string());
}

#[tokio::test]
async fn workspaces_004_paging_returns_each_member_once_across_a_shared_joining_time() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    let mut arranged = vec![ada.user_id.clone()];
    for name in ["b", "c", "d", "e"] {
        let user = users::register(&api, &format!("{name}@example.com")).await;
        workspaces::join(db, &workspace, &user.user_id, "member").await;
        arranged.push(user.user_id);
    }
    db.owner_executes("UPDATE workspace_memberships SET created_at = '2026-11-01T10:00:00Z'")
        .await;

    let path = format!("/v1/workspaces/{workspace}/members?limit=2");
    let pages = pages(&api, &ada, &path, "user_id").await;

    // Among members who share a joining time, the higher membership ID comes first.
    let by_membership = db
        .owner_reads(&format!(
            "SELECT user_id::text FROM workspace_memberships \
             WHERE workspace_id = '{workspace}' ORDER BY id DESC"
        ))
        .await;
    assert_eq!(pages.iter().map(Vec::len).collect::<Vec<_>>(), [2, 2, 1]);
    assert_eq!(pages.concat(), by_membership);
    let mut once = pages.concat();
    once.sort();
    arranged.sort();
    assert_eq!(once, arranged);
}

#[tokio::test]
async fn workspaces_003_a_search_pages_through_its_matches() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    for name in ["Team one", "Other", "Team two", "Team three"] {
        workspaces::arrange(db, name, &[(&ada.user_id, "owner")]).await;
    }

    let pages = pages(&api, &ada, "/v1/workspaces?search=team&limit=2", "name").await;

    assert_eq!(pages, [vec!["Team three", "Team two"], vec!["Team one"]]);
}

#[tokio::test]
async fn workspaces_004_a_search_pages_through_its_matches() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    for name in ["team.one", "other", "team.two", "team.three"] {
        let user = users::register(&api, &format!("{name}@example.com")).await;
        workspaces::join(db, &workspace, &user.user_id, "member").await;
    }

    let path = format!("/v1/workspaces/{workspace}/members?search=TEAM&limit=2");
    let pages = pages(&api, &ada, &path, "email").await;

    assert_eq!(
        pages,
        [
            vec!["team.three@example.com", "team.two@example.com"],
            vec!["team.one@example.com"]
        ]
    );
}

/// Renames `workspace` to `name` as `user`, with `if_match` as its `If-Match` if given.
async fn rename(
    api: &Api,
    user: &SignedIn,
    workspace: &str,
    name: &str,
    if_match: Option<&str>,
) -> crate::support::api::Response {
    let mut call = api
        .patch(
            &format!("/v1/workspaces/{workspace}"),
            &json!({ "name": name }),
        )
        .bearer(&user.access);
    if let Some(etag) = if_match {
        call = call.header("if-match", etag);
    }
    call.send().await
}

/// The name `workspace` holds.
async fn stored_name(api: &Api, workspace: &str) -> String {
    api.database()
        .owner_reads(&format!(
            "SELECT name FROM workspaces WHERE id = '{workspace}'"
        ))
        .await
        .remove(0)
}

#[tokio::test]
async fn workspaces_007_reading_a_workspace_gives_it_with_the_caller_s_role_and_its_etag() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &grace.user_id, "member").await;
    let path = format!("/v1/workspaces/{workspace}");

    let owner = api.get(&path).bearer(&ada.access).send().await;
    let member = api.get(&path).bearer(&grace.access).send().await;

    assert_eq!(owner.status, 200);
    let body = owner.json();
    assert_eq!(body["id"], workspace);
    assert_eq!(body["name"], "Acme");
    assert_eq!(body["role"], "owner");
    assert!(body["created_at"].as_str().unwrap().ends_with('Z'));
    assert_eq!(owner.header("etag"), "\"1\"");
    assert_eq!(member.status, 200);
    assert_eq!(member.json()["role"], "member");
    assert_eq!(member.header("etag"), "\"1\"");
}

#[tokio::test]
async fn workspaces_007_a_workspace_shows_its_caller_the_permissions_their_role_holds() {
    let api = Api::new().await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[]).await;
    let path = format!("/v1/workspaces/{workspace}");
    let mut callers = Vec::new();
    for role in ["owner", "admin", "member"] {
        let caller = users::register(&api, &format!("{role}@example.com")).await;
        workspaces::join(db, &workspace, &caller.user_id, role).await;
        callers.push((role, caller));
    }

    let mut shown = Vec::new();
    for (role, caller) in &callers {
        let read = api.get(&path).bearer(&caller.access).send().await;
        assert_eq!(read.status, 200, "{role}");
        let body = read.json();
        // Exactly those that the matrix gives the role, in its order (ACCESS-002).
        assert_eq!(body["permissions"], json!(held(role)), "{role}");
        shown.push(body["permissions"].clone());
    }

    let holds = |role: usize, permission: &str| {
        shown[role].as_array().unwrap().contains(&json!(permission))
    };
    let (owner, admin, member) = (0, 1, 2);
    assert!(holds(owner, "workspace.delete") && holds(owner, "workspace.transfer"));
    assert!(!holds(owner, "workspace.leave"));
    assert!(holds(admin, "workspace.leave") && holds(admin, "members.remove"));
    assert!(!holds(admin, "workspace.transfer") && !holds(admin, "workspace.delete"));
    assert!(holds(member, "workspace.read") && holds(member, "members.read"));
    assert!(!holds(member, "workspace.update") && !holds(member, "members.remove"));
    // A member whose role changed reads the permissions of the role they now hold.
    let (owner, member) = (&callers[0].1, &callers[2].1);
    let changed = change_role(&api, owner, (&workspace, &member.user_id), "admin", None).await;
    assert_eq!(changed.status, 200);
    let read = api.get(&path).bearer(&member.access).send().await;
    assert_eq!(read.json()["role"], "admin");
    assert_eq!(read.json()["permissions"], json!(held("admin")));
}

#[tokio::test]
async fn workspaces_008_renaming_stores_the_name_records_it_and_publishes_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &grace.user_id, "admin").await;
    // The registrations' sessions published their starts (AUTH-046).
    db.owner_executes("DELETE FROM events").await;

    let response = rename(&api, &grace, &workspace, "  Acme \t Labs ", None).await;

    assert_eq!(response.status, 200);
    let body = response.json();
    assert_eq!(body["id"], workspace);
    assert_eq!(body["name"], "Acme Labs");
    assert_eq!(body["role"], "admin");
    assert_eq!(response.header("etag"), "\"2\"");
    // The workspace as a read gives it to the same caller (WORKSPACES-007).
    let read = api
        .get(&format!("/v1/workspaces/{workspace}"))
        .bearer(&grace.access)
        .send()
        .await;
    assert_eq!(body, read.json());
    assert_eq!(stored_name(&api, &workspace).await, "Acme Labs");
    let entries = db
        .owner_reads(
            "SELECT scope || ' ' || workspace_id || ' ' || actor_user_id || ' ' \
             || coalesce(target_user_id::text, '-') || ' ' || resource_type || ' ' \
             || resource_id || ' ' || details::text || ' ' || coalesce(permission, '-') \
             FROM audit_entries WHERE action = 'workspace.renamed'",
        )
        .await;
    assert_eq!(
        entries,
        [format!(
            "workspace {workspace} {} - workspace {workspace} {{}} workspace.read",
            grace.user_id
        )]
    );
    let events = db
        .owner_reads(
            "SELECT name || ' ' || scope || ' ' || workspace_id || ' ' || permission || ' ' \
             || payload::text FROM events",
        )
        .await;
    assert_eq!(
        events,
        [format!(
            "workspace.updated workspace {workspace} workspace.read {{\"workspace_id\": \"{workspace}\"}}"
        )]
    );
}

#[tokio::test]
async fn workspaces_008_a_member_may_not_rename_and_the_owner_may() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &grace.user_id, "member").await;

    let refused = rename(&api, &grace, &workspace, "Grace's", None).await;
    let unchanged = stored_name(&api, &workspace).await;
    let renamed = rename(&api, &ada, &workspace, "Ada's", None).await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "request.permission_denied");
    assert_eq!(unchanged, "Acme");
    assert_eq!(renamed.status, 200);
    assert_eq!(stored_name(&api, &workspace).await, "Ada's");
}

#[tokio::test]
async fn workspaces_002_a_new_name_that_the_rule_refuses_answers_400_and_changes_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;

    let blank = rename(&api, &ada, &workspace, " \t ", None).await;
    // Postgres stores no U+0000 in a text, so the name must be refused before it reaches it.
    let control = rename(&api, &ada, &workspace, "Ac\u{0}me", None).await;
    // A name longer than the document allows is malformed input, sent raw.
    let long = api
        .request(http::Method::PATCH, &format!("/v1/workspaces/{workspace}"))
        .header("content-type", "application/merge-patch+json")
        .body(json!({ "name": "a".repeat(256) }).to_string())
        .bearer(&ada.access)
        .send()
        .await;

    for (refused, code) in [
        (&blank, "required"),
        (&long, "too_long"),
        (&control, "control_character"),
    ] {
        assert_eq!(refused.status, 400);
        assert_eq!(refused.json()["code"], "request.invalid_fields");
        assert_eq!(refused.json()["errors"][0]["field"], "/name");
        assert_eq!(refused.json()["errors"][0]["code"], code);
    }
    assert_eq!(stored_name(&api, &workspace).await, "Acme");
}

// part: workspace_creation
#[tokio::test]
async fn workspaces_002_a_created_workspace_s_name_is_normalized_and_one_that_the_rule_refuses_answers_400()
 {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let longest = "a".repeat(255);
    let create = |name: &str| {
        api.post_raw("/v1/workspaces", &json!({ "name": name }))
            .bearer(&ada.access)
            .send()
    };

    // Trimmed, its inner whitespace collapsed, and in Normalization Form C.
    let created = create("  Cafe\u{301} \t\n Labs ").await;
    let long_enough = create(&longest).await;
    let blank = create(" \t ").await;
    // Postgres stores no U+0000 in a text, so the name must be refused before it reaches it.
    let control = create("Ac\u{0}me").await;
    // A name longer than the document allows is malformed input, sent raw.
    let long = create(&"a".repeat(256)).await;

    assert_eq!(created.status, 201, "{}", created.json());
    assert_eq!(created.json()["name"], "Caf\u{e9} Labs");
    assert_eq!(long_enough.status, 201, "{}", long_enough.json());
    for (refused, code) in [
        (&blank, "required"),
        (&long, "too_long"),
        (&control, "control_character"),
    ] {
        assert_eq!(refused.status, 400, "{code}");
        assert_eq!(refused.json()["code"], "request.invalid_fields");
        assert_eq!(refused.json()["errors"][0]["field"], "/name");
        assert_eq!(refused.json()["errors"][0]["code"], code);
    }
    let mut names = api
        .database()
        .owner_reads("SELECT name FROM workspaces")
        .await;
    names.sort();
    assert_eq!(names, ["Caf\u{e9} Labs".to_owned(), longest]);
}
// end part: workspace_creation

// part: superadmins
#[tokio::test]
async fn workspaces_002_a_superadmin_s_workspace_s_name_is_normalized_and_one_that_the_rule_refuses_answers_400()
 {
    let api = Api::new().await;
    let sam = users::register(&api, "sam@example.com").await;
    crate::support::superadmins::make_with_app(&api, &sam).await;
    let longest = "a".repeat(255);
    let create = |name: &str| {
        api.post_raw("/v1/superadmin/workspaces", &json!({ "name": name }))
            .bearer(&sam.access)
            .send()
    };

    // Trimmed, its inner whitespace collapsed, and in Normalization Form C.
    let created = create("  Cafe\u{301} \t\n Labs ").await;
    let long_enough = create(&longest).await;
    let blank = create(" \t ").await;
    // Postgres stores no U+0000 in a text, so the name must be refused before it reaches it.
    let control = create("Ac\u{0}me").await;
    // A name longer than the document allows is malformed input, sent raw.
    let long = create(&"a".repeat(256)).await;

    assert_eq!(created.status, 201, "{}", created.json());
    assert_eq!(created.json()["name"], "Caf\u{e9} Labs");
    assert_eq!(long_enough.status, 201, "{}", long_enough.json());
    for (refused, code) in [
        (&blank, "required"),
        (&long, "too_long"),
        (&control, "control_character"),
    ] {
        assert_eq!(refused.status, 400, "{code}");
        assert_eq!(refused.json()["code"], "request.invalid_fields");
        assert_eq!(refused.json()["errors"][0]["field"], "/name");
        assert_eq!(refused.json()["errors"][0]["code"], code);
    }
    let mut names = api
        .database()
        .owner_reads("SELECT name FROM workspaces")
        .await;
    names.sort();
    assert_eq!(names, ["Caf\u{e9} Labs".to_owned(), longest]);
}
// end part: superadmins

#[tokio::test]
async fn platform_049_a_rename_sent_as_plain_json_is_refused() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;

    let response = api
        .request(http::Method::PATCH, &format!("/v1/workspaces/{workspace}"))
        .header("content-type", "application/json")
        .body(json!({ "name": "Ada's" }).to_string())
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 400);
    assert_eq!(response.json()["code"], "request.invalid_body");
    assert_eq!(
        response.json()["detail"],
        "The body must be sent as application/merge-patch+json."
    );
    assert_eq!(stored_name(&api, &workspace).await, "Acme");
}

#[tokio::test]
async fn platform_071_a_rename_with_a_stale_etag_answers_412_and_changes_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &grace.user_id, "admin").await;
    let read = api
        .get(&format!("/v1/workspaces/{workspace}"))
        .bearer(&ada.access)
        .send()
        .await;
    let etag = read.header("etag").to_owned();
    assert_eq!(etag, "\"1\"");

    let first = rename(&api, &grace, &workspace, "Grace's", Some(&etag)).await;
    let stale = rename(&api, &ada, &workspace, "Ada's", Some(&etag)).await;

    assert_eq!(first.status, 200);
    assert_eq!(first.header("etag"), "\"2\"");
    assert_eq!(stale.status, 412);
    assert_eq!(stale.json()["code"], "request.precondition_failed");
    assert_eq!(stored_name(&api, &workspace).await, "Grace's");
    let recorded = db
        .owner_reads(
            "SELECT (SELECT count(*) FROM audit_entries WHERE action = 'workspace.renamed') \
             || ' ' || (SELECT count(*) FROM events WHERE name = 'workspace.updated')",
        )
        .await;
    assert_eq!(recorded, ["1 1"]);
}

#[tokio::test]
async fn platform_071_a_current_etag_a_star_or_none_lets_the_rename_apply() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;

    let current = rename(&api, &ada, &workspace, "One", Some("\"7\", \"1\"")).await;
    let star = rename(&api, &ada, &workspace, "Two", Some("*")).await;
    let none = rename(&api, &ada, &workspace, "Three", None).await;
    let weak = rename(&api, &ada, &workspace, "Four", Some("W/\"4\"")).await;

    assert_eq!(current.status, 200);
    assert_eq!(star.status, 200);
    assert_eq!(none.status, 200);
    assert_eq!(none.header("etag"), "\"4\"");
    // A weak tag never matches, even of the current version (PLATFORM-071).
    assert_eq!(weak.status, 412);
    assert_eq!(stored_name(&api, &workspace).await, "Three");
}

#[tokio::test]
async fn sec_19_workspace_updated_reaches_every_member_s_every_device_and_no_replay_from_before_joining()
 {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let ada_laptop = users::new_session(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let alan = users::register(&api, "alan@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(
        db,
        "Acme",
        &[(&ada.user_id, "owner"), (&grace.user_id, "member")],
    )
    .await;
    // An event to Alan alone gives his stream one to resume after.
    db.owner_executes(&events::statement(
        &[Event::to_user(&alan.user_id, "note.made", "{}")],
        true,
    ))
    .await;
    let before = streams::opened(&api, &alan).await.last_event_id;
    // Every role holds workspace.read, which workspace.updated requires: no member lacks it.
    let mut phone = streams::opened(&api, &ada).await;
    let mut laptop = streams::opened(&api, &ada_laptop).await;
    let mut member = streams::opened(&api, &grace).await;

    let renamed = rename(&api, &ada, &workspace, "Acme Labs", None).await;
    workspaces::join(db, &workspace, &alan.user_id, "member").await;
    let mut joined = streams::connect(&api, &alan.access, before.as_deref()).await;
    joined.opened().await;
    let again = rename(&api, &ada, &workspace, "Acme Works", None).await;
    let after_joining = joined.event().await;

    assert_eq!(renamed.status, 200);
    assert_eq!(again.status, 200);
    let mut second = Vec::new();
    for stream in [&mut phone, &mut laptop, &mut member] {
        for _ in 0..2 {
            let event = stream.event().await;
            assert_eq!(event.event.as_deref(), Some("workspace.updated"));
            assert_eq!(event.payload(), &json!({ "workspace_id": workspace }));
            second.push(event.id);
        }
    }
    // The rename from before Alan joined isn't replayed to him: the first he receives is the one
    // made after.
    assert_eq!(after_joining.event.as_deref(), Some("workspace.updated"));
    assert_eq!(after_joining.id, second[1]);
}

/// Changes the role of `user` in `workspace` to `role`, as `caller`, with `if_match` as its
/// `If-Match` if given.
async fn change_role(
    api: &Api,
    caller: &SignedIn,
    (workspace, user): (&str, &str),
    role: &str,
    if_match: Option<&str>,
) -> crate::support::api::Response {
    let mut call = api
        .patch(
            &format!("/v1/workspaces/{workspace}/members/{user}"),
            &json!({ "role": role }),
        )
        .bearer(&caller.access);
    if let Some(tag) = if_match {
        call = call.header("if-match", tag);
    }
    call.send().await
}

/// Transfers `workspace` to `user`, as `caller`.
async fn transfer(
    api: &Api,
    caller: &SignedIn,
    workspace: &str,
    user: &str,
) -> crate::support::api::Response {
    api.post(
        &format!("/v1/workspaces/{workspace}/transfer"),
        &json!({ "user_id": user }),
    )
    .bearer(&caller.access)
    .send()
    .await
}

/// The members of `workspace`, each as their user ID and their role, in the order they joined.
async fn roles(api: &Api, workspace: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT user_id || ' ' || role FROM workspace_memberships \
             WHERE workspace_id = '{workspace}' ORDER BY created_at, id"
        ))
        .await
}

/// The entries of `action`, each as its scope, the log it is in, its actor, its target, its
/// resource, its details and its permission.
async fn entries(api: &Api, action: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT scope || ' ' || coalesce(workspace_id, user_id) || ' ' || actor_user_id || ' ' \
             || coalesce(target_user_id::text, '-') || ' ' || resource_type || ' ' || resource_id \
             || ' ' || details::text || ' ' || coalesce(permission, '-') \
             FROM audit_entries WHERE action = '{action}'"
        ))
        .await
}

/// Every event, as its name, its scope, its workspace, the permission it requires and its payload.
async fn published(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads(
            "SELECT name || ' ' || scope || ' ' || workspace_id || ' ' || permission || ' ' \
             || payload::text FROM events ORDER BY position",
        )
        .await
}

#[tokio::test]
async fn workspaces_011_reading_a_member_gives_them_as_the_list_does_with_their_etag() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let stranger = users::register(&api, "stranger@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(
        db,
        "Acme",
        &[(&ada.user_id, "owner"), (&grace.user_id, "member")],
    )
    .await;
    workspaces::arrange(db, "Other", &[(&stranger.user_id, "owner")]).await;
    let member = |user: &str| format!("/v1/workspaces/{workspace}/members/{user}");

    let read = call(&api, http::Method::GET, &member(&ada.user_id), &grace).await;
    let listed = call(
        &api,
        http::Method::GET,
        &format!("/v1/workspaces/{workspace}/members"),
        &grace,
    )
    .await;
    let elsewhere = call(&api, http::Method::GET, &member(&stranger.user_id), &grace).await;
    let not_an_id = call(&api, http::Method::GET, &member("nobody"), &grace).await;

    assert_eq!(read.status, 200);
    assert_eq!(read.header("etag"), "\"1\"");
    let body = read.json();
    assert_eq!(body["user_id"], ada.user_id);
    assert_eq!(body["email"], "ada@example.com");
    assert_eq!(body["role"], "owner");
    let listed = listed.json();
    let item = listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["user_id"] == ada.user_id)
        .unwrap();
    assert_eq!(&body, item);
    for missing in [&elsewhere, &not_an_id] {
        assert_eq!(missing.status, 404);
        assert_eq!(missing.json()["code"], "workspaces.member_not_found");
    }
}

#[tokio::test]
async fn workspaces_012_changing_a_role_stores_it_records_it_publishes_it_and_signals_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(
        db,
        "Acme",
        &[(&ada.user_id, "owner"), (&grace.user_id, "member")],
    )
    .await;
    // The registrations' sessions published their starts (AUTH-046).
    db.owner_executes("DELETE FROM events").await;
    let mut signals = db.signals().await;

    let changed = change_role(&api, &ada, (&workspace, &grace.user_id), "admin", None).await;

    assert_eq!(changed.status, 200);
    assert_eq!(changed.header("etag"), "\"2\"");
    let body = changed.json();
    assert_eq!(body["user_id"], grace.user_id);
    assert_eq!(body["email"], "grace@example.com");
    assert_eq!(body["role"], "admin");
    let read = call(
        &api,
        http::Method::GET,
        &format!("/v1/workspaces/{workspace}/members/{}", grace.user_id),
        &ada,
    )
    .await;
    assert_eq!(read.json(), body);
    assert_eq!(read.header("etag"), "\"2\"");
    assert_eq!(
        roles(&api, &workspace).await,
        [
            format!("{} owner", ada.user_id),
            format!("{} admin", grace.user_id)
        ]
    );
    assert_eq!(
        entries(&api, "member.role_changed").await,
        [format!(
            "workspace {workspace} {} {} workspace {workspace} \
             {{\"to\": \"admin\", \"from\": \"member\"}} members.read",
            ada.user_id, grace.user_id
        )]
    );
    assert_eq!(
        published(&api).await,
        [format!(
            "member.role_changed workspace {workspace} members.read \
             {{\"role\": \"admin\", \"user_id\": \"{}\", \"workspace_id\": \"{workspace}\"}}",
            grace.user_id
        )]
    );
    // The signal names its transaction, the event's.
    let transaction: u64 = db.owner_reads("SELECT xact::text FROM events").await[0]
        .parse()
        .unwrap();
    assert_eq!(
        signals.received().await,
        [workspaces::signal(&grace.user_id, &workspace, transaction)]
    );
}

#[tokio::test]
async fn workspaces_012_a_role_that_the_member_holds_already_changes_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(
        db,
        "Acme",
        &[(&ada.user_id, "owner"), (&grace.user_id, "member")],
    )
    .await;
    db.owner_executes("DELETE FROM events").await;
    let mut signals = db.signals().await;

    let unchanged = change_role(&api, &ada, (&workspace, &grace.user_id), "member", None).await;

    assert_eq!(unchanged.status, 200);
    assert_eq!(unchanged.header("etag"), "\"1\"");
    assert_eq!(unchanged.json()["role"], "member");
    assert_eq!(unchanged.json()["user_id"], grace.user_id);
    assert!(entries(&api, "member.role_changed").await.is_empty());
    assert!(published(&api).await.is_empty());
    assert!(signals.received().await.is_empty());
    let versions = db
        .owner_reads("SELECT version::text FROM workspace_memberships ORDER BY created_at, id")
        .await;
    assert_eq!(versions, ["1", "1"]);
}

#[tokio::test]
async fn workspaces_013_a_caller_changes_only_a_lower_rank_and_only_to_a_role_below_their_own() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let admin = users::register(&api, "admin@example.com").await;
    let peer = users::register(&api, "peer@example.com").await;
    let member = users::register(&api, "member@example.com").await;
    let stranger = users::register(&api, "stranger@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[
            (&owner.user_id, "owner"),
            (&admin.user_id, "admin"),
            (&peer.user_id, "admin"),
            (&member.user_id, "member"),
        ],
    )
    .await;
    let before = roles(&api, &workspace).await;
    let change = |caller: &SignedIn, user: &str, role: &'static str| {
        let target = format!("/v1/workspaces/{workspace}/members/{user}");
        let call = api.patch(&target, &json!({ "role": role }));
        call.bearer(&caller.access).send()
    };

    let refused = [
        // Their own role, an equal's and the owner's.
        change(&admin, &admin.user_id, "member").await,
        change(&admin, &peer.user_id, "member").await,
        change(&admin, &owner.user_id, "member").await,
        change(&owner, &owner.user_id, "admin").await,
        // A role that doesn't rank below the caller's own: an equal one, and a higher, whether
        // or not the user is a member.
        change(&admin, &member.user_id, "admin").await,
        change(&admin, &member.user_id, "owner").await,
        change(&owner, &admin.user_id, "owner").await,
        change(&owner, &stranger.user_id, "owner").await,
        // A member holds no members.update.
        change(&member, &member.user_id, "member").await,
    ];

    for response in &refused {
        assert_eq!(response.status, 403);
        assert_eq!(response.json()["code"], "request.permission_denied");
    }
    assert_eq!(roles(&api, &workspace).await, before);
    assert!(entries(&api, "member.role_changed").await.is_empty());
    // Each refusal is recorded in the workspace's log (PLATFORM-010, AUDIT-004).
    let recorded = api
        .database()
        .owner_reads(&format!(
            "SELECT count(*)::text FROM audit_entries WHERE action = 'access.denied' \
             AND workspace_id = '{workspace}' AND details ->> 'permission' = 'members.update'"
        ))
        .await;
    assert_eq!(recorded, [refused.len().to_string()]);
}

#[tokio::test]
async fn workspaces_013_a_change_names_a_member_a_current_version_and_a_role() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let member = users::register(&api, "member@example.com").await;
    let stranger = users::register(&api, "stranger@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[(&owner.user_id, "owner"), (&member.user_id, "member")],
    )
    .await;
    let before = roles(&api, &workspace).await;
    let target = (workspace.as_str(), member.user_id.as_str());

    let not_member =
        change_role(&api, &owner, (&workspace, &stranger.user_id), "admin", None).await;
    // A path whose user ID isn't a UUID names no member, whatever the role.
    let not_an_id = change_role(&api, &owner, (&workspace, "nobody"), "owner", None).await;
    let stale = change_role(&api, &owner, target, "admin", Some("\"7\"")).await;
    // A change to the role they hold fails a stale precondition too.
    let stale_unchanged = change_role(&api, &owner, target, "member", Some("\"7\"")).await;
    let not_a_role = api
        .request(
            http::Method::PATCH,
            &format!("/v1/workspaces/{workspace}/members/{}", member.user_id),
        )
        .header("content-type", "application/merge-patch+json")
        .body(r#"{"role": "guest"}"#)
        .bearer(&owner.access)
        .send()
        .await;

    for missing in [&not_member, &not_an_id] {
        assert_eq!(missing.status, 404);
        assert_eq!(missing.json()["code"], "workspaces.member_not_found");
    }
    for failed in [&stale, &stale_unchanged] {
        assert_eq!(failed.status, 412);
        assert_eq!(failed.json()["code"], "request.precondition_failed");
    }
    assert_eq!(not_a_role.status, 400);
    assert_eq!(not_a_role.json()["code"], "request.invalid_fields");
    assert_eq!(not_a_role.json()["errors"][0]["field"], "/role");
    assert_eq!(roles(&api, &workspace).await, before);
}

#[tokio::test]
async fn workspaces_013_a_change_that_names_the_current_version_applies() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let member = users::register(&api, "member@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[(&owner.user_id, "owner"), (&member.user_id, "member")],
    )
    .await;
    let target = (workspace.as_str(), member.user_id.as_str());

    let current = change_role(&api, &owner, target, "admin", Some("\"7\", \"1\"")).await;

    assert_eq!(current.status, 200);
    assert_eq!(current.json()["role"], "admin");
    assert_eq!(current.header("etag"), "\"2\"");
}

#[tokio::test]
async fn workspaces_012_a_changed_role_closes_the_member_s_streams_which_open_again_under_it() {
    let routes = crate::support::announcements::routes();
    let api = Api::with(crate::support::api::Options::routes(routes)).await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[(&ada.user_id, "owner"), (&grace.user_id, "member")],
    )
    .await;
    // An announcement to those who may remove members, which a member may not, or to everyone.
    let announce = |restricted: bool| {
        crate::support::announcements::announce_in(&api, &ada, &workspace, 1, restricted)
    };
    let mut stream = streams::opened(&api, &grace).await;
    announce(true).await;
    announce(false).await;
    // As a member, Grace receives the second alone.
    let as_member = stream.event().await;

    let changed = change_role(&api, &ada, (&workspace, &grace.user_id), "admin", None).await;
    let (_, reason) = stream.closed().await;
    let last = stream.last_event_id.as_deref();
    let mut reopened = streams::connect(&api, &grace.access, last).await;
    reopened.opened().await;
    announce(true).await;
    let replayed = reopened.event().await;
    let as_admin = reopened.event().await;

    assert_eq!(as_member.event.as_deref(), Some("announcement.posted"));
    assert_eq!(changed.status, 200);
    assert_eq!(reason, "access_changed");
    // The stream that opens again replays the change, and receives what an admin may.
    assert_eq!(replayed.event.as_deref(), Some("member.role_changed"));
    assert_eq!(as_admin.event.as_deref(), Some("announcement.restricted"));
}

#[tokio::test]
async fn access_006_changing_a_role_closes_the_member_s_streams() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[(&ada.user_id, "owner"), (&grace.user_id, "member")],
    )
    .await;
    let mut stream = streams::opened(&api, &grace).await;

    let changed = change_role(&api, &ada, (&workspace, &grace.user_id), "admin", None).await;
    let (_, reason) = stream.closed().await;

    assert_eq!(changed.status, 200);
    assert_eq!(reason, "access_changed");
}

#[tokio::test]
async fn sec_19_member_role_changed_reaches_the_members_and_no_replay_from_before_joining() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let alan = users::register(&api, "alan@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(
        db,
        "Acme",
        &[
            (&ada.user_id, "owner"),
            (&grace.user_id, "member"),
            (&bob.user_id, "member"),
        ],
    )
    .await;
    // An event to Alan alone gives his stream one to resume after.
    db.owner_executes(&events::statement(
        &[Event::to_user(&alan.user_id, "note.made", "{}")],
        true,
    ))
    .await;
    let before = streams::opened(&api, &alan).await.last_event_id;
    // Every role holds members.read, which member.role_changed requires: no member lacks it.
    let mut member = streams::opened(&api, &bob).await;

    let first = change_role(&api, &ada, (&workspace, &grace.user_id), "admin", None).await;
    workspaces::join(db, &workspace, &alan.user_id, "member").await;
    let mut joined = streams::connect(&api, &alan.access, before.as_deref()).await;
    joined.opened().await;
    let second = change_role(&api, &ada, (&workspace, &grace.user_id), "member", None).await;
    let after_joining = joined.event().await;

    assert_eq!(first.status, 200);
    assert_eq!(second.status, 200);
    for role in ["admin", "member"] {
        let event = member.event().await;
        assert_eq!(event.event.as_deref(), Some("member.role_changed"));
        assert_eq!(
            event.payload(),
            &json!({ "workspace_id": workspace, "user_id": grace.user_id, "role": role })
        );
    }
    // The change from before Alan joined isn't replayed to him: the first he receives is the one
    // made after.
    assert_eq!(after_joining.event.as_deref(), Some("member.role_changed"));
    assert_eq!(after_joining.payload()["role"], "member");
}

#[tokio::test]
async fn workspaces_014_leaving_ends_the_membership_records_it_publishes_it_and_signals_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(
        db,
        "Acme",
        &[(&ada.user_id, "owner"), (&grace.user_id, "member")],
    )
    .await;
    db.owner_executes("DELETE FROM events").await;
    let mut signals = db.signals().await;

    let left = call(
        &api,
        http::Method::POST,
        &format!("/v1/workspaces/{workspace}/leave"),
        &grace,
    )
    .await;

    assert_eq!(left.status, 204);
    assert_eq!(
        roles(&api, &workspace).await,
        [format!("{} owner", ada.user_id)]
    );
    assert_eq!(
        entries(&api, "member.left").await,
        [format!(
            "workspace {workspace} {0} {0} workspace {workspace} {{}} members.read",
            grace.user_id
        )]
    );
    assert_eq!(
        published(&api).await,
        [format!(
            "member.left workspace {workspace} members.read \
             {{\"user_id\": \"{}\", \"workspace_id\": \"{workspace}\"}}",
            grace.user_id
        )]
    );
    let transaction: u64 = db.owner_reads("SELECT xact::text FROM events").await[0]
        .parse()
        .unwrap();
    assert_eq!(
        signals.received().await,
        [workspaces::signal(&grace.user_id, &workspace, transaction)]
    );
}

#[tokio::test]
async fn workspaces_014_leaving_closes_the_leaver_s_streams() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[(&ada.user_id, "owner"), (&grace.user_id, "member")],
    )
    .await;
    let mut stream = streams::opened(&api, &grace).await;

    let left = call(
        &api,
        http::Method::POST,
        &format!("/v1/workspaces/{workspace}/leave"),
        &grace,
    )
    .await;
    let (_, reason) = stream.closed().await;

    assert_eq!(left.status, 204);
    assert_eq!(reason, "access_changed");
}

#[tokio::test]
async fn workspaces_014_the_owner_may_not_leave_and_an_admin_may() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let admin = users::register(&api, "admin@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[(&owner.user_id, "owner"), (&admin.user_id, "admin")],
    )
    .await;
    let path = format!("/v1/workspaces/{workspace}/leave");

    let refused = call(&api, http::Method::POST, &path, &owner).await;
    let left = call(&api, http::Method::POST, &path, &admin).await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "request.permission_denied");
    assert_eq!(left.status, 204);
    assert_eq!(
        roles(&api, &workspace).await,
        [format!("{} owner", owner.user_id)]
    );
}

#[tokio::test]
async fn workspaces_015_transferring_swaps_the_roles_records_it_publishes_it_and_signals_both() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(
        db,
        "Acme",
        &[
            (&ada.user_id, "owner"),
            (&grace.user_id, "admin"),
            (&bob.user_id, "member"),
        ],
    )
    .await;
    db.owner_executes("DELETE FROM events").await;
    let mut signals = db.signals().await;

    let transferred = transfer(&api, &ada, &workspace, &grace.user_id).await;

    assert_eq!(transferred.status, 204);
    assert_eq!(
        roles(&api, &workspace).await,
        [
            format!("{} admin", ada.user_id),
            format!("{} owner", grace.user_id),
            format!("{} member", bob.user_id)
        ]
    );
    assert_eq!(
        entries(&api, "workspace.transferred").await,
        [format!(
            "workspace {workspace} {} {} workspace {workspace} {{}} members.read",
            ada.user_id, grace.user_id
        )]
    );
    assert_eq!(
        published(&api).await,
        [format!(
            "workspace.transferred workspace {workspace} members.read \
             {{\"owner_id\": \"{}\", \"workspace_id\": \"{workspace}\", \
             \"previous_owner_id\": \"{}\"}}",
            grace.user_id, ada.user_id
        )]
    );
    let transaction: u64 = db.owner_reads("SELECT xact::text FROM events").await[0]
        .parse()
        .unwrap();
    assert_eq!(
        signals.received().await,
        [
            workspaces::signal(&ada.user_id, &workspace, transaction),
            workspaces::signal(&grace.user_id, &workspace, transaction)
        ]
    );
    // The workspace's own row didn't change: its version is the one it was created with.
    let read = call(
        &api,
        http::Method::GET,
        &format!("/v1/workspaces/{workspace}"),
        &ada,
    )
    .await;
    assert_eq!(read.json()["role"], "admin");
    assert_eq!(read.header("etag"), "\"1\"");
}

#[tokio::test]
async fn workspaces_015_a_transfer_closes_the_streams_of_the_two_whose_roles_it_swaps() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[(&ada.user_id, "owner"), (&grace.user_id, "admin")],
    )
    .await;
    let mut before = streams::opened(&api, &ada).await;
    let mut after = streams::opened(&api, &grace).await;

    let transferred = transfer(&api, &ada, &workspace, &grace.user_id).await;
    let (_, former_owner) = before.closed().await;
    let (_, new_owner) = after.closed().await;

    assert_eq!(transferred.status, 204);
    assert_eq!(former_owner, "access_changed");
    assert_eq!(new_owner, "access_changed");
}

#[tokio::test]
async fn workspaces_015_an_admin_or_a_member_may_not_transfer() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let admin = users::register(&api, "admin@example.com").await;
    let peer = users::register(&api, "peer@example.com").await;
    let member = users::register(&api, "member@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[
            (&owner.user_id, "owner"),
            (&admin.user_id, "admin"),
            (&peer.user_id, "admin"),
            (&member.user_id, "member"),
        ],
    )
    .await;
    let before = roles(&api, &workspace).await;

    let by_admin = transfer(&api, &admin, &workspace, &peer.user_id).await;
    let by_member = transfer(&api, &member, &workspace, &peer.user_id).await;

    for refused in [&by_admin, &by_member] {
        assert_eq!(refused.status, 403);
        assert_eq!(refused.json()["code"], "request.permission_denied");
    }
    assert_eq!(roles(&api, &workspace).await, before);
}

#[tokio::test]
async fn workspaces_016_a_transfer_goes_only_to_a_member_who_is_an_admin() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let admin = users::register(&api, "admin@example.com").await;
    let member = users::register(&api, "member@example.com").await;
    let stranger = users::register(&api, "stranger@example.com").await;
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
    // An admin of another workspace is no member of this one.
    workspaces::arrange(db, "Other", &[(&stranger.user_id, "owner")]).await;
    let before = roles(&api, &workspace).await;

    let to_member = transfer(&api, &owner, &workspace, &member.user_id).await;
    let to_myself = transfer(&api, &owner, &workspace, &owner.user_id).await;
    let to_stranger = transfer(&api, &owner, &workspace, &stranger.user_id).await;
    let not_an_id = api
        .post_raw(
            &format!("/v1/workspaces/{workspace}/transfer"),
            &json!({ "user_id": "nobody" }),
        )
        .bearer(&owner.access)
        .send()
        .await;

    for refused in [&to_member, &to_myself] {
        assert_eq!(refused.status, 422);
        assert_eq!(refused.json()["code"], "workspaces.transfer_requires_admin");
    }
    assert_eq!(to_stranger.status, 404);
    assert_eq!(to_stranger.json()["code"], "workspaces.member_not_found");
    assert_eq!(not_an_id.status, 400);
    assert_eq!(not_an_id.json()["code"], "request.invalid_fields");
    assert_eq!(not_an_id.json()["errors"][0]["field"], "/user_id");
    assert_eq!(roles(&api, &workspace).await, before);
    assert!(entries(&api, "workspace.transferred").await.is_empty());
}

#[tokio::test]
async fn sec_19_workspace_transferred_reaches_the_members_and_no_replay_from_before_joining() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let alan = users::register(&api, "alan@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(
        db,
        "Acme",
        &[
            (&ada.user_id, "owner"),
            (&grace.user_id, "admin"),
            (&bob.user_id, "member"),
        ],
    )
    .await;
    // An event to Alan alone gives his stream one to resume after.
    db.owner_executes(&events::statement(
        &[Event::to_user(&alan.user_id, "note.made", "{}")],
        true,
    ))
    .await;
    let before = streams::opened(&api, &alan).await.last_event_id;
    // Every role holds members.read, which workspace.transferred requires: no member lacks it.
    let mut member = streams::opened(&api, &bob).await;

    let first = transfer(&api, &ada, &workspace, &grace.user_id).await;
    workspaces::join(db, &workspace, &alan.user_id, "member").await;
    let mut joined = streams::connect(&api, &alan.access, before.as_deref()).await;
    joined.opened().await;
    let second = transfer(&api, &grace, &workspace, &ada.user_id).await;
    let after_joining = joined.event().await;

    assert_eq!(first.status, 204);
    assert_eq!(second.status, 204);
    for (owner, previous) in [(&grace, &ada), (&ada, &grace)] {
        let event = member.event().await;
        assert_eq!(event.event.as_deref(), Some("workspace.transferred"));
        assert_eq!(
            event.payload(),
            &json!({
                "workspace_id": workspace,
                "owner_id": owner.user_id,
                "previous_owner_id": previous.user_id
            })
        );
    }
    // The transfer from before Alan joined isn't replayed to him: the first he receives is the
    // one made after.
    assert_eq!(
        after_joining.event.as_deref(),
        Some("workspace.transferred")
    );
    assert_eq!(after_joining.payload()["owner_id"], ada.user_id);
}

#[tokio::test]
async fn workspaces_017_deleting_a_workspace_deletes_everything_in_it_and_records_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let db = api.database();
    let members = [
        (ada.user_id.as_str(), "owner"),
        (grace.user_id.as_str(), "admin"),
        (bob.user_id.as_str(), "member"),
    ];
    let workspace = workspaces::arrange(db, "Acme", &members).await;
    let other = workspaces::arrange(db, "Other", &members[..2]).await;
    // Each workspace has an entry in its log, an event and a job enqueued for it.
    for id in [&workspace, &other] {
        workspaces::fill(db, id, &ada.user_id).await;
    }
    let mut signals = db.signals().await;
    let path = format!("/v1/workspaces/{workspace}");

    let deleted = call(&api, http::Method::DELETE, &path, &ada).await;

    assert_eq!(deleted.status, 204);
    assert_eq!(workspaces::held_by(db, &workspace).await, "0 0 0 0 0");
    assert_eq!(workspaces::held_by(db, &other).await, "1 2 1 1 1");
    // The owner's own log keeps the deletion, since the workspace's went with it.
    assert_eq!(
        entries(&api, "workspace.deleted").await,
        [format!(
            "user {0} {0} - workspace {workspace} {{}} -",
            ada.user_id
        )]
    );
    // Each member's signal names the deletion's transaction: the owner's first, then each rank's.
    let received = signals.received().await;
    let transaction = received[0]["transaction"].as_u64().unwrap();
    let expected = members.map(|(user, _)| workspaces::signal(user, &workspace, transaction));
    assert_eq!(received, expected);
    assert_eq!(call(&api, http::Method::GET, &path, &ada).await.status, 404);
}

#[tokio::test]
async fn workspaces_017_every_table_that_names_a_workspace_goes_with_it() {
    let api = Api::new().await;

    // A foreign key to `workspaces` that didn't cascade would keep a workspace from being
    // deleted, or leave its rows behind.
    let kept = api
        .database()
        .owner_reads(
            "SELECT conrelid::regclass || '.' || conname FROM pg_constraint \
             WHERE contype = 'f' AND confrelid = 'workspaces'::regclass AND confdeltype <> 'c'",
        )
        .await;

    assert!(kept.is_empty(), "{kept:?}");
}

#[tokio::test]
async fn workspaces_017_deleting_a_workspace_closes_each_member_s_streams() {
    let api = Api::new().await;
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
    let mut admin = streams::opened(&api, &grace).await;
    let mut member = streams::opened(&api, &bob).await;

    let deleted = call(
        &api,
        http::Method::DELETE,
        &format!("/v1/workspaces/{workspace}"),
        &ada,
    )
    .await;
    let (_, admin_reason) = admin.closed().await;
    let (_, member_reason) = member.closed().await;

    assert_eq!(deleted.status, 204);
    assert_eq!(admin_reason, "access_changed");
    assert_eq!(member_reason, "access_changed");
}

#[tokio::test]
async fn workspaces_017_an_admin_or_a_member_may_not_delete() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let admin = users::register(&api, "admin@example.com").await;
    let member = users::register(&api, "member@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[
            (&owner.user_id, "owner"),
            (&admin.user_id, "admin"),
            (&member.user_id, "member"),
        ],
    )
    .await;
    let path = format!("/v1/workspaces/{workspace}");

    let by_admin = call(&api, http::Method::DELETE, &path, &admin).await;
    let by_member = call(&api, http::Method::DELETE, &path, &member).await;

    for refused in [&by_admin, &by_member] {
        assert_eq!(refused.status, 403);
        assert_eq!(refused.json()["code"], "request.permission_denied");
    }
    assert_eq!(roles(&api, &workspace).await.len(), 3);
}

#[tokio::test]
async fn workspaces_018_a_transfer_and_a_deletion_need_a_proof_from_the_last_5_minutes() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let admin = users::register(&api, "admin@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[(&owner.user_id, "owner"), (&admin.user_id, "admin")],
    )
    .await;
    let path = format!("/v1/workspaces/{workspace}");
    let before = roles(&api, &workspace).await;
    users::authenticated_ago(&api, &owner, 6).await;
    users::authenticated_ago(&api, &admin, 6).await;

    let transfer_refused = transfer(&api, &owner, &workspace, &admin.user_id).await;
    let deletion_refused = call(&api, http::Method::DELETE, &path, &owner).await;
    // A caller who may not delete learns that, whatever their session proved.
    let not_allowed = call(&api, http::Method::DELETE, &path, &admin).await;

    for refused in [&transfer_refused, &deletion_refused] {
        assert_eq!(refused.status, 403);
        assert_eq!(refused.json()["code"], "request.reauthentication_required");
    }
    assert_eq!(not_allowed.json()["code"], "request.permission_denied");
    assert_eq!(roles(&api, &workspace).await, before);
    assert!(entries(&api, "workspace.transferred").await.is_empty());
    assert!(entries(&api, "workspace.deleted").await.is_empty());
}

#[tokio::test]
async fn workspaces_018_a_proof_from_within_the_5_minutes_lets_each_through() {
    let api = Api::new().await;
    let owner = users::register(&api, "owner@example.com").await;
    let admin = users::register(&api, "admin@example.com").await;
    let workspace = workspaces::arrange(
        api.database(),
        "Acme",
        &[(&owner.user_id, "owner"), (&admin.user_id, "admin")],
    )
    .await;
    users::authenticated_ago(&api, &owner, 4).await;
    users::authenticated_ago(&api, &admin, 4).await;

    let transferred = transfer(&api, &owner, &workspace, &admin.user_id).await;
    // The admin owns the workspace, and deletes it.
    let path = format!("/v1/workspaces/{workspace}");
    let deleted = call(&api, http::Method::DELETE, &path, &admin).await;

    assert_eq!(transferred.status, 204);
    assert_eq!(deleted.status, 204);
}
