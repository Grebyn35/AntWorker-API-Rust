//! A caller's stream, as the hub subscribed it: its replay, then what the hub delivers to it, then
//! the reason the hub closed it (REALTIME-004, REALTIME-005).

use std::{collections::VecDeque, sync::Arc};

use serde::Serialize;
use tokio::sync::{mpsc, watch};
use utoipa::ToSchema;

use super::hub::{Command, StreamId};
use crate::{
    authz::Committed,
    errors,
    types::{UserId, WorkspaceId},
};

/// Why the hub closed a stream, which `stream.closed` names (REALTIME-005).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = CloseReason)]
pub enum Closed {
    /// The caller's session ended, or stopped lasting.
    SessionEnded,
    /// The caller's memberships changed.
    AccessChanged,
    /// The client read too slowly to keep up (PLATFORM-032).
    Lagging,
    /// The instance is shutting down (PLATFORM-062).
    Shutdown,
    /// The listener was lost, so the hub may have missed signals (PLATFORM-033).
    Interrupted,
}

impl Closed {
    /// The reason, as `stream.closed` names it.
    pub fn reason(self) -> &'static str {
        match self {
            Self::SessionEnded => "session_ended",
            Self::AccessChanged => "access_changed",
            Self::Lagging => "lagging",
            Self::Shutdown => "shutdown",
            Self::Interrupted => "interrupted",
        }
    }
}

/// Whom an event goes to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Audience {
    /// A user's streams.
    User(UserId),
    /// The streams of a workspace's members whose role holds the permission of this name.
    Workspace(WorkspaceId, String),
}

/// A committed event, as the hub loaded it.
#[derive(Debug)]
pub struct Event {
    pub(crate) id: uuid::Uuid,
    pub(crate) name: String,
    pub(crate) payload: serde_json::Value,
    pub(crate) trace_id: Option<String>,
    pub(crate) traceparent: Option<String>,
    /// The user whose request published the event, if a `Principal` opened its unit of work.
    pub(crate) publisher: Option<UserId>,
    pub(crate) position: i64,
    pub(crate) audience: Audience,
    /// The transactions that had committed when the event did, which a workspace event's
    /// members' memberships must be among (SEC-19).
    pub(crate) committed: Option<Committed>,
}

impl Event {
    /// The event's ID, which a client resumes from with `Last-Event-ID`.
    pub fn id(&self) -> uuid::Uuid {
        self.id
    }

    /// The event's name, such as `member.removed` (API-19).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The event's payload, of IDs (API-19).
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }

    /// The trace ID of the request that published the event, as a stream of `user`'s reads it:
    /// the event's publisher alone, and no one when no `Principal` opened the event's unit of
    /// work, as for a job's, or when it published outside a trace (PLATFORM-095, D10).
    pub fn trace_id_for(&self, user: UserId) -> Option<&str> {
        if self.publisher == Some(user) {
            self.trace_id.as_deref()
        } else {
            None
        }
    }
}

/// What a stream sends next.
#[derive(Debug)]
pub enum Delivery {
    /// The stream is open: what commits from now on reaches it (REALTIME-001).
    Opened {
        /// Whether the stream skipped what waited for it, because more waited than a replay
        /// sends, or it resumed from an event the API doesn't hold, so the client reloads its
        /// state (REALTIME-004).
        truncated: bool,
        /// Where a reconnect resumes the stream from, for a stream that started with what commits
        /// next: the newest event its caller may receive from before it started, or the nil UUID,
        /// which names no event, when there is none. A resumed stream's own `Last-Event-ID`
        /// resumes it, so it gives none.
        resume: Option<uuid::Uuid>,
    },
    /// An event.
    Event(Arc<Event>),
    /// The hub closed the stream, which ends after this (REALTIME-005).
    Closed(Closed),
}

/// A caller's stream: the events of its replay, then those the hub delivers, each once and in
/// the order their units of work committed (PLATFORM-028). Dropping it removes the stream from
/// the hub.
#[derive(Debug)]
pub struct Subscription {
    pub(crate) opened: Option<Delivery>,
    pub(crate) replay: VecDeque<Arc<Event>>,
    pub(crate) queue: mpsc::Receiver<Arc<Event>>,
    pub(crate) closed: watch::Receiver<Option<Closed>>,
    /// The last position the stream sent, or skips through: what the replay covered, or what the
    /// client already received on a replica whose hub was ahead.
    pub(crate) sent: i64,
    pub(crate) ended: bool,
    /// Held for its drop, which removes the stream from the hub.
    pub(crate) _registration: Registration,
}

impl Subscription {
    /// What the stream sends next, or `None` once it has sent why it closed.
    pub async fn next(&mut self) -> Option<Delivery> {
        if self.ended {
            return None;
        }
        if let Some(opened) = self.opened.take() {
            return Some(opened);
        }
        if let Some(event) = self.replay.pop_front() {
            return Some(Delivery::Event(event));
        }
        loop {
            match self.queue.recv().await {
                Some(event) if event.position <= self.sent => {}
                Some(event) => {
                    self.sent = event.position;
                    return Some(Delivery::Event(event));
                }
                None => {
                    self.ended = true;
                    // The hub names the reason before it lets the queue go; a hub that stopped
                    // without naming one stopped with the process.
                    let reason = self.closed.borrow().unwrap_or(Closed::Shutdown);
                    return Some(Delivery::Closed(reason));
                }
            }
        }
    }
}

/// A stream's place in the hub, which dropping removes.
#[derive(Debug)]
pub(crate) struct Registration {
    pub(crate) id: StreamId,
    pub(crate) commands: mpsc::UnboundedSender<Command>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        // The hub's task holds its receiver while any registration holds a sender, so a send
        // fails only once the task has ended in a panic, which leaves no stream to remove.
        if let Err(error) = self.commands.send(Command::Unregister { id: self.id }) {
            errors::ignore(&error, "hub_stopped");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Audience, Event};
    use crate::types::UserId;

    /// An event to `publisher`'s own streams, which `publisher` published, if any, under
    /// `traceparent`, if any.
    fn event(publisher: Option<UserId>, traceparent: Option<&str>) -> Event {
        Event {
            id: uuid::Uuid::nil(),
            name: "note.made".to_owned(),
            payload: serde_json::json!({}),
            trace_id: traceparent
                .and_then(|traceparent| traceparent.split('-').nth(1))
                .map(str::to_owned),
            traceparent: traceparent.map(str::to_owned),
            publisher,
            position: 1,
            audience: Audience::User(UserId::from_uuid(uuid::Uuid::from_u128(1))),
            committed: None,
        }
    }

    #[test]
    fn platform_095_an_event_gives_its_trace_id_to_its_publisher_and_to_no_one_else() {
        let ada = UserId::from_uuid(uuid::Uuid::from_u128(1));
        let grace = UserId::from_uuid(uuid::Uuid::from_u128(2));
        let traceparent = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01";
        let trace_id = Some("0af7651916cd43dd8448eb211c80319c");

        let hers = event(Some(ada), Some(traceparent));
        let no_one_s = event(None, Some(traceparent));
        let untraced = event(Some(ada), None);

        assert_eq!(hers.trace_id_for(ada), trace_id);
        assert_eq!(hers.trace_id_for(grace), None);
        // The event's audience, the user it goes to, doesn't make her its publisher.
        assert_eq!(no_one_s.trace_id_for(ada), None);
        assert_eq!(untraced.trace_id_for(ada), None);
    }
}
