//! Workspaces and their members (the workspaces spec), with the lists' mechanisms they are the
//! first to use (the platform spec).

use serde_json::{Value, json};

use crate::support::{
    Api,
    users::{self, SignedIn},
    workspaces,
};

/// Creates a workspace named `name` as `user`, with `key` as its idempotency key if given.
async fn create(
    api: &Api,
    user: &SignedIn,
    name: &str,
    key: Option<&str>,
) -> crate::support::api::Response {
    let mut call = api
        .post("/v1/workspaces", &json!({ "name": name }))
        .bearer(&user.access);
    if let Some(key) = key {
        call = call.header("idempotency-key", key);
    }
    call.send().await
}

/// The values of `field` in each item of a list's body.
fn field(body: &Value, field: &str) -> Vec<String> {
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item[field].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn workspaces_001_creating_a_workspace_makes_the_caller_its_owner() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = create(&api, &ada, "Acme", None).await;

    assert_eq!(response.status, 201);
    let body = response.json();
    let id = body["id"].as_str().unwrap();
    assert_eq!(response.header("location"), format!("/v1/workspaces/{id}"));
    assert_eq!(body["name"], "Acme");
    assert_eq!(body["role"], "owner");
    assert!(body["created_at"].as_str().unwrap().ends_with('Z'));
    let db = api.database();
    let members = db
        .owner_reads("SELECT user_id || ' ' || role FROM workspace_memberships")
        .await;
    assert_eq!(members, [format!("{} owner", ada.user_id)]);
    let entries = db
        .owner_reads(
            "SELECT action || ' ' || scope || ' ' || workspace_id || ' ' || actor_user_id || ' ' \
             || resource_type || ' ' || resource_id || ' ' || details::text \
             FROM audit_entries WHERE action = 'workspace.created'",
        )
        .await;
    assert_eq!(
        entries,
        [format!(
            "workspace.created workspace {id} {} workspace {id} {{}}",
            ada.user_id
        )]
    );
    let events = db
        .owner_reads(
            "SELECT type || ' ' || scope || ' ' || user_id || ' ' || (payload->>'workspace_id')              || ' ' || (payload->>'session_id') FROM events",
        )
        .await;
    assert_eq!(
        events,
        [format!(
            "membership.created user {} {id} {}",
            ada.user_id, ada.session_id
        )]
    );
}

#[tokio::test]
async fn workspaces_002_a_name_is_stored_normalized_and_must_hold_a_character() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let created = create(&api, &ada, "  Acme \t  Labs ", None).await;
    let refused = create(&api, &ada, " \t ", None).await;

    assert_eq!(created.json()["name"], "Acme Labs");
    assert_eq!(refused.status, 400);
    assert_eq!(refused.json()["code"], "request.invalid_fields");
    assert_eq!(refused.json()["errors"][0]["field"], "/name");
    assert_eq!(refused.json()["errors"][0]["code"], "required");
}

#[tokio::test]
async fn workspaces_001_a_retry_with_the_key_creates_one_workspace() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let first = create(&api, &ada, "Acme", Some("\"k-1\"")).await;
    let retry = create(&api, &ada, "Acme", Some("\"k-1\"")).await;

    assert_eq!(first.status, 201);
    assert_eq!(retry.status, 201);
    assert_eq!(retry.json(), first.json());
    assert_eq!(retry.header("location"), first.header("location"));
    let count = api
        .database()
        .owner_reads("SELECT count(*)::text FROM workspaces")
        .await;
    assert_eq!(count, ["1"]);
}

/// The IDs, by `field`, of every page of the list at `path`, a path with a query, read as `user`
/// by following each page's `next_cursor`.
async fn pages(api: &Api, user: &SignedIn, path: &str, field_name: &str) -> Vec<Vec<String>> {
    let mut pages = Vec::new();
    let mut next = path.to_owned();
    loop {
        let body = api.get(&next).bearer(&user.access).send().await.json();
        pages.push(field(&body, field_name));
        match body["next_cursor"].as_str() {
            Some(cursor) => next = format!("{path}&cursor={cursor}"),
            None => return pages,
        }
    }
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
    assert_eq!(
        field(&body, "joined_at"),
        ["2026-11-01T11:00:00.500000Z", "2026-11-01T10:00:00.000000Z"]
    );
    assert_eq!(body["next_cursor"], Value::Null);
}

#[tokio::test]
async fn platform_059_paging_returns_each_row_once_across_a_shared_joining_time() {
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
async fn platform_059_a_limit_out_of_range_and_a_cursor_of_another_route_are_refused_together() {
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
async fn platform_060_a_search_matches_any_part_ignoring_case_in_every_script() {
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
async fn workspaces_005_removing_a_member_deletes_their_membership_and_signals_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    workspaces::join(db, &workspace, &grace.user_id, "member").await;
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
             FROM audit_entries WHERE action = 'member.removed'",
        )
        .await;
    assert_eq!(
        entries,
        [format!(
            "workspace {workspace} {} {1} member {1} {{}}",
            ada.user_id, grace.user_id
        )]
    );
    let events = db
        .owner_reads(
            "SELECT type || ' ' || scope || ' ' || workspace_id || ' ' || permission FROM events",
        )
        .await;
    assert_eq!(
        events,
        [format!("member.removed workspace {workspace} members.read")]
    );
    assert_eq!(
        signals.received().await,
        [
            json!({ "kind": "membership_ended", "user_id": grace.user_id, "workspace_id": workspace })
        ]
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
async fn platform_059_a_page_holds_50_items_without_a_limit() {
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
