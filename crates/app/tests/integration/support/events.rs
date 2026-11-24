//! Events that a test commits as a unit of work commits them, without the API: each with the next
//! position, the transaction's ID and snapshot, and the commit's time (PLATFORM-028), or a time
//! before it, and, unless the test leaves the hub to find them, a notification of each one's
//! position, where a unit of work's commit notifies the last one's alone.

/// Whom an event reaches.
pub enum Scope<'a> {
    /// The user of this ID.
    User(&'a str),
    /// The members of the workspace of this ID who hold `workspace.read`.
    Workspace(&'a str),
}

/// One event, `note.made`, by the columns of its row.
pub struct Event<'a> {
    /// Whom it reaches.
    pub scope: Scope<'a>,
    /// Its payload, as JSON.
    pub payload: &'a str,
    /// The trace context of what published it, if any.
    pub traceparent: Option<&'a str>,
    /// How long before the statement it committed, as Postgres writes an interval, such as
    /// `1 minute`, for a hub that hears of it late; or `None`, when it commits with the statement.
    pub committed_before: Option<&'a str>,
}

impl<'a> Event<'a> {
    /// The event to the user `user_id`, with `payload` and no trace context.
    pub fn to_user(user_id: &'a str, payload: &'a str) -> Self {
        Self {
            scope: Scope::User(user_id),
            payload,
            traceparent: None,
            committed_before: None,
        }
    }

    /// The event to the members of `workspace_id` who hold `workspace.read`, with the payload `{}`
    /// and no trace context.
    pub fn to_workspace(workspace_id: &'a str) -> Self {
        Self {
            scope: Scope::Workspace(workspace_id),
            payload: "{}",
            traceparent: None,
            committed_before: None,
        }
    }
}

/// The statement that commits `events` as a unit of work's commit does, and, when `notify`,
/// notifies the hub of each one's position, where the commit notifies it of the last one's.
pub fn statement(events: &[Event<'_>], notify: bool) -> String {
    let quoted = |text: &str| format!("'{}'", text.replace('\'', "''"));
    let rows: Vec<String> = events
        .iter()
        .map(|event| {
            let (scope, user, workspace, permission) = match event.scope {
                Scope::User(user) => ("user", quoted(user), "NULL".to_owned(), "NULL"),
                Scope::Workspace(workspace) => (
                    "workspace",
                    "NULL".to_owned(),
                    quoted(workspace),
                    "'workspace.read'",
                ),
            };
            format!(
                "('{scope}', {user}, {workspace}, 'note.made', {permission}, {}, {}, {})",
                quoted(event.payload),
                event.traceparent.map_or("NULL".to_owned(), quoted),
                quoted(event.committed_before.unwrap_or("0 seconds"))
            )
        })
        .collect();
    let insert = format!(
        "INSERT INTO events (scope, user_id, workspace_id, name, permission, payload, traceparent, \
         position, xact, snapshot, created_at) \
         SELECT scope::event_scope, user_id::uuid, workspace_id::uuid, name, permission, \
         payload::jsonb, traceparent, nextval('events_position_seq'), pg_current_xact_id(), \
         pg_current_snapshot(), clock_timestamp() - committed_before::interval \
         FROM (VALUES {}) \
         AS v(scope, user_id, workspace_id, name, permission, payload, traceparent, committed_before)",
        rows.join(", ")
    );
    if notify {
        format!(
            "WITH published AS ({insert} RETURNING position) \
             SELECT pg_notify('events', position::text) FROM published"
        )
    } else {
        insert
    }
}
