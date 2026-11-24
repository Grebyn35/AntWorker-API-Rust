//! Builders of audit entries, through the `owner` connection, for the tests of what reads a log,
//! such as an export (TEST-09).

use super::TestDatabase;

/// The client of an entry: its IP address and user agent, and the user whose they are.
pub struct Client<'a> {
    pub user: &'a str,
    pub ip: &'a str,
    pub agent: &'a str,
}

/// Records `member.removed` in `user`'s own log, acted by `actor`, with `client` as its client,
/// as `audit::record` records an entry, and returns its ID.
pub async fn in_own_log(db: &TestDatabase, user: &str, actor: &str, client: Client<'_>) -> String {
    let id = db
        .owner_reads(&format!(
            "INSERT INTO audit_entries (scope, user_id, actor_user_id, action, resource_type) \
             VALUES ('user', '{user}', '{actor}', 'member.removed', 'workspace') RETURNING id::text"
        ))
        .await
        .remove(0);
    with_client(db, &id, &client).await;
    id
}

/// Records `workspace.renamed` in `workspace`'s log, acted by `actor`, as `audit::record` records
/// an entry, and returns its ID.
pub async fn in_workspace_log(db: &TestDatabase, workspace: &str, actor: &str) -> String {
    db.owner_reads(&format!(
        "INSERT INTO audit_entries (scope, workspace_id, actor_user_id, action, resource_type) \
         VALUES ('workspace', '{workspace}', '{actor}', 'workspace.renamed', 'workspace') \
         RETURNING id::text"
    ))
    .await
    .remove(0)
}

/// The log that holds an entry that [`Logged`] records: a workspace's, or a user's own, by its
/// ID.
#[derive(Clone, Copy)]
pub enum Log<'a> {
    Workspace(&'a str),
    Own(&'a str),
}

/// An entry of a log, which [`Logged::record`] records through the `owner` connection, as
/// `audit::record` records one, for the tests of what reads a log (TEST-09).
pub struct Logged<'a> {
    pub log: Log<'a>,
    pub actor: &'a str,
    pub action: &'a str,
    /// The kind of resource it happened to, and the resource's ID.
    pub resource: (&'a str, &'a str),
    pub target: Option<&'a str>,
    /// The permission that reading it in a workspace's log takes beside `audit.read`, if any.
    pub permission: Option<&'a str>,
    /// When it was recorded, an SQL timestamp, such as `2026-11-01T12:00:00Z`, or, if not given,
    /// the database's `now()`.
    pub at: Option<&'a str>,
}

impl<'a> Logged<'a> {
    /// `action` in `workspace`'s log, acted by `actor` on the workspace, done to no target, whose
    /// reading takes no permission beside `audit.read`.
    pub fn new(workspace: &'a str, actor: &'a str, action: &'a str) -> Self {
        Self {
            log: Log::Workspace(workspace),
            actor,
            action,
            resource: ("workspace", workspace),
            target: None,
            permission: None,
            at: None,
        }
    }

    /// `action` in `user`'s own log, acted by `user` on their account, done to no target.
    pub fn own(user: &'a str, action: &'a str) -> Self {
        Self {
            log: Log::Own(user),
            actor: user,
            action,
            resource: ("user", user),
            target: None,
            permission: None,
            at: None,
        }
    }

    /// The entry, recorded at `at`, an SQL timestamp.
    pub fn at(self, at: &'a str) -> Self {
        Self {
            at: Some(at),
            ..self
        }
    }

    /// Records the entry, and returns its ID.
    pub async fn record(&self, db: &TestDatabase) -> String {
        let text =
            |value: Option<&str>| value.map_or("NULL".to_owned(), |value| format!("'{value}'"));
        let (scope, user, workspace) = match self.log {
            Log::Workspace(workspace) => ("workspace", None, Some(workspace)),
            Log::Own(user) => ("user", Some(user), None),
        };
        db.owner_reads(&format!(
            "INSERT INTO audit_entries (scope, user_id, workspace_id, actor_user_id, \
             target_user_id, action, resource_type, resource_id, permission, created_at) \
             VALUES ('{scope}', {}, {}, '{}', {}, '{}', '{}', '{}', {}, coalesce({}, now())) \
             RETURNING id::text",
            text(user),
            text(workspace),
            self.actor,
            text(self.target),
            self.action,
            self.resource.0,
            self.resource.1,
            text(self.permission),
            text(self.at),
        ))
        .await
        .remove(0)
    }
}

/// Records 6,000 entries of `tests.noise` in `user`'s own log, each with details of about 1,000
/// characters of hexadecimal noise, which deflate to about half their size, in one statement.
pub async fn noise(db: &TestDatabase, user: &str) {
    db.owner_executes(&format!(
        "INSERT INTO audit_entries (scope, user_id, action, resource_type, details) \
         SELECT 'user', '{user}', 'tests.noise', 'tests', jsonb_build_object('noise', \
             (SELECT string_agg(md5(random()::text || n || p), '') \
              FROM generate_series(1, 30) AS q (p))) \
         FROM generate_series(1, 6000) AS r (n)"
    ))
    .await;
}

/// How many entries of `action` record a client with an IP address or a user agent, which no
/// entry of an operator's subcommand does.
pub async fn with_a_client(db: &TestDatabase, action: &str) -> usize {
    db.owner_reads(&format!(
        "SELECT count(*)::text FROM audit_entry_clients c JOIN audit_entries e \
         ON e.id = c.audit_entry_id WHERE e.action = '{action}' \
         AND (c.client_ip IS NOT NULL OR c.user_agent IS NOT NULL)"
    ))
    .await[0]
        .parse()
        .unwrap()
}

/// Records `client` as the client of the entry `entry`.
pub async fn with_client(db: &TestDatabase, entry: &str, client: &Client<'_>) {
    let agent = client.agent.replace('\'', "''");
    db.owner_executes(&format!(
        "INSERT INTO audit_entry_clients (audit_entry_id, user_id, client_ip, user_agent) \
         VALUES ('{entry}', '{}', '{}', '{agent}')",
        client.user, client.ip
    ))
    .await;
}
