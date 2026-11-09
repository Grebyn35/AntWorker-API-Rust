//! Builders of audit entries, through the `owner` connection, for the tests of what reads a log,
//! such as an export (TEST-09).

use super::TestDatabase;

/// The client of an entry: its IP address and user agent, and the user whose they are.
pub struct Client<'a> {
    pub user: &'a str,
    pub ip: &'a str,
    pub agent: &'a str,
}

/// Records `action` in `user`'s own log, acted by `actor`, with `client` as its client if given,
/// as `audit::record` records an entry, and returns its ID.
pub async fn in_own_log(
    db: &TestDatabase,
    user: &str,
    actor: &str,
    action: &str,
    client: Option<Client<'_>>,
) -> String {
    let id = db
        .owner_reads(&format!(
            "INSERT INTO audit_entries (scope, user_id, actor_user_id, action, resource_type) \
             VALUES ('user', '{user}', '{actor}', '{action}', 'workspace') RETURNING id::text"
        ))
        .await
        .remove(0);
    if let Some(client) = client {
        with_client(db, &id, &client).await;
    }
    id
}

/// Records `action` in `workspace`'s log, acted by `actor`, as `audit::record` records an entry,
/// and returns its ID.
pub async fn in_workspace_log(
    db: &TestDatabase,
    workspace: &str,
    actor: &str,
    action: &str,
) -> String {
    db.owner_reads(&format!(
        "INSERT INTO audit_entries (scope, workspace_id, actor_user_id, action, resource_type) \
         VALUES ('workspace', '{workspace}', '{actor}', '{action}', 'workspace') RETURNING id::text"
    ))
    .await
    .remove(0)
}

/// Records `count` entries of `action` in `user`'s own log, each with details of about 1,000
/// characters of hexadecimal noise, which deflate to about half their size, in one statement.
pub async fn noise(db: &TestDatabase, user: &str, action: &str, count: u32) {
    db.owner_executes(&format!(
        "INSERT INTO audit_entries (scope, user_id, action, resource_type, details) \
         SELECT 'user', '{user}', '{action}', 'tests', jsonb_build_object('noise', \
             (SELECT string_agg(md5(random()::text || n || p), '') \
              FROM generate_series(1, 30) AS q (p))) \
         FROM generate_series(1, {count}) AS r (n)"
    ))
    .await;
}

/// Records `client` as the client of the entry `entry`.
async fn with_client(db: &TestDatabase, entry: &str, client: &Client<'_>) {
    let agent = client.agent.replace('\'', "''");
    db.owner_executes(&format!(
        "INSERT INTO audit_entry_clients (entry_id, user_id, client_ip, user_agent) \
         VALUES ('{entry}', '{}', '{}', '{agent}')",
        client.user, client.ip
    ))
    .await;
}
