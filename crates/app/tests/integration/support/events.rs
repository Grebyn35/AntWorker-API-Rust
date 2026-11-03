//! Events that a test commits as a unit of work commits them, without the API: each with the next
//! position, the transaction's ID and snapshot, and the commit's time (PLATFORM-071), and, unless
//! the test leaves the hub to find them, the notification that names each position.

/// Whom an event reaches.
pub enum Scope<'a> {
    /// The user of this ID.
    User(&'a str),
    /// The members of the workspace of this ID who hold the permission.
    Workspace(&'a str, &'a str),
}

/// One event, by the columns of its row.
pub struct Event<'a> {
    /// Whom it reaches.
    pub scope: Scope<'a>,
    /// Its name, such as `note.made`.
    pub name: &'a str,
    /// Its payload, as JSON.
    pub payload: &'a str,
    /// The trace context of what published it, if any.
    pub traceparent: Option<&'a str>,
}

impl<'a> Event<'a> {
    /// The event `name` to the user `user_id`, with `payload` and no trace context.
    pub fn to_user(user_id: &'a str, name: &'a str, payload: &'a str) -> Self {
        Self {
            scope: Scope::User(user_id),
            name,
            payload,
            traceparent: None,
        }
    }

    /// The event `name` to the members of `workspace_id` who hold `permission`, with `payload`.
    pub fn to_workspace(
        workspace_id: &'a str,
        permission: &'a str,
        name: &'a str,
        payload: &'a str,
    ) -> Self {
        Self {
            scope: Scope::Workspace(workspace_id, permission),
            name,
            payload,
            traceparent: None,
        }
    }
}

/// The statement that commits `events` as a unit of work's commit does, and that notifies the hub
/// of each one's position when `notify`, as the commit does.
pub fn statement(events: &[Event<'_>], notify: bool) -> String {
    let quoted = |text: &str| format!("'{}'", text.replace('\'', "''"));
    let rows: Vec<String> = events
        .iter()
        .map(|event| {
            let (scope, user, workspace, permission) = match event.scope {
                Scope::User(user) => ("user", quoted(user), "NULL".to_owned(), "NULL".to_owned()),
                Scope::Workspace(workspace, permission) => (
                    "workspace",
                    "NULL".to_owned(),
                    quoted(workspace),
                    quoted(permission),
                ),
            };
            format!(
                "('{scope}', {user}, {workspace}, {}, {permission}, {}, {})",
                quoted(event.name),
                quoted(event.payload),
                event.traceparent.map_or("NULL".to_owned(), quoted)
            )
        })
        .collect();
    let insert = format!(
        "INSERT INTO events (scope, user_id, workspace_id, name, permission, payload, traceparent, \
         position, xact, snapshot, created_at) \
         SELECT scope::event_scope, user_id::uuid, workspace_id::uuid, name, permission, \
         payload::jsonb, traceparent, nextval('events_position_seq'), pg_current_xact_id(), \
         pg_current_snapshot(), clock_timestamp() \
         FROM (VALUES {}) AS v(scope, user_id, workspace_id, name, permission, payload, traceparent)",
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
