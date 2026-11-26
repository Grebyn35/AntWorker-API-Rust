//! Exporting one's data (the accounts spec's "Exporting").

use std::io::{Cursor, Read};

use axum::{body::Body, extract::ConnectInfo};
use platform::telemetry::testing;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::support::{
    Api,
    api::Response,
    api::one,
    audit::{self, Client},
    execute_on, lists, mailbox, providers, query_waits_on_a_lock, streams,
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
pub async fn download(api: &Api, user: &SignedIn, id: &str, headers: &[(&str, &str)]) -> Response {
    let mut call = api
        .get(&format!("/v1/users/me/exports/{id}/content"))
        .bearer(&user.access);
    for (name, value) in headers {
        call = call.header(name, value);
    }
    call.send().await
}

/// Stores the document of the export `id` that holds the most deflated bytes again as three
/// chunks, the thirds of the one chunk that held it, as a document of more bytes than a chunk
/// holds is stored (PLATFORM-084), and returns its path, the length of a third and the offset of
/// the third chunk among its deflated bytes.
async fn in_thirds(api: &Api, id: &str) -> (String, usize, usize) {
    let longest = one(
        api,
        &format!(
            "SELECT position || ' ' || stored_bytes || ' ' || path FROM export_entries \
             WHERE export_id = '{id}' ORDER BY stored_bytes DESC, position LIMIT 1"
        ),
    )
    .await;
    let mut fields = longest.splitn(3, ' ');
    let (position, length, path) = (
        fields.next().unwrap(),
        fields.next().unwrap().parse::<usize>().unwrap(),
        fields.next().unwrap().to_owned(),
    );
    let document = format!("export_id = '{id}' AND position = {position}");
    let chunks = one(
        api,
        &format!("SELECT count(*)::text FROM export_chunks WHERE {document}"),
    )
    .await;
    assert_eq!(chunks, "1", "{path} is one chunk");
    let third = length / 3;
    assert!(third >= 2, "{path} holds {length} deflated bytes");
    api.database()
        .owner_executes(&format!(
            "INSERT INTO export_chunks (export_id, position, offset_bytes, bytes) \
             SELECT export_id, position, {third}, substring(bytes FROM {third} + 1 FOR {third}) \
             FROM export_chunks WHERE {document} \
             UNION ALL \
             SELECT export_id, position, 2 * {third}, substring(bytes FROM 2 * {third} + 1) \
             FROM export_chunks WHERE {document}; \
             UPDATE export_chunks SET bytes = substring(bytes FROM 1 FOR {third}) \
             WHERE {document} AND offset_bytes = 0"
        ))
        .await;
    (path, third, 2 * third)
}

/// Where the deflated bytes of the entry at `path` start in `archive`: after its local header,
/// whose name and extra field have the lengths that the header's bytes 26 to 29 give (APPNOTE
/// 4.3.7).
fn entry_data(archive: &[u8], path: &str) -> usize {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive.to_vec())).unwrap();
    let header = usize::try_from(zip.by_name(path).unwrap().header_start()).unwrap();
    let length = |at: usize| {
        usize::from(u16::from_le_bytes([
            archive[header + at],
            archive[header + at + 1],
        ]))
    };
    header + 30 + length(26) + length(28)
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

/// The moment that `value`, a field of a response, gives (API-11).
fn moment(value: &Value) -> time::OffsetDateTime {
    time::OffsetDateTime::parse(
        value.as_str().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap()
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
        "SELECT action || ' ' || resource_type || ' ' || resource_id || ' ' || user_id || ' ' \
         || actor_user_id FROM audit_entries WHERE action = 'export.requested'",
    )
    .await;
    assert_eq!(
        logged,
        format!("export.requested export {id} {0} {0}", ada.user_id)
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
async fn accounts_020_a_session_that_proved_its_user_more_than_5_minutes_ago_is_refused_and_counts_nothing()
 {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    users::authenticated_ago(&api, &ada, 6).await;

    let refused = request(&api, &ada).await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "request.reauthentication_required");
    assert_eq!(
        refused.json()["detail"],
        "Prove who you are again, then send the request again."
    );
    assert_eq!(one(&api, "SELECT count(*)::text FROM exports").await, "0");
    assert_eq!(
        one(
            &api,
            "SELECT count(*)::text FROM audit_entries WHERE action = 'export.requested'"
        )
        .await,
        "0"
    );
    // The refusal took nothing of the day's one export: once the session proves its user again,
    // the request is let through (ACCOUNTS-021).
    let reauthenticated = users::reauthenticate(&api, &ada, users::PASSWORD).await;
    assert_eq!(reauthenticated.status, 200);
    // With the tokens that the proof gave the session (AUTH-095).
    let ada = users::SignedIn::from(&reauthenticated);
    assert_eq!(request(&api, &ada).await.status, 202);
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
    let archive = download(&api, &ada, &first, &[]).await.body;

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
    // Its archive's size in bytes.
    assert_eq!(export["size_bytes"], archive.len());
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
        assert_eq!(content.json()["code"], "accounts.export_not_found");
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
    let content = download(&api, &ada, &first, &[]).await;
    assert_eq!(content.status, 404);
    assert_eq!(content.json()["code"], "accounts.export_not_found");
    let listed = api
        .get("/v1/users/me/exports")
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(listed.json()["items"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn accounts_022_an_export_expires_7_days_after_it_became_ready_or_after_its_request() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let ready_id = built(&api, &ada).await;
    // A second export, requested a day later, as the bucket allows, which stays pending.
    api.database()
        .owner_executes("UPDATE exports SET created_at = created_at - interval '1 day'")
        .await;
    api.database()
        .owner_executes("DELETE FROM rate_limit_buckets")
        .await;
    let pending_id = request(&api, &ada).await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let path = |id: &str| format!("/v1/users/me/exports/{id}");

    let ready = api
        .get(&path(&ready_id))
        .bearer(&ada.access)
        .send()
        .await
        .json();
    let pending = api
        .get(&path(&pending_id))
        .bearer(&ada.access)
        .send()
        .await
        .json();

    assert_eq!(ready["state"], "ready");
    assert_eq!(
        moment(&ready["expires_at"]) - moment(&ready["ready_at"]),
        time::Duration::days(7)
    );
    // Its request's time, which the test moved a day back.
    let requested = one(
        &api,
        &format!(
            "SELECT to_char(created_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
             FROM exports WHERE id = '{ready_id}'"
        ),
    )
    .await;
    assert_eq!(ready["created_at"], requested.as_str());
    assert_eq!(pending["state"], "pending");
    assert_eq!(pending["ready_at"], json!(null));
    assert_eq!(pending["size_bytes"], json!(null));
    assert_eq!(
        moment(&pending["expires_at"]) - moment(&pending["created_at"]),
        time::Duration::days(7)
    );
}

#[tokio::test]
async fn accounts_023_the_job_builds_the_archive_makes_the_export_ready_and_publishes_it() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
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
    // The email of ACCOUNTS-025, which the run enqueued, and which the run of its job sent.
    let emails = mailbox::to(&address).await;
    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].subject, "Your AntWorker data export is ready");
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
    // The run ended its job, which no attempt waits to retry, and recorded and published nothing
    // more.
    let jobs = one(&api, "SELECT count(*)::text FROM jobs").await;
    assert_eq!(jobs, "0");
    assert_eq!(entries_of(&api, "export.completed").await.len(), 1);
    assert_eq!(event_users(&api, "export.completed").await.len(), 1);
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
async fn accounts_023_a_last_attempt_that_fails_once_another_made_the_export_failed_adds_nothing() {
    let api = Api::new().await;
    let db = api.database();
    let ada = users::register(&api, "ada@example.com").await;
    let id = request(&api, &ada).await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The job's tenth attempt, the last.
    db.owner_executes("UPDATE jobs SET attempts = 9 WHERE kind = 'accounts.export'")
        .await;
    // The expiry holds the export's row as it makes it failed (ACCOUNTS-027), and the run's
    // first statement, its lock of the row, waits for it: the row then changed after the run's
    // snapshot, which the lock refuses, so the run fails.
    let mut expiry = db
        .holding(&format!(
            "SELECT 1 FROM exports WHERE id = '{id}' FOR UPDATE"
        ))
        .await;

    let ((), runs) = tokio::join!(
        async {
            query_waits_on_a_lock(db, "FROM exports").await;
            execute_on(
                &mut expiry,
                &format!("UPDATE exports SET state = 'failed' WHERE id = '{id}'"),
            )
            .await;
            execute_on(&mut expiry, "COMMIT").await;
        },
        api.run_jobs(),
    );

    assert_eq!(runs, 1);
    let state = one(
        &api,
        &format!("SELECT state::text FROM exports WHERE id = '{id}'"),
    )
    .await;
    assert_eq!(state, "failed");
    // The run found the export failed already, so it recorded and published nothing.
    assert!(entries_of(&api, "export.failed").await.is_empty());
    assert!(event_users(&api, "export.failed").await.is_empty());
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

#[tokio::test]
async fn accounts_023_the_archive_holds_every_module_s_data_as_of_one_moment() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let id = request(&api, &ada).await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The run's first statement after the one that sets its isolation level locks the export's
    // row, so it waits for this transaction, which holds the row.
    let mut held = db
        .holding(&format!(
            "SELECT 1 FROM exports WHERE id = '{id}' FOR UPDATE"
        ))
        .await;

    let run = api.run_jobs();
    let meanwhile = async {
        query_waits_on_a_lock(db, "FROM exports").await;
        // Both commit while the run waits: a workspace of Ada's, which the contract of
        // `workspaces` reads first, and a name for her session's device, which the contract of
        // `auth` reads later.
        workspaces::arrange(db, "Ada's notes", &[(&ada.user_id, "owner")]).await;
        db.owner_executes(&format!(
            "UPDATE sessions SET device_name = 'Ada''s laptop' WHERE id = '{}'",
            ada.session_id
        ))
        .await;
        execute_on(&mut held, "ROLLBACK").await;
    };
    tokio::join!(run, meanwhile);

    let export = api
        .get(&format!("/v1/users/me/exports/{id}"))
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(export.json()["state"], "ready");
    let archive = download(&api, &ada, &id, &[]).await.body;
    assert!(document(&archive, "workspaces/workspaces.json").is_empty());
    assert!(document(&archive, "access/memberships.json").is_empty());
    let sessions = document(&archive, "auth/sessions.json");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["device_name"], Value::Null);
}

/// The names of the workspaces whose only member is Ada that [`arranged`] creates, in their order.
const OWN: [&str; 2] = ["Ada's notes", "Ada's drafts"];

/// Ada, with two workspaces of her own, named [`OWN`], and one of Grace's, an entry of her own
/// log that Grace acted in, and an entry of each workspace's log, among what each workspace
/// holds; returns Ada, Grace, her own workspaces' IDs, in the order of [`OWN`], and Grace's
/// workspace's ID.
async fn arranged(api: &Api) -> (SignedIn, SignedIn, Vec<String>, String) {
    let ada = users::register(api, "ada@example.com").await;
    let grace = users::register(api, "grace@example.com").await;
    let db = api.database();
    let mut own = Vec::new();
    for name in OWN {
        own.push(workspaces::arrange(db, name, &[(&ada.user_id, "owner")]).await);
    }
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
    audit::in_own_log(db, &ada.user_id, &grace.user_id, client).await;
    for workspace in own.iter().chain([&shared]) {
        audit::in_workspace_log(db, workspace, &ada.user_id).await;
    }
    // part: invitations
    // An invitation of each workspace.
    for (workspace, email) in own.iter().zip(INVITED) {
        let mine = invitations::invite(api, &ada, workspace, email, "member").await;
        assert_eq!(mine.status, 201);
    }
    let theirs = invitations::invite(api, &grace, &shared, "max@example.com", "member").await;
    assert_eq!(theirs.status, 201);
    // end part: invitations
    (ada, grace, own, shared)
}

// part: invitations
/// The address that [`arranged`] invites to each of Ada's own workspaces, in the order of [`OWN`].
const INVITED: [&str; 2] = ["lin@example.com", "joan@example.com"];
// end part: invitations

/// The strings of `items`, sorted.
fn sorted<'a>(items: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut items: Vec<&str> = items.into_iter().collect();
    items.sort_unstable();
    items
}

/// The workspaces and the memberships that `archive`, Ada's, holds: each of her own workspaces,
/// `own`, named as in [`OWN`], with its `id`, `name`, `created_at` and `updated_at` alone, and
/// her three memberships, with their roles.
fn assert_workspaces(archive: &[u8], own: &[String], shared: &str) {
    let workspaces = document(archive, "workspaces/workspaces.json");
    let mut held: Vec<(String, String)> = workspaces
        .iter()
        .map(|workspace| {
            let keys = sorted(workspace.as_object().unwrap().keys().map(String::as_str));
            assert_eq!(keys, ["created_at", "id", "name", "updated_at"]);
            (
                workspace["id"].as_str().unwrap().to_owned(),
                workspace["name"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    held.sort();
    let mut named: Vec<(String, String)> =
        own.iter().cloned().zip(OWN.map(str::to_owned)).collect();
    named.sort();
    assert_eq!(held, named);
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
    let mut expected: Vec<(String, String)> = own
        .iter()
        .map(|workspace| (workspace.clone(), "owner".to_owned()))
        .collect();
    expected.push((shared.to_owned(), "member".to_owned()));
    expected.sort();
    assert_eq!(roles, expected);
}

/// The logs that `archive`, Ada's, holds: her own, with no client of an entry Grace acted in, and
/// those of her own workspaces, `own`, alone, each of which has an entry there.
fn assert_logs(archive: &[u8], grace: &SignedIn, own: &[String]) {
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
    let mut logged = sorted(
        workspace_logs
            .iter()
            .map(|entry| entry["workspace_id"].as_str().unwrap()),
    );
    logged.dedup();
    assert_eq!(
        logged,
        sorted(own.iter().map(String::as_str)),
        "{workspace_logs:?}"
    );
}

// part: invitations
/// The invitations that `archive`, Ada's, holds: the one of each of her own workspaces, `own`, to
/// its address in [`INVITED`], and nothing of the invitation of Grace's workspace.
fn assert_invitations(archive: &[u8], own: &[String]) {
    let invited = document(archive, "invitations/invitations.json");
    let mut held: Vec<(&str, &str)> = invited
        .iter()
        .map(|invitation| {
            (
                invitation["workspace_id"].as_str().unwrap(),
                invitation["email"].as_str().unwrap(),
            )
        })
        .collect();
    held.sort_unstable();
    let mut sent: Vec<(&str, &str)> = own.iter().map(String::as_str).zip(INVITED).collect();
    sent.sort_unstable();
    assert_eq!(held, sent, "{invited:?}");
    assert!(!holds(archive, "max@example.com"));
}
// end part: invitations

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
    // A code of Ada's, which the job of a code issues no account that proved its address, as
    // hers has (AUTH-030), so the export's document of codes has a record to show, and a name for
    // her session's device, which is personal data the export holds (AUTH-042).
    api.database()
        .owner_executes(&format!(
            "INSERT INTO verification_codes (user_id, purpose, code_hash, expires_at) \
             VALUES ('{}', 'email_verification', '\\x0123abcd', now() + interval '10 minutes'); \
             UPDATE sessions SET device_name = 'Ada''s laptop' WHERE user_id = '{}'",
            ada.user_id, ada.user_id
        ))
        .await;
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
        "auth/verification_codes.json",
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
    let sessions = document(&archive, "auth/sessions.json");
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0]["id"], ada.session_id.as_str());
    assert_eq!(sessions[0]["device_name"], "Ada's laptop");
    for hash in [
        "access_token_hash",
        "refresh_token_hash",
        "previous_refresh_token_hash",
    ] {
        assert!(sessions[0].get(hash).is_none(), "{hash}");
    }
    assert_eq!(document(&archive, "auth/device_tokens.json").len(), 1);
    let codes = document(&archive, "auth/verification_codes.json");
    assert_eq!(codes.len(), 1);
    assert_eq!(codes[0]["purpose"], "email_verification");
    assert!(codes[0].get("code_hash").is_none());
    assert_logs(&archive, &grace, &own);
    // part: invitations
    assert_invitations(&archive, &own);
    // end part: invitations
    // No hash of a password or a token, of either user's, and nothing of Grace's but her ID.
    for absent in ["$argon2", "Grace's team", "grace@example.com", "192.0.2.7"] {
        assert!(!holds(&archive, absent), "{absent}");
    }
    for (table, column) in [
        ("sessions", "access_token_hash"),
        ("sessions", "refresh_token_hash"),
        ("device_tokens", "token_hash"),
        ("verification_codes", "code_hash"),
    ] {
        let query = format!("SELECT encode({column}, 'hex') FROM {table}");
        for hash in api.database().owner_reads(&query).await {
            assert!(!holds(&archive, &hash), "{table}.{column}");
        }
    }
}

#[tokio::test]
async fn accounts_024_the_archive_holds_the_second_factors_without_their_secrets_or_hashes() {
    let api = Api::new().await;
    let address = mailbox::address("ada");
    let ada = users::register(&api, &address).await;
    let app = crate::support::second_factors::add_app(&api, &ada).await;
    // A sign-in that waits for its code, from a named device, and a new setup, which waits for
    // its confirmation.
    let challenged = api
        .post(
            "/v1/sessions",
            &json!({ "email": address, "password": users::PASSWORD,
                     "device_id": "phone", "device_name": "Ada's phone" }),
        )
        .send()
        .await;
    assert_eq!(challenged.status, 200);
    let setup = crate::support::second_factors::start_setup(&api, &ada).await;
    assert_eq!(setup.status, 200);
    let id = built(&api, &ada).await;

    let archive = download(&api, &ada, &id, &[]).await.body;

    let apps = document(&archive, "auth/totp_factors.json");
    assert_eq!(apps.len(), 1);
    assert!(apps[0]["last_used_step"].is_i64(), "{apps:?}");
    assert!(apps[0].get("secret").is_none());
    let setups = document(&archive, "auth/totp_setups.json");
    assert_eq!(setups.len(), 1);
    assert!(setups[0].get("secret").is_none());
    assert_eq!(document(&archive, "auth/recovery_codes.json").len(), 10);
    let challenges = document(&archive, "auth/sign_in_challenges.json");
    assert_eq!(challenges.len(), 1);
    assert_eq!(challenges[0]["device_id"], "phone");
    assert_eq!(challenges[0]["device_name"], "Ada's phone");
    // No secret, no code and no hash of one.
    assert!(!holds(&archive, &app.secret));
    assert!(!holds(&archive, setup.json()["secret"].as_str().unwrap()));
    assert!(!holds(&archive, &app.recovery_codes[0]));
    for (table, column) in [
        ("totp_factors", "secret"),
        ("totp_setups", "secret"),
        ("recovery_codes", "code_hash"),
        ("sign_in_challenges", "token_hash"),
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
    let api = Api::without_email().await;
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
    // Ready two days ago, from a request three days ago, so the day it became ready is neither
    // its request's nor today's.
    api.database()
        .owner_executes(&format!(
            "UPDATE exports SET created_at = created_at - interval '3 days', \
             ready_at = ready_at - interval '2 days', expires_at = expires_at - interval '2 days' \
             WHERE id = '{id}'"
        ))
        .await;

    let whole = download(&api, &ada, &id, &[]).await;
    // A change since the export was built changes none of its bytes.
    let renamed = users::rename(&api, &ada, "Countess of Lovelace", None).await;
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
    // `filename*` encodes the product's name, which may hold spaces once `just rename` gives it
    // another (ARCH-15).
    let name = format!("AntWorker-export-{ready}.zip");
    assert_eq!(
        whole.header("content-disposition"),
        format!(
            "attachment; filename=\"{name}\"; filename*=UTF-8''{}",
            name.replace(' ', "%20")
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
    assert_eq!(
        content.json()["detail"],
        "The export's archive isn't built yet, or its job failed and it never will be."
    );
    let downloads = one(
        &api,
        "SELECT count(*)::text FROM audit_entries WHERE action = 'export.downloaded'",
    )
    .await;
    assert_eq!(downloads, "0");
}

#[tokio::test]
async fn accounts_026_a_document_stored_in_chunks_serves_the_same_bytes_whole_and_from_inside_one()
{
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let id = built(&api, &ada).await;
    let whole = download(&api, &ada, &id, &[]).await;

    let (path, third, last) = in_thirds(&api, &id).await;
    let again = download(&api, &ada, &id, &[]).await;
    // From the second byte of the first chunk to the second of the third.
    let data = entry_data(&whole.body, &path);
    let (start, end) = (data + 1, data + last + 1);
    let range = download(
        &api,
        &ada,
        &id,
        &[("range", &format!("bytes={start}-{end}"))],
    )
    .await;

    assert!(third >= 2);
    assert_eq!(again.status, 200);
    assert_eq!(again.body, whole.body);
    assert_eq!(range.status, 206);
    assert_eq!(range.body, whole.body[start..=end]);
}

#[tokio::test]
async fn accounts_026_a_document_that_lacks_a_chunk_cuts_the_download_short() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let id = built(&api, &ada).await;
    let (path, second, _) = in_thirds(&api, &id).await;
    api.database()
        .owner_executes(&format!(
            "DELETE FROM export_chunks WHERE export_id = '{id}' AND offset_bytes = {second} \
             AND position = (SELECT position FROM export_entries \
                 WHERE export_id = '{id}' AND path = '{path}')"
        ))
        .await;

    // Sent to the router itself, since the support's requests read a body to its end.
    let mut request = http::Request::get(format!("/v1/users/me/exports/{id}/content"))
        .header("authorization", format!("Bearer {}", ada.access))
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        crate::support::api::PEER
            .parse::<std::net::SocketAddr>()
            .unwrap(),
    ));
    let response = api.router().oneshot(request).await.unwrap();
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await;

    assert_eq!(status, 200);
    assert!(body.is_err(), "the body ended as one that held every byte");
    let cut: Vec<_> = testing::logged_on_this_thread()
        .into_iter()
        .filter(|line| line["error.type"] == "download_cut")
        .collect();
    assert_eq!(cut.len(), 1, "{cut:#?}");
    assert_eq!(
        cut[0]["exception.message"],
        "internal error: an export's document lacks a chunk of its bytes"
    );
}

#[tokio::test]
async fn accounts_027_the_expiry_runs_every_hour() {
    let api = Api::new().await;

    let interval = api.interval("accounts.expire_exports");

    assert_eq!(interval, crate::support::jobs::HOUR);
}

#[tokio::test]
async fn accounts_027_the_expiry_deletes_expired_exports_and_fails_those_pending_for_a_day() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let lin = users::register(&api, "lin@example.com").await;
    let alan = users::register(&api, "alan@example.com").await;
    let expired = built(&api, &ada).await;
    let stale = request(&api, &grace).await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let older = request(&api, &lin).await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let young = request(&api, &alan).await.json()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let db = api.database();
    db.owner_executes("DELETE FROM jobs").await;
    db.owner_executes(&format!(
        "UPDATE exports SET expires_at = now() - interval '1 second' WHERE id = '{expired}'"
    ))
    .await;
    // Grace's export was requested 24 hours ago, Lin's 2 days ago, and Alan's 23 hours and 59
    // minutes ago.
    db.owner_executes(&format!(
        "UPDATE exports SET created_at = now() - CASE id WHEN '{stale}' \
         THEN interval '24 hours' WHEN '{older}' THEN interval '2 days' \
         ELSE interval '23 hours 59 minutes' END \
         WHERE id IN ('{stale}', '{older}', '{young}')"
    ))
    .await;
    let mut stream = streams::opened(&api, &grace).await;

    api.run_periodic("accounts.expire_exports").await;
    api.run_periodic("accounts.expire_exports").await;

    let left = db
        .owner_reads("SELECT id || ' ' || state::text FROM exports ORDER BY created_at")
        .await;
    assert_eq!(
        left,
        [
            format!("{older} failed"),
            format!("{stale} failed"),
            format!("{young} pending")
        ]
    );
    let documents = one(
        &api,
        "SELECT (SELECT count(*) FROM export_entries) || ' ' || \
         (SELECT count(*) FROM export_chunks)",
    )
    .await;
    assert_eq!(documents, "0 0");
    // Each failed export's event, published to its own user, once.
    let mut published = db
        .owner_reads(
            "SELECT user_id || ' ' || (payload->>'export_id') FROM events \
             WHERE name = 'export.failed'",
        )
        .await;
    published.sort_unstable();
    let mut expected = vec![
        format!("{} {stale}", grace.user_id),
        format!("{} {older}", lin.user_id),
    ];
    expected.sort_unstable();
    assert_eq!(published, expected);
    let event = stream.event().await;
    assert_eq!(event.event.as_deref(), Some("export.failed"));
    assert_eq!(event.payload(), &json!({ "export_id": stale }));
}

#[tokio::test]
async fn accounts_027_a_run_fails_and_deletes_more_exports_than_one_batch_holds() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    // More than two batches of 1,000 of each kind: exports whose job failed, whose `expires_at`,
    // 7 days after their request, passed a second ago, and exports still pending a second past
    // 24 hours after their request.
    api.database()
        .owner_executes(&format!(
            "INSERT INTO exports (user_id, state, created_at, expires_at) \
             SELECT '{0}', 'failed', now() - interval '7 days 1 second', \
             now() - interval '1 second' FROM generate_series(1, 2500); \
             INSERT INTO exports (user_id, created_at, expires_at) \
             SELECT '{0}', now() - interval '24 hours 1 second', now() + interval '6 days' \
             FROM generate_series(1, 2500)",
            ada.user_id
        ))
        .await;

    api.run_periodic("accounts.expire_exports").await;

    // The expired exports that are left, the pending ones, the failed ones that haven't expired,
    // and the events of those that failed.
    let left = one(
        &api,
        "SELECT (SELECT count(*) FROM exports WHERE expires_at <= now()) || ' ' || \
         (SELECT count(*) FROM exports WHERE state = 'pending') || ' ' || \
         (SELECT count(*) FROM exports WHERE state = 'failed' AND expires_at > now()) || ' ' || \
         (SELECT count(*) FROM events WHERE name = 'export.failed')",
    )
    .await;
    assert_eq!(left, "0 0 2500 2500");
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
        "default-src 'none'; base-uri 'none'; frame-ancestors 'none'; sandbox"
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

#[tokio::test]
async fn accounts_024_the_archive_holds_the_provider_identities_and_each_session_s_provider() {
    let upstream = crate::support::upstream::Upstream::start().await;
    let api = Api::with(providers::stand_in(&upstream)).await;
    providers::gives(
        &upstream,
        providers::identity("subject-1", &mailbox::address("ada"), true),
    );
    let ada = providers::signed_in(&api).await;
    let id = built(&api, &ada).await;

    let archive = download(&api, &ada, &id, &[]).await.body;

    let identities = document(&archive, "auth/provider_identities.json");
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0]["provider"], "stand_in");
    assert_eq!(identities[0]["subject"], "subject-1");
    let sessions = document(&archive, "auth/sessions.json");
    assert_eq!(sessions[0]["provider"], "stand_in");
}
