//! Inviting someone by email, and the invitee seeing and accepting the invitation (the
//! invitations spec).

use serde_json::{Value, json};

use crate::support::{
    Api, Options,
    api::Response,
    invitations::invite,
    mailbox,
    users::{self, SignedIn},
    workspaces,
};

/// The caller's invitations, at `query`.
async fn received(api: &Api, user: &SignedIn, query: &str) -> Value {
    let response = api
        .get(&format!("/v1/users/me/invitations{query}"))
        .bearer(&user.access)
        .send()
        .await;
    assert_eq!(response.status, 200);
    response.json()
}

/// Accepts the invitation `id`, as `user`.
async fn accept(api: &Api, user: &SignedIn, id: &str) -> Response {
    api.request(
        http::Method::POST,
        &format!("/v1/users/me/invitations/{id}/accept"),
    )
    .bearer(&user.access)
    .send()
    .await
}

/// A workspace named Acme that `owner` owns, with `others` as its other members.
async fn acme(api: &Api, owner: &SignedIn, others: &[(&SignedIn, &str)]) -> String {
    let mut members = vec![(owner.user_id.as_str(), "owner")];
    members.extend(
        others
            .iter()
            .map(|(user, role)| (user.user_id.as_str(), *role)),
    );
    workspaces::arrange(api.database(), "Acme", &members).await
}

#[tokio::test]
async fn invitations_001_inviting_an_email_creates_a_pending_invitation_and_enqueues_its_email() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let invitee = mailbox::address("invitee");

    let response = invite(&api, &ada, &workspace, &invitee, "member").await;

    assert_eq!(response.status, 201);
    let body = response.json();
    let id = body["id"].as_str().unwrap();
    assert_eq!(
        response.header("location"),
        format!("/v1/workspaces/{workspace}/invitations/{id}")
    );
    assert_eq!(body["workspace_id"], workspace.as_str());
    assert_eq!(body["email"], invitee.as_str());
    assert_eq!(body["role"], "member");
    assert_eq!(body["inviter_id"], ada.user_id.as_str());
    let db = api.database();
    let lasts = db
        .owner_reads("SELECT (expires_at - created_at = interval '7 days')::text FROM invitations")
        .await;
    assert_eq!(lasts, ["true"]);
    let entries = db
        .owner_reads(
            "SELECT action || ' ' || scope || ' ' || workspace_id || ' ' || actor_user_id || ' ' \
             || resource_type || ' ' || resource_id || ' ' || details::text FROM audit_entries \
             WHERE action LIKE 'invitation.%'",
        )
        .await;
    assert_eq!(
        entries,
        [format!(
            "invitation.created workspace {workspace} {} invitation {id} {{\"role\": \"member\"}}",
            ada.user_id
        )]
    );
    let jobs = db
        .owner_reads("SELECT kind || ' ' || workspace_id || ' ' || user_id FROM jobs")
        .await;
    assert_eq!(jobs, [format!("mail.send {workspace} {}", ada.user_id)]);
    assert_eq!(api.run_jobs().await, 1);
    assert_eq!(mailbox::to(&invitee).await.len(), 1);
}

#[tokio::test]
async fn invitations_001_an_invitation_lasts_the_configured_days() {
    let api = Api::with(Options::default().set("INVITATION_EXPIRY_DAYS", 3)).await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;

    let response = invite(&api, &ada, &workspace, "bob@example.com", "member").await;

    assert_eq!(response.status, 201);
    let lasts = api
        .database()
        .owner_reads("SELECT (expires_at - created_at = interval '3 days')::text FROM invitations")
        .await;
    assert_eq!(lasts, ["true"]);
}

#[tokio::test]
async fn invitations_001_the_role_must_rank_below_the_inviters() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let workspace = acme(&api, &ada, &[(&bob, "admin")]).await;

    let admin_as_admin = invite(&api, &bob, &workspace, "carol@example.com", "admin").await;
    let admin_as_owner = invite(&api, &bob, &workspace, "carol@example.com", "owner").await;
    let owner_as_owner = invite(&api, &ada, &workspace, "carol@example.com", "owner").await;
    let admin_as_member = invite(&api, &bob, &workspace, "carol@example.com", "member").await;

    for refused in [&admin_as_admin, &admin_as_owner, &owner_as_owner] {
        assert_eq!(refused.status, 403);
        assert_eq!(refused.json()["code"], "request.permission_denied");
    }
    assert_eq!(admin_as_member.status, 201);
    let db = api.database();
    let invitations = db.owner_reads("SELECT role::text FROM invitations").await;
    assert_eq!(invitations, ["member"]);
    let refusals = db
        .owner_reads("SELECT count(*)::text FROM audit_entries WHERE action = 'access.denied'")
        .await;
    assert_eq!(refusals, ["3"]);
}

#[tokio::test]
async fn invitations_001_a_retry_with_the_same_key_answers_the_same_invitation_and_sends_one_email()
{
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let path = format!("/v1/workspaces/{workspace}/invitations");
    let body = json!({ "email": "bob@example.com", "role": "member" });

    let first = api
        .post(&path, &body)
        .bearer(&ada.access)
        .header("idempotency-key", "\"invite-bob\"")
        .send()
        .await;
    let retry = api
        .post(&path, &body)
        .bearer(&ada.access)
        .header("idempotency-key", "\"invite-bob\"")
        .send()
        .await;

    assert_eq!(first.status, 201);
    assert_eq!(retry.status, 201);
    assert_eq!(retry.json(), first.json());
    assert_eq!(retry.header("location"), first.header("location"));
    let jobs = api
        .database()
        .owner_reads("SELECT count(*)::text FROM jobs")
        .await;
    assert_eq!(jobs, ["1"]);
}

#[tokio::test]
async fn invitations_002_inviting_a_member_answers_409_and_writes_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let workspace = acme(&api, &ada, &[(&bob, "member")]).await;

    let response = invite(&api, &ada, &workspace, "Bob@Example.com", "member").await;

    assert_eq!(response.status, 409);
    assert_eq!(response.json()["code"], "invitations.already_member");
    let db = api.database();
    for written in [
        "SELECT count(*)::text FROM invitations",
        "SELECT count(*)::text FROM jobs",
        "SELECT count(*)::text FROM audit_entries WHERE action LIKE 'invitation.%'",
        "SELECT count(*)::text FROM events WHERE type LIKE 'invitation.%'",
    ] {
        assert_eq!(db.owner_reads(written).await, ["0"], "{written}");
    }
}

#[tokio::test]
async fn invitations_003_inviting_again_refreshes_the_invitation_in_place() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let workspace = acme(&api, &ada, &[(&bob, "admin")]).await;
    let first = invite(&api, &ada, &workspace, "carol@example.com", "admin").await;
    let id = first.json()["id"].as_str().unwrap().to_owned();
    let db = api.database();
    db.owner_executes("UPDATE invitations SET expires_at = now() + interval '1 day'")
        .await;

    let response = invite(&api, &bob, &workspace, "carol@example.com", "member").await;

    assert_eq!(response.status, 200);
    assert!(response.headers("location").is_empty());
    let body = response.json();
    assert_eq!(body["id"], id.as_str());
    assert_eq!(body["role"], "member");
    assert_eq!(body["inviter_id"], bob.user_id.as_str());
    let refreshed = db
        .owner_reads(
            "SELECT (expires_at > now() + interval '6 days')::text || ' ' || role::text \
             || ' ' || inviter_id FROM invitations",
        )
        .await;
    assert_eq!(refreshed, [format!("true member {}", bob.user_id)]);
    let entries = db
        .owner_reads(
            "SELECT action || ' ' || scope || ' ' || workspace_id || ' ' || actor_user_id || ' ' \
             || resource_type || ' ' || resource_id || ' ' \
             || coalesce(target_user_id::text, '-') || ' ' || details::text \
             FROM audit_entries WHERE action LIKE 'invitation.%' ORDER BY created_at, id",
        )
        .await;
    assert_eq!(
        entries,
        [
            format!(
                "invitation.created workspace {workspace} {} invitation {id} - {{\"role\": \"admin\"}}",
                ada.user_id
            ),
            format!(
                "invitation.updated workspace {workspace} {} invitation {id} - {{\"role\": \"member\"}}",
                bob.user_id
            ),
        ]
    );
    let jobs = db.owner_reads("SELECT count(*)::text FROM jobs").await;
    assert_eq!(jobs, ["2"]);
}

#[tokio::test]
async fn invitations_004_the_caller_lists_their_unexpired_invitations_in_every_workspace() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let db = api.database();
    let acme = acme(&api, &ada, &[]).await;
    let globex = workspaces::arrange(db, "Globex", &[(&ada.user_id, "owner")]).await;
    let initech = workspaces::arrange(db, "Initech", &[(&ada.user_id, "owner")]).await;
    let to_acme = invite(&api, &ada, &acme, "carol@example.com", "member")
        .await
        .json();
    let to_globex = invite(&api, &ada, &globex, "carol@example.com", "admin")
        .await
        .json();
    invite(&api, &ada, &initech, "carol@example.com", "member").await;
    invite(&api, &ada, &acme, "dave@example.com", "member").await;
    db.owner_executes(&format!(
        "UPDATE invitations SET expires_at = now() - interval '1 second' \
         WHERE workspace_id = '{initech}'"
    ))
    .await;

    let all = received(&api, &carol, "").await;
    let searched = received(&api, &carol, "?search=glob").await;
    let first_page = received(&api, &carol, "?limit=1").await;

    let items = all["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], to_globex["id"]);
    assert_eq!(items[1]["id"], to_acme["id"]);
    assert_eq!(items[0]["workspace_id"], globex.as_str());
    assert_eq!(items[0]["workspace_name"], "Globex");
    assert_eq!(items[0]["inviter_name"], "Ada Lovelace");
    assert_eq!(items[0]["role"], "admin");
    assert_eq!(items[0]["expires_at"], to_globex["expires_at"]);
    assert_eq!(all["next_cursor"], Value::Null);
    let searched: Vec<_> = searched["items"].as_array().unwrap().iter().collect();
    assert_eq!(searched.len(), 1);
    assert_eq!(searched[0]["workspace_name"], "Globex");
    assert_eq!(first_page["items"].as_array().unwrap().len(), 1);
    let cursor = first_page["next_cursor"].as_str().unwrap();
    let second_page = received(&api, &carol, &format!("?limit=1&cursor={cursor}")).await;
    assert_eq!(second_page["items"][0]["id"], to_acme["id"]);
}

#[tokio::test]
async fn invitations_005_accepting_adds_the_caller_and_deletes_the_invitation() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let invitation = invite(&api, &ada, &workspace, "carol@example.com", "admin")
        .await
        .json();
    let id = invitation["id"].as_str().unwrap();
    let db = api.database();
    db.owner_executes("DELETE FROM events").await;

    let response = accept(&api, &carol, id).await;

    assert_eq!(response.status, 200);
    assert_eq!(
        response.json(),
        json!({ "workspace_id": workspace, "role": "admin" })
    );
    let members = db
        .owner_reads(&format!(
            "SELECT role::text FROM workspace_memberships WHERE user_id = '{}'",
            carol.user_id
        ))
        .await;
    assert_eq!(members, ["admin"]);
    let left = db
        .owner_reads("SELECT count(*)::text FROM invitations")
        .await;
    assert_eq!(left, ["0"]);
    let entries = db
        .owner_reads(
            "SELECT scope || ' ' || workspace_id || ' ' || actor_user_id || ' ' || resource_type \
             || ' ' || resource_id || ' ' || coalesce(target_user_id::text, '-') || ' ' \
             || details::text FROM audit_entries WHERE action = 'invitation.accepted'",
        )
        .await;
    assert_eq!(
        entries,
        [format!(
            "workspace {workspace} {} invitation {id} - {{}}",
            carol.user_id
        )]
    );
    let events = db
        .owner_reads(
            "SELECT type || ' ' || scope || ' ' || coalesce(permission, '-') || ' ' || payload::text \
             FROM events ORDER BY created_at, id",
        )
        .await;
    assert_eq!(
        events,
        [
            format!(
                "member.joined workspace members.read {{\"user_id\": \"{}\", \"workspace_id\": \"{workspace}\"}}",
                carol.user_id
            ),
            format!(
                "membership.created user - {{\"session_id\": \"{}\", \"workspace_id\": \"{workspace}\"}}",
                carol.session_id
            ),
        ]
    );
}

#[tokio::test]
async fn invitations_006_an_expired_invitation_answers_422() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let invitation = invite(&api, &ada, &workspace, "carol@example.com", "member")
        .await
        .json();
    let db = api.database();
    db.owner_executes("UPDATE invitations SET expires_at = now() - interval '1 second'")
        .await;

    let response = accept(&api, &carol, invitation["id"].as_str().unwrap()).await;

    assert_eq!(response.status, 422);
    assert_eq!(response.json()["code"], "invitations.expired");
    let members = db
        .owner_reads("SELECT count(*)::text FROM workspace_memberships")
        .await;
    assert_eq!(members, ["1"]);
}

#[tokio::test]
async fn invitations_006_accepting_while_a_member_answers_409_and_changes_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let invitation = invite(&api, &ada, &workspace, "carol@example.com", "admin")
        .await
        .json();
    let db = api.database();
    workspaces::join(db, &workspace, &carol.user_id, "member").await;

    let response = accept(&api, &carol, invitation["id"].as_str().unwrap()).await;

    assert_eq!(response.status, 409);
    assert_eq!(response.json()["code"], "invitations.already_member");
    let role = db
        .owner_reads(&format!(
            "SELECT role::text FROM workspace_memberships WHERE user_id = '{}'",
            carol.user_id
        ))
        .await;
    assert_eq!(role, ["member"]);
    let left = db
        .owner_reads("SELECT count(*)::text FROM invitations")
        .await;
    assert_eq!(left, ["1"]);
}

#[tokio::test]
async fn invitations_006_an_invitation_not_addressed_to_the_caller_answers_404() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let dave = users::register(&api, "dave@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let invitation = invite(&api, &ada, &workspace, "carol@example.com", "member")
        .await
        .json();

    let not_theirs = accept(&api, &dave, invitation["id"].as_str().unwrap()).await;
    let not_a_uuid = accept(&api, &dave, "not-a-uuid").await;

    for response in [not_theirs, not_a_uuid] {
        assert_eq!(response.status, 404);
        assert_eq!(response.json()["code"], "invitations.not_found");
    }
    let left = api
        .database()
        .owner_reads("SELECT count(*)::text FROM invitations")
        .await;
    assert_eq!(left, ["1"]);
}

#[tokio::test]
async fn invitations_007_the_periodic_job_deletes_expired_invitations_of_every_workspace() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let acme = acme(&api, &ada, &[]).await;
    let globex = workspaces::arrange(db, "Globex", &[(&ada.user_id, "owner")]).await;
    for workspace in [&acme, &globex] {
        invite(&api, &ada, workspace, "carol@example.com", "member").await;
        invite(&api, &ada, workspace, "dave@example.com", "member").await;
    }
    db.owner_executes(
        "UPDATE invitations SET expires_at = now() - interval '1 second' \
         WHERE email = 'carol@example.com'",
    )
    .await;

    api.run_periodic("invitations.expire").await;

    let left = db
        .owner_reads("SELECT DISTINCT email FROM invitations")
        .await;
    assert_eq!(left, ["dave@example.com"]);
    let entries = db
        .owner_reads("SELECT count(*)::text FROM audit_entries WHERE action LIKE '%deleted'")
        .await;
    assert_eq!(entries, ["0"]);
}

#[tokio::test]
async fn invitations_008_an_invitee_with_an_account_receives_invitation_received() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let db = api.database();
    db.owner_executes("DELETE FROM events").await;

    let to_carol = invite(&api, &ada, &workspace, "carol@example.com", "member")
        .await
        .json();
    invite(&api, &ada, &workspace, "carol@example.com", "admin").await;
    invite(&api, &ada, &workspace, "nobody@example.com", "member").await;

    let events = db
        .owner_reads("SELECT type || ' ' || user_id || ' ' || payload::text FROM events")
        .await;
    let received = format!(
        "invitation.received {} {{\"workspace_id\": \"{workspace}\", \"invitation_id\": \"{}\"}}",
        carol.user_id,
        to_carol["id"].as_str().unwrap()
    );
    assert_eq!(events, [received.clone(), received]);
}

#[tokio::test]
async fn platform_012_a_job_runs_for_its_scope_after_its_member_loses_their_role() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let workspace = acme(&api, &ada, &[(&bob, "admin")]).await;
    let invitee = mailbox::address("invitee");
    invite(&api, &bob, &workspace, &invitee, "member").await;
    let removed = api
        .request(
            http::Method::DELETE,
            &format!("/v1/workspaces/{workspace}/members/{}", bob.user_id),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(removed.status, 204);

    let runs = api.run_jobs().await;

    assert_eq!(runs, 1);
    assert_eq!(mailbox::to(&invitee).await.len(), 1);
}

#[tokio::test]
async fn invitations_001_an_invitation_lasts_1_to_90_days() {
    for days in [0, 91] {
        let refused =
            crate::support::api::refusal(&Options::default().set("INVITATION_EXPIRY_DAYS", days));

        assert_eq!(
            refused,
            "INVITATION_EXPIRY_DAYS must be a whole number from 1 to 90"
        );
    }
}

#[tokio::test]
async fn invitations_004_a_page_boundary_between_invitations_created_at_once_loses_none() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let db = api.database();
    for name in ["Acme", "Globex", "Initech"] {
        let workspace = workspaces::arrange(db, name, &[(&ada.user_id, "owner")]).await;
        invite(&api, &ada, &workspace, "carol@example.com", "member").await;
    }
    db.owner_executes("UPDATE invitations SET created_at = '2026-11-01T12:00:00Z'")
        .await;

    let mut seen = Vec::new();
    let mut query = "?limit=1".to_owned();
    loop {
        let page = received(&api, &carol, &query).await;
        for item in page["items"].as_array().unwrap() {
            seen.push(item["workspace_name"].as_str().unwrap().to_owned());
        }
        match page["next_cursor"].as_str() {
            Some(cursor) => query = format!("?limit=1&cursor={cursor}"),
            None => break,
        }
    }

    seen.sort();
    assert_eq!(seen, ["Acme", "Globex", "Initech"]);
}

#[tokio::test]
async fn invitations_004_a_search_pages_through_its_matches() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let db = api.database();
    for name in ["Alpha One", "Beta", "Alpha Two"] {
        let workspace = workspaces::arrange(db, name, &[(&ada.user_id, "owner")]).await;
        invite(&api, &ada, &workspace, "carol@example.com", "member").await;
    }

    let first = received(&api, &carol, "?search=alpha&limit=1").await;
    let cursor = first["next_cursor"].as_str().unwrap();
    let second = received(
        &api,
        &carol,
        &format!("?search=alpha&limit=1&cursor={cursor}"),
    )
    .await;

    assert_eq!(first["items"][0]["workspace_name"], "Alpha Two");
    assert_eq!(second["items"][0]["workspace_name"], "Alpha One");
    assert_eq!(second["items"].as_array().unwrap().len(), 1);
    assert_eq!(second["next_cursor"], Value::Null);
}

#[tokio::test]
async fn invitations_007_a_second_run_of_the_sweep_changes_nothing_more() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let workspace = acme(&api, &ada, &[]).await;
    invite(&api, &ada, &workspace, "carol@example.com", "member").await;
    invite(&api, &ada, &workspace, "dave@example.com", "member").await;
    db.owner_executes(
        "UPDATE invitations SET expires_at = now() - interval '1 second' \
         WHERE email = 'carol@example.com'",
    )
    .await;
    api.run_periodic("invitations.expire").await;
    let once = db.owner_reads("SELECT email FROM invitations").await;

    api.run_periodic("invitations.expire").await;

    assert_eq!(db.owner_reads("SELECT email FROM invitations").await, once);
    assert_eq!(once, ["dave@example.com"]);
}

#[tokio::test]
async fn invitations_009_beyond_its_limit_a_member_s_invitation_answers_429() {
    let api = Api::with(Options::default().set("INVITATION_LIMIT", 2)).await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let workspace = acme(&api, &ada, &[(&bob, "member")]).await;

    // A request that answers otherwise still counts.
    let member = invite(&api, &ada, &workspace, "bob@example.com", "member").await;
    let second = invite(&api, &ada, &workspace, "carol@example.com", "member").await;
    let third = invite(&api, &ada, &workspace, "dave@example.com", "member").await;

    assert_eq!(member.status, 409);
    assert_eq!(second.status, 201);
    assert_eq!(third.status, 429);
    assert_eq!(third.json()["code"], "request.rate_limited");
    let invited = api
        .database()
        .owner_reads("SELECT email FROM invitations")
        .await;
    assert_eq!(invited, ["carol@example.com"]);
}

#[tokio::test]
async fn invitations_009_the_limit_and_its_window_are_checked() {
    for (setting, value, refused) in [
        (
            "INVITATION_LIMIT",
            0,
            "INVITATION_LIMIT must be a whole number from 1 to 100000",
        ),
        (
            "INVITATION_WINDOW_SECONDS",
            86_401,
            "INVITATION_WINDOW_SECONDS must be a whole number from 1 to 86400",
        ),
    ] {
        let refusal = crate::support::api::refusal(&Options::default().set(setting, value));

        assert_eq!(refusal, refused);
    }
}
