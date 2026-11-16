//! What a workspace's members do with its invitations: reading and listing them, and changing,
//! sending again and cancelling one (the invitations spec).

use http::Method;
use serde_json::{Value, json};

use super::acme;
use crate::support::{
    Api, Options,
    api::Response,
    invitations::invite,
    lists, mailbox, streams,
    users::{self, SignedIn},
    workspaces,
};

/// The path of the invitation `id` of `workspace`.
fn path(workspace: &str, id: &str) -> String {
    format!("/v1/workspaces/{workspace}/invitations/{id}")
}

/// Reads the invitation `id` of `workspace`, as `user`.
async fn read(api: &Api, user: &SignedIn, workspace: &str, id: &str) -> Response {
    api.get(&path(workspace, id))
        .bearer(&user.access)
        .send()
        .await
}

/// Changes the role of the invitation `id` of `workspace` to `role`, as `user`.
async fn change_role(
    api: &Api,
    user: &SignedIn,
    workspace: &str,
    id: &str,
    role: &str,
) -> Response {
    api.patch(&path(workspace, id), &json!({ "role": role }))
        .bearer(&user.access)
        .send()
        .await
}

/// Sends the invitation `id` of `workspace` again, as `user`.
async fn resend(api: &Api, user: &SignedIn, workspace: &str, id: &str) -> Response {
    api.request(Method::POST, &format!("{}/resend", path(workspace, id)))
        .bearer(&user.access)
        .send()
        .await
}

/// Cancels the invitation `id` of `workspace`, as `user`.
async fn cancel(api: &Api, user: &SignedIn, workspace: &str, id: &str) -> Response {
    api.request(Method::DELETE, &path(workspace, id))
        .bearer(&user.access)
        .send()
        .await
}

/// Invites `email` to `workspace` at `role`, as `user`, and gives the invitation's ID.
async fn invited(api: &Api, user: &SignedIn, workspace: &str, email: &str, role: &str) -> String {
    let response = invite(api, user, workspace, email, role).await;
    assert_eq!(response.status, 201);
    response.json()["id"].as_str().unwrap().to_owned()
}

/// Each entry of an `invitation.%` action but the invitations' creation, oldest first, as its
/// action, permission, actor, resource, target and details, `-` for no permission, actor or
/// target.
async fn entries(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads(
            "SELECT action || ' ' || coalesce(permission, '-') || ' ' \
             || coalesce(actor_user_id::text, '-') || ' ' || resource_id \
             || ' ' || coalesce(target_user_id::text, '-') || ' ' || details::text \
             FROM audit_entries WHERE action LIKE 'invitation.%' \
             AND action <> 'invitation.created' ORDER BY created_at, id",
        )
        .await
}

/// The permission of each refusal that the workspace's log records, oldest first (PLATFORM-010).
async fn refusals(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads(
            "SELECT details->>'permission' FROM audit_entries \
             WHERE action = 'access.denied' AND scope = 'workspace' ORDER BY created_at, id",
        )
        .await
}

/// Each event published since the last `DELETE FROM events`, oldest first, as its name, its
/// user, or `-` for a workspace's, the permission it requires, or `-` for a user's, and its
/// payload.
async fn events(api: &Api) -> Vec<String> {
    api.database()
        .owner_reads(
            "SELECT name || ' ' || coalesce(user_id::text, '-') || ' ' \
             || coalesce(permission, '-') || ' ' || payload::text \
             FROM events ORDER BY created_at, id",
        )
        .await
}

/// The payload of an event about the invitation `id` of `workspace`, to its members or to its
/// invitee, as `jsonb` writes it.
fn payload(workspace: &str, id: &str) -> String {
    format!("{{\"workspace_id\": \"{workspace}\", \"invitation_id\": \"{id}\"}}")
}

#[tokio::test]
async fn invitations_011_reading_an_invitation_answers_it_with_its_etag() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = acme(&api, &ada, &[(&grace, "member")]).await;
    let invited = invite(&api, &ada, &workspace, "carol@example.com", "admin").await;
    let id = invited.json()["id"].as_str().unwrap().to_owned();

    let response = read(&api, &grace, &workspace, &id).await;

    assert_eq!(response.status, 200);
    assert_eq!(response.json(), invited.json());
    assert_eq!(response.header("etag"), "\"1\"");
    assert_eq!(invited.header("etag"), "\"1\"");
}

#[tokio::test]
async fn invitations_011_an_expired_invitation_is_read_until_the_sweep_deletes_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let id = invited(&api, &ada, &workspace, "carol@example.com", "member").await;
    api.database()
        .owner_executes("UPDATE invitations SET expires_at = now() - interval '1 second'")
        .await;

    let expired = read(&api, &ada, &workspace, &id).await;
    api.run_periodic("invitations.expire").await;
    let swept = read(&api, &ada, &workspace, &id).await;

    assert_eq!(expired.status, 200);
    assert_eq!(expired.json()["id"], id.as_str());
    assert_eq!(swept.status, 404);
    assert_eq!(swept.json()["code"], "invitations.not_found");
}

#[tokio::test]
async fn invitations_011_an_id_that_names_no_invitation_of_the_workspace_answers_404() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;

    for id in ["0190f3c4-0000-7000-8000-000000000000", "not-an-id"] {
        let response = read(&api, &ada, &workspace, id).await;

        assert_eq!(response.status, 404, "{id}");
        assert_eq!(response.json()["code"], "invitations.not_found", "{id}");
    }
}

#[tokio::test]
async fn invitations_012_listing_gives_every_invitation_of_the_workspace_the_latest_first() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let workspace = acme(&api, &ada, &[(&grace, "member")]).await;
    let db = api.database();
    let globex = workspaces::arrange(db, "Globex", &[(&ada.user_id, "owner")]).await;
    invited(&api, &ada, &workspace, "carol@example.com", "member").await;
    let latest = invite(&api, &ada, &workspace, "dave@example.com", "admin").await;
    invited(&api, &ada, &globex, "erin@example.com", "member").await;
    db.owner_executes(
        "UPDATE invitations SET expires_at = now() - interval '1 second' \
         WHERE email = 'carol@example.com'",
    )
    .await;

    let listed = api
        .get(&format!("/v1/workspaces/{workspace}/invitations"))
        .bearer(&grace.access)
        .send()
        .await;

    assert_eq!(listed.status, 200);
    let body = listed.json();
    assert_eq!(
        lists::field(&body, "email"),
        ["dave@example.com", "carol@example.com"]
    );
    assert_eq!(body["items"][0], latest.json());
    assert_eq!(body["next_cursor"], Value::Null);
}

#[tokio::test]
async fn invitations_012_a_page_boundary_between_invitations_created_at_once_loses_none() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    for email in ["carol@example.com", "dave@example.com", "erin@example.com"] {
        invited(&api, &ada, &workspace, email, "member").await;
    }
    api.database()
        .owner_executes("UPDATE invitations SET created_at = '2026-11-01T12:00:00Z'")
        .await;

    let pages = lists::pages(
        &api,
        &ada,
        &format!("/v1/workspaces/{workspace}/invitations?limit=1"),
        "email",
    )
    .await;

    assert_eq!(pages.len(), 3);
    let mut seen: Vec<_> = pages.into_iter().flatten().collect();
    seen.sort();
    assert_eq!(
        seen,
        ["carol@example.com", "dave@example.com", "erin@example.com"]
    );
}

#[tokio::test]
async fn invitations_012_a_search_pages_through_its_matches() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    for email in [
        "alpha.one@example.com",
        "beta@example.com",
        "alpha.two@example.com",
    ] {
        invited(&api, &ada, &workspace, email, "member").await;
    }

    let pages = lists::pages(
        &api,
        &ada,
        &format!("/v1/workspaces/{workspace}/invitations?search=ALPHA&limit=1"),
        "email",
    )
    .await;

    assert_eq!(
        pages,
        [["alpha.two@example.com"], ["alpha.one@example.com"]]
    );
}

#[tokio::test]
async fn invitations_013_changing_the_role_stores_it_records_and_publishes_without_an_email() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let invited = invite(&api, &ada, &workspace, "carol@example.com", "admin").await;
    let before = invited.json();
    let id = before["id"].as_str().unwrap().to_owned();
    let db = api.database();
    db.owner_executes("DELETE FROM events").await;
    db.owner_executes("DELETE FROM jobs").await;

    let response = change_role(&api, &ada, &workspace, &id, "member").await;

    assert_eq!(response.status, 200);
    assert_eq!(response.header("etag"), "\"2\"");
    let mut expected = before.clone();
    expected["role"] = json!("member");
    assert_eq!(response.json(), expected);
    assert_eq!(
        entries(&api).await,
        [format!(
            "invitation.role_changed invitations.read {} {id} {} {{\"to\": \"member\", \"from\": \"admin\"}}",
            ada.user_id, carol.user_id
        )]
    );
    assert_eq!(
        events(&api).await,
        [
            format!(
                "invitation.updated - invitations.read {}",
                payload(&workspace, &id)
            ),
            format!(
                "invitation.revised {} - {}",
                carol.user_id,
                payload(&workspace, &id)
            ),
        ]
    );
    assert!(db.owner_reads("SELECT kind FROM jobs").await.is_empty());
}

#[tokio::test]
async fn invitations_013_an_expired_invitation_s_role_changes_and_it_stays_expired() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let id = invited(&api, &ada, &workspace, "carol@example.com", "admin").await;
    let db = api.database();
    db.owner_executes("UPDATE invitations SET expires_at = now() - interval '1 second'")
        .await;

    let response = change_role(&api, &ada, &workspace, &id, "member").await;

    assert_eq!(response.status, 200);
    assert_eq!(response.json()["role"], "member");
    let expired = db
        .owner_reads("SELECT (expires_at < now())::text FROM invitations")
        .await;
    assert_eq!(expired, ["true"]);
}

#[tokio::test]
async fn invitations_013_a_role_the_invitation_offers_already_changes_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let id = invited(&api, &ada, &workspace, "carol@example.com", "admin").await;
    api.database().owner_executes("DELETE FROM events").await;

    let response = change_role(&api, &ada, &workspace, &id, "admin").await;

    assert_eq!(response.status, 200);
    assert_eq!(response.header("etag"), "\"1\"");
    assert_eq!(response.json()["role"], "admin");
    assert!(entries(&api).await.is_empty());
    assert!(events(&api).await.is_empty());
}

#[tokio::test]
async fn invitations_013_a_role_that_isn_t_one_or_another_member_is_refused() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let id = invited(&api, &ada, &workspace, "carol@example.com", "admin").await;
    let send = |body: &'static str| {
        api.request(Method::PATCH, &path(&workspace, &id))
            .header("content-type", "application/merge-patch+json")
            .body(body)
            .bearer(&ada.access)
            .send()
    };

    let not_a_role = send(r#"{"role": "guest"}"#).await;
    let no_role = send("{}").await;
    let an_expiry =
        send(r#"{"role": "member", "expires_at": "2030-01-01T00:00:00.000000Z"}"#).await;

    assert_eq!(not_a_role.status, 400);
    assert_eq!(not_a_role.json()["code"], "request.invalid_fields");
    assert_eq!(not_a_role.json()["errors"][0]["field"], "/role");
    for refused in [&no_role, &an_expiry] {
        assert_eq!(refused.status, 400);
        assert_eq!(refused.json()["code"], "request.invalid_body");
    }
    let row = api
        .database()
        .owner_reads("SELECT role::text || ' ' || version FROM invitations")
        .await;
    assert_eq!(row, ["admin 1"]);
}

#[tokio::test]
async fn invitations_013_an_if_match_that_names_none_of_its_versions_answers_412() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let id = invited(&api, &ada, &workspace, "carol@example.com", "admin").await;
    let changed = change_role(&api, &ada, &workspace, &id, "member").await;
    assert_eq!(changed.status, 200);

    let stale = api
        .patch(&path(&workspace, &id), &json!({ "role": "admin" }))
        .header("if-match", "\"1\"")
        .bearer(&ada.access)
        .send()
        .await;
    let current = api
        .patch(&path(&workspace, &id), &json!({ "role": "admin" }))
        .header("if-match", changed.header("etag"))
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(stale.status, 412);
    assert_eq!(stale.json()["code"], "request.precondition_failed");
    assert_eq!(current.status, 200);
    assert_eq!(current.json()["role"], "admin");
    assert_eq!(current.header("etag"), "\"3\"");
}

#[tokio::test]
async fn invitations_014_an_admin_changes_sends_again_and_cancels_no_admin_s_invitation() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let workspace = acme(&api, &ada, &[(&bob, "admin")]).await;
    let id = invited(&api, &ada, &workspace, "carol@example.com", "admin").await;

    let changed = change_role(&api, &bob, &workspace, &id, "member").await;
    let resent = resend(&api, &bob, &workspace, &id).await;
    let cancelled = cancel(&api, &bob, &workspace, &id).await;

    for response in [&changed, &resent, &cancelled] {
        assert_eq!(response.status, 403);
        assert_eq!(response.json()["code"], "request.permission_denied");
    }
    let row = api
        .database()
        .owner_reads("SELECT role::text || ' ' || version FROM invitations")
        .await;
    assert_eq!(row, ["admin 1"]);
    assert!(entries(&api).await.is_empty());
    assert_eq!(
        refusals(&api).await,
        [
            "invitations.update",
            "invitations.create",
            "invitations.cancel"
        ]
    );
}

#[tokio::test]
async fn invitations_014_an_admin_refreshes_no_admin_s_invitation_by_inviting_its_address() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let workspace = acme(&api, &ada, &[(&bob, "admin")]).await;
    invited(&api, &ada, &workspace, "carol@example.com", "admin").await;
    let db = api.database();
    db.owner_executes("DELETE FROM jobs").await;

    let response = invite(&api, &bob, &workspace, "carol@example.com", "member").await;

    assert_eq!(response.status, 403);
    assert_eq!(response.json()["code"], "request.permission_denied");
    let row = db
        .owner_reads("SELECT role::text || ' ' || inviter_id || ' ' || version FROM invitations")
        .await;
    assert_eq!(row, [format!("admin {} 1", ada.user_id)]);
    assert!(entries(&api).await.is_empty());
    assert!(db.owner_reads("SELECT kind FROM jobs").await.is_empty());
    assert_eq!(refusals(&api).await, ["invitations.create"]);
}

#[tokio::test]
async fn invitations_014_a_change_assigns_only_a_role_below_the_caller_s() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let workspace = acme(&api, &ada, &[(&bob, "admin")]).await;
    let id = invited(&api, &ada, &workspace, "carol@example.com", "member").await;

    let refused = [
        change_role(&api, &bob, &workspace, &id, "admin").await,
        change_role(&api, &bob, &workspace, &id, "owner").await,
        change_role(&api, &ada, &workspace, &id, "owner").await,
    ];

    for response in &refused {
        assert_eq!(response.status, 403);
        assert_eq!(response.json()["code"], "request.permission_denied");
    }
    assert_eq!(refusals(&api).await, ["invitations.update"; 3]);
    let role = api
        .database()
        .owner_reads("SELECT role::text FROM invitations")
        .await;
    assert_eq!(role, ["member"]);
}

#[tokio::test]
async fn invitations_015_sending_again_renews_the_invitation_and_enqueues_its_email() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let workspace = acme(&api, &ada, &[(&bob, "admin")]).await;
    let id = invited(&api, &ada, &workspace, "carol@example.com", "member").await;
    let db = api.database();
    db.owner_executes("UPDATE invitations SET expires_at = now() - interval '1 second'")
        .await;
    db.owner_executes("DELETE FROM events").await;
    db.owner_executes("DELETE FROM jobs").await;

    let response = resend(&api, &bob, &workspace, &id).await;

    assert_eq!(response.status, 200);
    assert_eq!(response.header("etag"), "\"3\"");
    let body = response.json();
    assert_eq!(body["id"], id.as_str());
    assert_eq!(body["role"], "member");
    assert_eq!(body["inviter_id"], bob.user_id.as_str());
    let renewed = db
        .owner_reads(
            "SELECT (expires_at > now() + interval '6 days 23 hours')::text FROM invitations",
        )
        .await;
    assert_eq!(renewed, ["true"]);
    assert_eq!(
        entries(&api).await,
        [format!(
            "invitation.resent invitations.read {} {id} {} {{}}",
            bob.user_id, carol.user_id
        )]
    );
    assert_eq!(
        events(&api).await,
        [
            format!(
                "invitation.updated - invitations.read {}",
                payload(&workspace, &id)
            ),
            format!(
                "invitation.received {} - {}",
                carol.user_id,
                payload(&workspace, &id)
            ),
        ]
    );
    let jobs = db
        .owner_reads("SELECT kind || ' ' || workspace_id || ' ' || user_id FROM jobs")
        .await;
    assert_eq!(
        jobs,
        [format!("platform.send_email {workspace} {}", bob.user_id)]
    );
}

#[tokio::test]
async fn invitations_015_the_email_sent_again_names_the_member_who_sent_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let bob = users::register(&api, "bob@example.com").await;
    let workspace = acme(&api, &ada, &[(&bob, "admin")]).await;
    let renamed = api
        .patch("/v1/users/me", &json!({ "name": "Bob Jones" }))
        .bearer(&bob.access)
        .send()
        .await;
    assert_eq!(renamed.status, 200);
    let invitee = mailbox::address("invitee");
    let id = invited(&api, &ada, &workspace, &invitee, "member").await;
    api.database().owner_executes("DELETE FROM jobs").await;

    let response = resend(&api, &bob, &workspace, &id).await;
    api.run_jobs().await;

    assert_eq!(response.status, 200);
    let emails = mailbox::to(&invitee).await;
    assert_eq!(emails.len(), 1);
    assert_eq!(
        emails[0].subject,
        "Bob Jones invited you to Acme on AntWorker"
    );
}

#[tokio::test]
async fn invitations_009_sending_again_counts_in_the_member_s_bucket_of_inviting() {
    let options = Options::default()
        .set("INVITATION_LIMIT", 3)
        .set("INVITATION_WINDOW_SECONDS", 60);
    let api = Api::with(options).await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let id = invited(&api, &ada, &workspace, "carol@example.com", "member").await;

    // A resend that names no invitation answers 404, and counts all the same.
    let unknown = resend(
        &api,
        &ada,
        &workspace,
        "0190f3c4-0000-7000-8000-000000000000",
    )
    .await;
    let second = resend(&api, &ada, &workspace, &id).await;
    let third = resend(&api, &ada, &workspace, &id).await;
    let fourth = invite(&api, &ada, &workspace, "dave@example.com", "member").await;

    assert_eq!(unknown.status, 404);
    assert_eq!(second.status, 200);
    for refused in [&third, &fourth] {
        assert_eq!(refused.status, 429);
        assert_eq!(refused.json()["code"], "request.rate_limited");
    }
    assert_eq!(entries(&api).await.len(), 1);
}

#[tokio::test]
async fn invitations_016_cancelling_deletes_the_invitation_records_and_publishes() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let id = invited(&api, &ada, &workspace, "carol@example.com", "member").await;
    let db = api.database();
    db.owner_executes("UPDATE invitations SET expires_at = now() - interval '1 second'")
        .await;
    db.owner_executes("DELETE FROM events").await;

    let response = cancel(&api, &ada, &workspace, &id).await;
    let again = cancel(&api, &ada, &workspace, &id).await;

    assert_eq!(response.status, 204);
    assert!(
        db.owner_reads("SELECT id FROM invitations")
            .await
            .is_empty()
    );
    assert_eq!(
        entries(&api).await,
        [format!(
            "invitation.cancelled invitations.read {} {id} {} {{}}",
            ada.user_id, carol.user_id
        )]
    );
    assert_eq!(
        events(&api).await,
        [
            format!(
                "invitation.cancelled - invitations.read {}",
                payload(&workspace, &id)
            ),
            format!(
                "invitation.withdrawn {} - {}",
                carol.user_id,
                payload(&workspace, &id)
            ),
        ]
    );
    assert_eq!(again.status, 404);
    assert_eq!(again.json()["code"], "invitations.not_found");
}

#[tokio::test]
async fn invitations_016_cancelling_an_invitation_to_an_address_without_an_account_tells_no_one() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = acme(&api, &ada, &[]).await;
    let id = invited(&api, &ada, &workspace, "nobody@example.com", "member").await;
    api.database().owner_executes("DELETE FROM events").await;

    let response = cancel(&api, &ada, &workspace, &id).await;

    assert_eq!(response.status, 204);
    assert_eq!(
        entries(&api).await,
        [format!(
            "invitation.cancelled invitations.read {} {id} - {{}}",
            ada.user_id
        )]
    );
    assert_eq!(
        events(&api).await,
        [format!(
            "invitation.cancelled - invitations.read {}",
            payload(&workspace, &id)
        )]
    );
}

#[tokio::test]
async fn sec_19_the_invitation_events_reach_the_members_and_no_replay_from_before_joining() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let carol = users::register(&api, "carol@example.com").await;
    let dan = users::register(&api, "dan@example.com").await;
    // Every role holds invitations.read, which each of these events requires: no member lacks it.
    let workspace = acme(&api, &ada, &[(&grace, "member")]).await;
    let mut owner = streams::connect(&api, &ada.access, None).await;
    owner.opened().await;
    let mut member = streams::connect(&api, &grace.access, None).await;
    member.opened().await;
    // Dan's last event before he joins is his own invitation's, which he reads from his stream.
    let mut first = streams::connect(&api, &dan.access, None).await;
    first.opened().await;
    let welcome = invited(&api, &ada, &workspace, "dan@example.com", "member").await;
    let received = first.event().await;
    assert_eq!(received.event.as_deref(), Some("invitation.received"));
    drop(first);

    let erin = invited(&api, &ada, &workspace, "erin@example.com", "admin").await;
    invite(&api, &ada, &workspace, "erin@example.com", "member").await;
    change_role(&api, &ada, &workspace, &erin, "admin").await;
    resend(&api, &ada, &workspace, &erin).await;
    cancel(&api, &ada, &workspace, &erin).await;
    let to_carol = invited(&api, &ada, &workspace, "carol@example.com", "member").await;
    super::decline(&api, &carol, &to_carol).await;
    super::accept(&api, &dan, &welcome).await;
    let mut later = streams::connect(&api, &dan.access, received.id.as_deref()).await;
    later.opened().await;
    let replayed = [later.event().await, later.event().await];

    let expected = [
        ("invitation.created", &welcome),
        ("invitation.created", &erin),
        ("invitation.updated", &erin),
        ("invitation.updated", &erin),
        ("invitation.updated", &erin),
        ("invitation.cancelled", &erin),
        ("invitation.created", &to_carol),
        ("invitation.declined", &to_carol),
    ];
    for stream in [&mut owner, &mut member] {
        for (name, id) in expected {
            let frame = stream.event().await;
            assert_eq!(frame.event.as_deref(), Some(name));
            assert_eq!(frame.payload()["invitation_id"], id.as_str());
        }
        let joined = stream.event().await;
        assert_eq!(joined.event.as_deref(), Some("member.joined"));
    }
    // Every invitation event came before Dan's membership, so his replay holds only his joining.
    let names: Vec<_> = replayed.iter().map(|frame| frame.event.clone()).collect();
    assert_eq!(
        names,
        [
            Some("member.joined".to_owned()),
            Some("membership.created".to_owned())
        ]
    );
}
