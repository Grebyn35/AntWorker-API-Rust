//! Exporting one's data (the accounts spec's "Exporting").

use std::io::{Cursor, Read};

use serde_json::{Value, json};

use crate::support::{
    Api, Options,
    api::Response,
    audit::{self, Client},
    lists, mailbox, streams,
    users::{self, SignedIn},
    workspaces,
};
// part: invitations
use crate::support::invitations;
// end part: invitations

/// Requests an export of `user`'s data.
async fn request(api: &Api, user: &SignedIn) -> Response {
    api.request(http::Method::POST, "/v1/users/me/exports")
        .bearer(&user.access)
        .send()
        .await
}

/// Requests an export of `user`'s data, builds its archive, and returns its ID.
pub async fn built(api: &Api, user: &SignedIn) -> String {
    let response = request(api, user).await;
    assert_eq!(response.status, 202);
    api.run_jobs().await;
    response.json()["id"].as_str().unwrap().to_owned()
}

/// Downloads the export `id` of `user`'s, with the headers `headers`.
async fn download(api: &Api, user: &SignedIn, id: &str, headers: &[(&str, &str)]) -> Response {
    let mut call = api
        .get(&format!("/v1/users/me/exports/{id}/content"))
        .bearer(&user.access);
    for (name, value) in headers {
        call = call.header(name, value);
    }
    call.send().await
}

/// Every entry of `archive`, by its path, with its bytes.
pub fn entries(archive: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive.to_vec())).unwrap();
    (0..zip.len())
        .map(|index| {
            let mut file = zip.by_index(index).unwrap();
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes).unwrap();
            (file.name().unwrap().into_owned(), bytes)
        })
        .collect()
}

/// The records of the document at `path` in `archive`.
pub fn document(archive: &[u8], path: &str) -> Vec<Value> {
    let found = entries(archive).into_iter().find(|(name, _)| name == path);
    assert!(found.is_some(), "no {path}");
    serde_json::from_slice(&found.unwrap().1).unwrap()
}

/// The log, actor, resource and resource ID of each entry of `action`, as
/// `user <user> <actor> <resource> <id>`, the actor `-` when there is none.
async fn entries_of(api: &Api, action: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT scope::text || ' ' || coalesce(user_id::text, '-') || ' ' || \
             coalesce(actor_user_id::text, '-') || ' ' || resource_type || ' ' || resource_id \
             FROM audit_entries WHERE action = '{action}' ORDER BY created_at"
        ))
        .await
}

/// The user of each event named `name`, in the order they were published.
async fn event_users(api: &Api, name: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT user_id::text FROM events WHERE name = '{name}' ORDER BY created_at"
        ))
        .await
}

/// The one value the owner's query `sql` reads.
async fn one(api: &Api, sql: &str) -> String {
    api.database().owner_reads(sql).await.remove(0)
}

#[tokio::test]
async fn accounts_020_a_request_answers_202_with_the_pending_export_records_it_and_enqueues_its_job()
 {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = request(&api, &ada).await;

    assert_eq!(response.status, 202);
    let export = response.json();
    let id = export["id"].as_str().unwrap();
    assert_eq!(
        response.header("location"),
        format!("/v1/users/me/exports/{id}")
    );
    assert_eq!(export["state"], "pending");
    assert_eq!(export["ready_at"], json!(null));
    assert_eq!(export["size_bytes"], json!(null));
    let lasts = one(
        &api,
        &format!(
            "SELECT round(extract(epoch FROM expires_at - created_at))::text \
             FROM exports WHERE id = '{id}'"
        ),
    )
    .await;
    assert_eq!(lasts, "604800");
    let logged = one(
        &api,
        "SELECT action || ' ' || resource_type || ' ' || resource_id || ' ' || user_id \
         FROM audit_entries WHERE action = 'export.requested'",
    )
    .await;
    assert_eq!(
        logged,
        format!("export.requested export {id} {}", ada.user_id)
    );
    let job = one(
        &api,
        "SELECT kind || ' ' || user_id || ' ' || (payload->>'export_id') FROM jobs \
         WHERE kind = 'accounts.export'",
    )
    .await;
    assert_eq!(job, format!("accounts.export {} {id}", ada.user_id));
}

#[tokio::test]
async fn accounts_021_a_second_request_within_24_hours_answers_429_and_creates_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    assert_eq!(request(&api, &ada).await.status, 202);

    let again = request(&api, &ada).await;

    assert_eq!(again.status, 429);
    assert_eq!(again.json()["code"], "request.rate_limited");
    assert!(again.header("retry-after").parse::<u64>().unwrap() > 86_000);
    assert_eq!(again.header("ratelimit-policy"), "\"export\";q=1;w=86400");
    let exports = one(&api, "SELECT count(*)::text FROM exports").await;
    assert_eq!(exports, "1");
    // Each user has a bucket of their own.
    assert_eq!(request(&api, &grace).await.status, 202);
}

#[tokio::test]
async fn accounts_022_the_caller_lists_and_reads_their_own_exports_and_no_other_user_s() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let first = built(&api, &ada).await;
    // A second export, requested a day later, as the bucket allows.
    api.database()
        .owner_executes("UPDATE exports SET created_at = created_at - interval '1 day'")
        .await;
    api.database()
        .owner_executes("DELETE FROM rate_limit_buckets")
        .await;
    let second = request(&api, &ada).await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let theirs = request(&api, &grace).await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let list = api
        .get("/v1/users/me/exports?limit=1")
        .bearer(&ada.access)
        .send()
        .await;
    let next = list.json()["next_cursor"].as_str().unwrap().to_owned();
    let rest = api
        .get(&format!("/v1/users/me/exports?cursor={next}"))
        .bearer(&ada.access)
        .send()
        .await;
    let read = api
        .get(&format!("/v1/users/me/exports/{first}"))
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(list.status, 200);
    assert_eq!(list.json()["items"][0]["id"], second.as_str());
    assert_eq!(list.json()["items"][0]["state"], "pending");
    assert_eq!(rest.json()["items"].as_array().unwrap().len(), 1);
    assert_eq!(rest.json()["items"][0]["id"], first.as_str());
    assert_eq!(rest.json()["next_cursor"], json!(null));
    assert_eq!(read.status, 200);
    let export = read.json();
    assert_eq!(export["state"], "ready");
    assert!(export["size_bytes"].as_u64().unwrap() > 0);
    assert!(export["ready_at"].is_string());
    for missing in [theirs.as_str(), "not-a-uuid"] {
        let response = api
            .get(&format!("/v1/users/me/exports/{missing}"))
            .bearer(&ada.access)
            .send()
            .await;
        assert_eq!(response.status, 404, "{missing}");
        assert_eq!(response.json()["code"], "accounts.export_not_found");
        let content = download(&api, &ada, missing, &[]).await;
        assert_eq!(content.status, 404, "{missing}");
    }
    // An expired export is gone, though the job that deletes it hasn't run.
    api.database()
        .owner_executes(&format!(
            "UPDATE exports SET expires_at = now() - interval '1 second' WHERE id = '{first}'"
        ))
        .await;
    let expired = api
        .get(&format!("/v1/users/me/exports/{first}"))
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(expired.status, 404);
    let listed = api
        .get("/v1/users/me/exports")
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(listed.json()["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn accounts_023_the_job_builds_the_archive_makes_the_export_ready_and_publishes_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;
    let id = request(&api, &ada).await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    api.run_jobs().await;

    let event = stream.event().await;
    assert_eq!(event.event.as_deref(), Some("export.completed"));
    assert_eq!(event.payload(), &json!({ "export_id": id }));
    let state = one(
        &api,
        &format!(
            "SELECT state::text || ' ' || (ready_at IS NOT NULL) || ' ' || \
             round(extract(epoch FROM expires_at - ready_at))::text FROM exports \
             WHERE id = '{id}'"
        ),
    )
    .await;
    assert_eq!(state, "ready true 604800");
    // Recorded in Ada's own log with no actor, as the job's, and published to her alone.
    assert_eq!(
        entries_of(&api, "export.completed").await,
        [format!("user {} - export {id}", ada.user_id)]
    );
    assert_eq!(
        event_users(&api, "export.completed").await,
        std::slice::from_ref(&ada.user_id)
    );
    let documents = one(
        &api,
        &format!("SELECT count(*)::text FROM export_entries WHERE export_id = '{id}'"),
    )
    .await;
    // A second run of the job, as a redelivery would make it, changes nothing.
    api.database()
        .owner_executes(&format!(
            "INSERT INTO jobs (kind, payload, user_id) \
             VALUES ('accounts.export', '{{\"export_id\": \"{id}\"}}', '{}')",
            ada.user_id
        ))
        .await;
    api.run_jobs().await;
    let again = one(
        &api,
        &format!("SELECT count(*)::text FROM export_entries WHERE export_id = '{id}'"),
    )
    .await;
    assert_eq!(again, documents);
}

#[tokio::test]
async fn accounts_023_a_last_attempt_that_fails_makes_the_export_failed_and_publishes_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut stream = streams::opened(&api, &ada).await;
    let id = request(&api, &ada).await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The job's tenth attempt, the last, whose contract of `auth` can't read the sessions.
    api.database()
        .owner_executes("UPDATE jobs SET attempts = 9 WHERE kind = 'accounts.export'")
        .await;
    api.database()
        .owner_executes("REVOKE SELECT ON sessions FROM app")
        .await;

    api.run_jobs().await;
    api.database()
        .owner_executes("GRANT SELECT ON sessions TO app")
        .await;

    let event = stream.event().await;
    assert_eq!(event.event.as_deref(), Some("export.failed"));
    assert_eq!(event.payload(), &json!({ "export_id": id }));
    let state = one(
        &api,
        &format!(
            "SELECT state::text || ' ' || (SELECT count(*) FROM export_chunks) \
             FROM exports WHERE id = '{id}'"
        ),
    )
    .await;
    assert_eq!(state, "failed 0");
    assert_eq!(
        entries_of(&api, "export.failed").await,
        [format!("user {} - export {id}", ada.user_id)]
    );
    assert_eq!(
        event_users(&api, "export.failed").await,
        std::slice::from_ref(&ada.user_id)
    );
    let read = api
        .get(&format!("/v1/users/me/exports/{id}"))
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(read.json()["state"], "failed");
    let content = download(&api, &ada, &id, &[]).await;
    assert_eq!(content.status, 409);
    assert_eq!(content.json()["code"], "accounts.export_not_ready");
}

#[tokio::test]
async fn accounts_023_a_run_of_a_job_whose_export_is_gone_changes_nothing() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let id = request(&api, &ada).await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The export went, as its expiry or its user's deletion deletes it, before its job ran.
    api.database()
        .owner_executes(&format!("DELETE FROM exports WHERE id = '{id}'"))
        .await;

    api.run_jobs().await;

    let left = one(
        &api,
        "SELECT (SELECT count(*) FROM exports) || ' ' || (SELECT count(*) FROM export_entries) \
         || ' ' || (SELECT count(*) FROM jobs) || ' ' || \
         (SELECT count(*) FROM events WHERE name LIKE 'export.%')",
    )
    .await;
    assert_eq!(left, "0 0 0 0");
}

/// Ada, with a workspace of her own and one of Grace's, an entry of her own log that Grace acted
/// in, and an entry of each workspace's log, among what each workspace holds; returns Ada, Grace
/// and both workspaces' IDs.
async fn arranged(api: &Api) -> (SignedIn, SignedIn, String, String) {
    let ada = users::register(api, "ada@example.com").await;
    let grace = users::register(api, "grace@example.com").await;
    let db = api.database();
    let own = workspaces::arrange(db, "Ada's notes", &[(&ada.user_id, "owner")]).await;
    let shared = workspaces::arrange(
        db,
        "Grace's team",
        &[(&grace.user_id, "owner"), (&ada.user_id, "member")],
    )
    .await;
    // An entry of Ada's own log that Grace acted in, whose client is Grace's.
    let client = Client {
        user: &grace.user_id,
        ip: "192.0.2.7",
        agent: "Grace's browser",
    };
    audit::in_own_log(
        db,
        &ada.user_id,
        &grace.user_id,
        "member.removed",
        Some(client),
    )
    .await;
    for workspace in [&own, &shared] {
        audit::in_workspace_log(db, workspace, &ada.user_id, "workspace.renamed").await;
    }
    // part: invitations
    // An invitation of each workspace.
    let mine = invitations::invite(api, &ada, &own, "lin@example.com", "member").await;
    assert_eq!(mine.status, 201);
    let theirs = invitations::invite(api, &grace, &shared, "max@example.com", "member").await;
    assert_eq!(theirs.status, 201);
    // end part: invitations
    (ada, grace, own, shared)
}

/// The workspaces and the memberships that `archive`, Ada's, holds: her own workspace, and both
/// memberships, with their roles.
fn assert_workspaces(archive: &[u8], own: &str, shared: &str) {
    let workspaces = document(archive, "workspaces/workspaces.json");
    assert_eq!(
        workspaces,
        [json!({
            "id": own,
            "name": "Ada's notes",
            "created_at": workspaces[0]["created_at"],
            "updated_at": workspaces[0]["updated_at"],
        })]
    );
    let mut roles: Vec<(String, String)> = document(archive, "access/memberships.json")
        .iter()
        .map(|membership| {
            (
                membership["workspace_id"].as_str().unwrap().to_owned(),
                membership["role"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    roles.sort();
    let mut expected = vec![
        (own.to_owned(), "owner".to_owned()),
        (shared.to_owned(), "member".to_owned()),
    ];
    expected.sort();
    assert_eq!(roles, expected);
}

/// The logs that `archive`, Ada's, holds: her own, with no client of an entry Grace acted in, and
/// her own workspace's alone.
fn assert_logs(archive: &[u8], grace: &SignedIn, own: &str) {
    let log = document(archive, "audit/log.json");
    let removed = log
        .iter()
        .find(|entry| entry["action"] == "member.removed")
        .unwrap();
    assert_eq!(removed["actor_user_id"], grace.user_id.as_str());
    assert_eq!(removed["client_ip"], json!(null));
    assert_eq!(removed["user_agent"], json!(null));
    let requested = log
        .iter()
        .find(|entry| entry["action"] == "export.requested")
        .unwrap();
    assert!(requested["client_ip"].is_string(), "{requested}");
    let workspace_logs = document(archive, "audit/workspace_logs.json");
    assert!(!workspace_logs.is_empty());
    assert!(
        workspace_logs
            .iter()
            .all(|entry| entry["workspace_id"] == own),
        "{workspace_logs:?}"
    );
}

/// Whether any entry of `archive` holds `text`.
fn holds(archive: &[u8], text: &str) -> bool {
    entries(archive)
        .iter()
        .any(|(_, bytes)| String::from_utf8_lossy(bytes).contains(text))
}

#[tokio::test]
async fn accounts_024_the_archive_holds_the_user_s_data_and_only_the_content_of_their_own_workspaces()
 {
    let api = Api::new().await;
    let (ada, grace, own, shared) = arranged(&api).await;
    let id = built(&api, &ada).await;

    let archive = download(&api, &ada, &id, &[]).await.body;

    let paths: Vec<String> = entries(&archive)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    for path in [
        "workspaces/workspaces.json",
        "access/memberships.json",
        "auth/identities.json",
        "auth/sessions.json",
        "auth/device_tokens.json",
        "audit/log.json",
        "audit/workspace_logs.json",
        "accounts/profile.json",
    ] {
        assert!(
            paths.iter().any(|name| name == path),
            "no {path} in {paths:?}"
        );
    }
    let profile = document(&archive, "accounts/profile.json");
    assert_eq!(profile.len(), 1);
    assert_eq!(profile[0]["id"], ada.user_id.as_str());
    assert_eq!(profile[0]["email"], "ada@example.com");
    assert_eq!(profile[0]["name"], "Ada Lovelace");
    assert_workspaces(&archive, &own, &shared);
    let identities = document(&archive, "auth/identities.json");
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0]["method"], "password");
    assert_eq!(document(&archive, "auth/sessions.json").len(), 1);
    assert_eq!(document(&archive, "auth/device_tokens.json").len(), 1);
    assert_logs(&archive, &grace, &own);
    // part: invitations
    let invited = document(&archive, "invitations/invitations.json");
    assert_eq!(invited.len(), 1, "{invited:?}");
    assert_eq!(invited[0]["workspace_id"], own.as_str());
    assert_eq!(invited[0]["email"], "lin@example.com");
    assert!(!holds(&archive, "max@example.com"));
    // end part: invitations
    // No hash of a password or a token, of either user's, and nothing of Grace's but her ID.
    for absent in ["$argon2", "Grace's team", "grace@example.com", "192.0.2.7"] {
        assert!(!holds(&archive, absent), "{absent}");
    }
    for (table, column) in [
        ("sessions", "access_token_hash"),
        ("sessions", "refresh_token_hash"),
        ("device_tokens", "token_hash"),
    ] {
        let query = format!("SELECT encode({column}, 'hex') FROM {table}");
        for hash in api.database().owner_reads(&query).await {
            assert!(!holds(&archive, &hash), "{table}.{column}");
        }
    }
}

#[tokio::test]
async fn accounts_025_with_email_on_a_ready_export_tells_the_address_until_when() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let id = built(&api, &ada).await;
    api.run_jobs().await;

    let emails = mailbox::to(&address).await;

    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].subject, "Your AntWorker data export is ready");
    let expires = one(
        &api,
        &format!(
            "SELECT to_char(expires_at AT TIME ZONE 'UTC', 'YYYY-MM-DD HH24:MI') \
             FROM exports WHERE id = '{id}'"
        ),
    )
    .await;
    assert!(
        emails[0].text.contains(&format!("until {expires} UTC")),
        "{}",
        emails[0].text
    );
    assert!(!emails[0].text.contains("http"), "{}", emails[0].text);
}

#[tokio::test]
async fn accounts_025_with_email_off_nothing_is_sent() {
    let api = Api::with(Options::default().set("EMAIL", "off")).await;
    let ada = users::register(&api, "ada@example.com").await;

    built(&api, &ada).await;

    let jobs = one(&api, "SELECT count(*)::text FROM jobs").await;
    assert_eq!(jobs, "0");
}

#[tokio::test]
async fn accounts_026_a_download_serves_the_archive_by_ranges_the_same_bytes_each_time_and_records_it()
 {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let id = built(&api, &ada).await;

    let whole = download(&api, &ada, &id, &[]).await;
    // A change since the export was built changes none of its bytes.
    let renamed = api
        .patch("/v1/users/me", &json!({ "name": "Countess of Lovelace" }))
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(renamed.status, 200);
    let again = download(&api, &ada, &id, &[]).await;
    let range = download(&api, &ada, &id, &[("range", "bytes=10-99")]).await;
    let tail = download(&api, &ada, &id, &[("range", "bytes=-22")]).await;
    let cached = download(&api, &ada, &id, &[("if-none-match", &format!("\"{id}\""))]).await;
    let head = api
        .request(
            http::Method::HEAD,
            &format!("/v1/users/me/exports/{id}/content"),
        )
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(whole.status, 200);
    assert_eq!(whole.header("content-type"), "application/zip");
    let ready = one(
        &api,
        &format!(
            "SELECT to_char(ready_at AT TIME ZONE 'UTC', 'YYYY-MM-DD') FROM exports \
             WHERE id = '{id}'"
        ),
    )
    .await;
    assert_eq!(
        whole.header("content-disposition"),
        format!(
            "attachment; filename=\"AntWorker-export-{ready}.zip\"; \
             filename*=UTF-8''AntWorker-export-{ready}.zip"
        )
    );
    assert_eq!(whole.header("etag"), format!("\"{id}\""));
    assert_eq!(whole.header("accept-ranges"), "bytes");
    let size = api
        .get(&format!("/v1/users/me/exports/{id}"))
        .bearer(&ada.access)
        .send()
        .await
        .json()["size_bytes"]
        .as_u64()
        .unwrap();
    assert_eq!(whole.body.len() as u64, size);
    assert_eq!(again.body, whole.body);
    assert_eq!(
        document(&again.body, "accounts/profile.json")[0]["name"],
        "Ada Lovelace"
    );
    assert_eq!(range.status, 206);
    assert_eq!(range.header("content-range"), format!("bytes 10-99/{size}"));
    assert_eq!(range.body, whole.body[10..100]);
    assert_eq!(tail.body, whole.body[whole.body.len() - 22..]);
    assert_eq!(cached.status, 304);
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    // Each request that served bytes records its download in Ada's own log, as her act, and the
    // 304 and the HEAD don't.
    let entry = format!("user {0} {0} export {id}", ada.user_id);
    assert_eq!(entries_of(&api, "export.downloaded").await, vec![entry; 4]);
}

#[tokio::test]
async fn accounts_026_a_pending_export_answers_409() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let id = request(&api, &ada).await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let content = download(&api, &ada, &id, &[]).await;

    assert_eq!(content.status, 409);
    assert_eq!(content.json()["code"], "accounts.export_not_ready");
    let downloads = one(
        &api,
        "SELECT count(*)::text FROM audit_entries WHERE action = 'export.downloaded'",
    )
    .await;
    assert_eq!(downloads, "0");
}

#[tokio::test]
async fn accounts_027_the_expiry_deletes_expired_exports_and_fails_those_pending_for_a_day() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let expired = built(&api, &ada).await;
    let stale = request(&api, &grace).await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let db = api.database();
    db.owner_executes("DELETE FROM jobs").await;
    db.owner_executes(&format!(
        "UPDATE exports SET expires_at = now() - interval '1 second' WHERE id = '{expired}'"
    ))
    .await;
    db.owner_executes(&format!(
        "UPDATE exports SET created_at = now() - interval '24 hours 1 second' \
         WHERE id = '{stale}'"
    ))
    .await;
    let mut stream = streams::opened(&api, &grace).await;

    api.run_periodic("accounts.expire_exports").await;
    api.run_periodic("accounts.expire_exports").await;

    let left = db
        .owner_reads("SELECT id || ' ' || state::text FROM exports")
        .await;
    assert_eq!(left, [format!("{stale} failed")]);
    let chunks = one(&api, "SELECT count(*)::text FROM export_chunks").await;
    assert_eq!(chunks, "0");
    let event = stream.event().await;
    assert_eq!(event.event.as_deref(), Some("export.failed"));
    assert_eq!(event.payload(), &json!({ "export_id": stale }));
}

#[tokio::test]
async fn accounts_022_paging_returns_each_export_once_across_a_shared_request_time() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let mut requested = Vec::new();
    for _ in 0..3 {
        requested.push(
            request(&api, &ada).await.json()["id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
        // The bucket allows a request a day; each day's is another export.
        db.owner_executes("DELETE FROM rate_limit_buckets").await;
    }
    db.owner_executes("UPDATE exports SET created_at = '2026-01-01T00:00:00Z'")
        .await;

    let pages = lists::pages(&api, &ada, "/v1/users/me/exports?limit=1", "id").await;

    assert_eq!(pages.len(), 3);
    let mut listed: Vec<String> = pages.into_iter().flatten().collect();
    listed.sort();
    requested.sort();
    assert_eq!(listed, requested);
}

#[tokio::test]
async fn platform_081_a_download_serves_its_bytes_with_the_headers_that_keep_a_browser_from_running_them()
 {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let id = built(&api, &ada).await;

    let whole = download(&api, &ada, &id, &[]).await;

    assert_eq!(whole.status, 200);
    assert_eq!(whole.header("content-length"), whole.body.len().to_string());
    assert_eq!(
        whole.header("content-security-policy"),
        "default-src 'none'; sandbox"
    );
    assert_eq!(whole.header("x-content-type-options"), "nosniff");
    assert_eq!(whole.header("cache-control"), "no-store");
    assert_eq!(whole.header("accept-ranges"), "bytes");
    assert_eq!(whole.header("etag"), format!("\"{id}\""));
}

#[tokio::test]
async fn platform_081_one_range_answers_206_and_a_range_it_can_t_serve_every_byte() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let id = built(&api, &ada).await;
    let whole = download(&api, &ada, &id, &[]).await.body;
    let size = whole.len();
    let tag = format!("\"{id}\"");

    let range = download(&api, &ada, &id, &[("range", "bytes=5-")]).await;
    let same = download(
        &api,
        &ada,
        &id,
        &[("range", "bytes=0-9"), ("if-range", &tag)],
    )
    .await;
    let other = download(
        &api,
        &ada,
        &id,
        &[("range", "bytes=0-9"), ("if-range", "\"other\"")],
    )
    .await;
    let several = download(&api, &ada, &id, &[("range", "bytes=0-1,5-6")]).await;
    let past = download(&api, &ada, &id, &[("range", &format!("bytes={size}-"))]).await;

    assert_eq!(range.status, 206);
    assert_eq!(
        range.header("content-range"),
        format!("bytes 5-{}/{size}", size - 1)
    );
    assert_eq!(range.header("content-length"), (size - 5).to_string());
    assert_eq!(range.body, whole[5..]);
    assert_eq!(same.status, 206);
    assert_eq!(same.body, whole[..10]);
    for every in [&other, &several, &past] {
        assert_eq!(every.status, 200);
        assert_eq!(every.body, whole);
    }
}

#[tokio::test]
async fn platform_081_if_none_match_answers_304_and_head_answers_without_the_bytes() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let id = built(&api, &ada).await;
    let size = download(&api, &ada, &id, &[]).await.body.len();

    let weak = download(
        &api,
        &ada,
        &id,
        &[("if-none-match", &format!("W/\"{id}\""))],
    )
    .await;
    let any = download(&api, &ada, &id, &[("if-none-match", "*")]).await;
    let head = api
        .request(
            http::Method::HEAD,
            &format!("/v1/users/me/exports/{id}/content"),
        )
        .bearer(&ada.access)
        .header("range", "bytes=0-9")
        .send()
        .await;

    for not_modified in [&weak, &any] {
        assert_eq!(not_modified.status, 304);
        assert!(not_modified.body.is_empty());
        assert_eq!(not_modified.header("etag"), format!("\"{id}\""));
    }
    // A HEAD ignores Range, and answers as a GET without one does, without the bytes.
    assert_eq!(head.status, 200);
    assert!(head.body.is_empty());
    assert_eq!(head.header("content-length"), size.to_string());
}
