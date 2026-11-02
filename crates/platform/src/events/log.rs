//! The hub's reads of the event log: its head, the events it loads by position, and a stream's
//! replay (PLATFORM-057, PLATFORM-071, SEC-17). `events` is `platform`'s bookkeeping: the hub
//! reads it by position, and a stream's reads by its caller's ID and memberships (DATA-22).
//!
//! A workspace event reaches a member whose role holds its permission, and whose membership had
//! committed when the event did: the transaction that added the membership is the event's own, or
//! among those its commit's snapshot holds, as `pg_visible_in_snapshot` reads it (SEC-17).

use std::sync::Arc;

use super::subscription::{Audience, Event};
use crate::{
    authz::{Committed, Memberships},
    db::UnitOfWork,
    errors::Internal,
    types::{UserId, WorkspaceId},
};

/// The most events one query loads.
pub(super) const BATCH: i64 = 1_000;

/// The most events a replay sends (REALTIME-004).
pub(super) const REPLAY_LIMIT: i64 = 1_000;

/// The last position given so far, or 0 before any.
pub(super) async fn head(uow: &mut UnitOfWork) -> Result<i64, Internal> {
    Ok(
        sqlx::query_scalar!(r#"SELECT coalesce(max(position), 0) AS "head!" FROM events"#)
            .fetch_one(uow.executor())
            .await?,
    )
}

/// The events after `after`, up to `through`, in the order of their positions, at most
/// [`BATCH`], each with how long ago it committed, by the database's clock.
pub(super) async fn load(
    uow: &mut UnitOfWork,
    after: i64,
    through: i64,
) -> Result<Vec<(Arc<Event>, f64)>, Internal> {
    let rows = sqlx::query!(
        r#"SELECT id, user_id, workspace_id, type AS name, permission, payload, traceparent,
            position AS "position!", xact::text AS xact, snapshot::text AS snapshot,
            extract(epoch FROM clock_timestamp() - created_at)::float8 AS "lag!"
        FROM events
        WHERE position > $1 AND position <= $2
        ORDER BY position
        LIMIT $3"#,
        after,
        through,
        BATCH,
    )
    .fetch_all(uow.executor())
    .await?;
    let mut events = Vec::with_capacity(rows.len());
    for row in rows {
        let audience = match (row.user_id, row.workspace_id, row.permission) {
            (Some(user), None, None) => Audience::User(UserId::from_uuid(user)),
            (None, Some(workspace), Some(permission)) => {
                Audience::Workspace(WorkspaceId::from_uuid(workspace), permission)
            }
            _ => {
                return Err(Internal::new(
                    "an event has neither a user's nor a workspace's scope",
                ));
            }
        };
        let event = Event {
            id: row.id,
            name: row.name,
            payload: row.payload,
            trace_id: trace_id(row.traceparent.as_deref()),
            traceparent: row.traceparent,
            position: row.position,
            audience,
            committed: committed(row.xact.as_deref(), row.snapshot.as_deref()),
        };
        events.push((Arc::new(event), row.lag));
    }
    Ok(events)
}

/// The position of the event `id`, or `None` when the API holds no such event. The event may be
/// one the caller may not receive now, such as the last of a workspace they left: its position
/// marks where their stream resumes all the same (REALTIME-004).
pub(super) async fn position(
    uow: &mut UnitOfWork,
    id: uuid::Uuid,
) -> Result<Option<i64>, Internal> {
    Ok(
        sqlx::query_scalar!("SELECT position FROM events WHERE id = $1", id)
            .fetch_optional(uow.executor())
            .await?
            .flatten(),
    )
}

/// The events after `after`, up to `through`, that `user` with `memberships` may receive, in
/// order: their own, and those of each of their workspaces whose permission their role holds
/// there, committed after their membership (SEC-17). At most [`REPLAY_LIMIT`] and one more, which
/// tells that the replay would be cut short.
pub(super) async fn replay(
    uow: &mut UnitOfWork,
    user: UserId,
    memberships: &Memberships,
    after: i64,
    through: i64,
) -> Result<Vec<Arc<Event>>, Internal> {
    let (workspaces, permissions, joined) = memberships.rows();
    let rows = sqlx::query!(
        r#"SELECT id AS "id!", type AS "name!", payload AS "payload!", traceparent,
            position AS "position!", user_id, workspace_id, permission, xact, snapshot
        FROM (
            SELECT id, type, payload, traceparent, position, user_id, workspace_id, permission,
                xact::text AS xact, snapshot::text AS snapshot
            FROM events
            WHERE user_id = $3 AND position > $1 AND position <= $2
            UNION ALL
            SELECT e.id, e.type, e.payload, e.traceparent, e.position, e.user_id, e.workspace_id,
                e.permission, e.xact::text, e.snapshot::text
            FROM unnest($4::uuid[], $5::text[], $6::text[])
                AS m(workspace_id, permission, joined)
            JOIN events AS e ON e.workspace_id = m.workspace_id
                AND e.permission = m.permission
                AND (m.joined::xid8 = e.xact OR pg_visible_in_snapshot(m.joined::xid8, e.snapshot))
                AND e.position > $1 AND e.position <= $2
        ) AS visible
        ORDER BY position
        LIMIT $7"#,
        after,
        through,
        user.uuid(),
        &workspaces,
        &permissions,
        &joined,
        REPLAY_LIMIT + 1,
    )
    .fetch_all(uow.executor())
    .await?;
    let events = rows
        .into_iter()
        .map(|row| {
            let audience = match (row.workspace_id, row.permission) {
                (Some(workspace), Some(permission)) => {
                    Audience::Workspace(WorkspaceId::from_uuid(workspace), permission)
                }
                _ => Audience::User(user),
            };
            Arc::new(Event {
                id: row.id,
                name: row.name,
                payload: row.payload,
                trace_id: trace_id(row.traceparent.as_deref()),
                traceparent: row.traceparent,
                position: row.position,
                audience,
                committed: committed(row.xact.as_deref(), row.snapshot.as_deref()),
            })
        })
        .collect();
    Ok(events)
}

/// The newest event, at or before `through`, that `user` with `memberships` may receive, as
/// the replay reads them, from which a reconnect resumes a stream that started after `through`: no
/// event it may receive lies between the two (REALTIME-001).
pub(super) async fn newest_visible(
    uow: &mut UnitOfWork,
    user: UserId,
    memberships: &Memberships,
    through: i64,
) -> Result<Option<uuid::Uuid>, Internal> {
    let (workspaces, permissions, joined) = memberships.rows();
    Ok(sqlx::query_scalar!(
        r#"SELECT id AS "id!" FROM (
            SELECT id, position FROM events WHERE user_id = $2 AND position <= $1
            UNION ALL
            SELECT e.id, e.position
            FROM unnest($3::uuid[], $4::text[], $5::text[])
                AS m(workspace_id, permission, joined)
            JOIN events AS e ON e.workspace_id = m.workspace_id
                AND e.permission = m.permission
                AND (m.joined::xid8 = e.xact OR pg_visible_in_snapshot(m.joined::xid8, e.snapshot))
                AND e.position <= $1
        ) AS visible
        ORDER BY position DESC
        LIMIT 1"#,
        through,
        user.uuid(),
        &workspaces,
        &permissions,
        &joined,
    )
    .fetch_optional(uow.executor())
    .await?)
}

/// The transactions that had committed with an event, from its stored `xact` and `snapshot`.
fn committed(xact: Option<&str>, snapshot: Option<&str>) -> Option<Committed> {
    Committed::parse(xact?, snapshot?)
}

/// The trace ID of a stored `traceparent`, `00-<trace ID>-<span ID>-<flags>`.
fn trace_id(traceparent: Option<&str>) -> Option<String> {
    traceparent
        .and_then(|traceparent| traceparent.split('-').nth(1))
        .map(str::to_owned)
}
