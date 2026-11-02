//! A caller's stream, as the hub subscribed it: its replay, then what the hub delivers to it, then
//! the reason the hub closed it (REALTIME-004, REALTIME-005).

use std::{collections::VecDeque, sync::Arc};

use tokio::sync::{mpsc, oneshot};

use super::hub::{Command, StreamId};
use crate::types::{UserId, WorkspaceId};

/// Why the hub closed a stream, which `stream.closed` names (REALTIME-005).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Closed {
    /// The caller's session ended, or stopped lasting.
    SessionEnded,
    /// The caller's memberships changed.
    AccessChanged,
    /// The client read too slowly to keep up (PLATFORM-072).
    Lagging,
    /// The instance is shutting down (PLATFORM-038).
    Shutdown,
}

impl Closed {
    /// The reason, as `stream.closed` names it.
    pub fn reason(self) -> &'static str {
        match self {
            Self::SessionEnded => "session_ended",
            Self::AccessChanged => "access_changed",
            Self::Lagging => "lagging",
            Self::Shutdown => "shutdown",
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
    pub(crate) position: i64,
    pub(crate) audience: Audience,
}

impl Event {
    /// The event's ID, which a client resumes from with `Last-Event-ID`.
    pub fn id(&self) -> uuid::Uuid {
        self.id
    }

    /// The event's type, such as `member.removed` (API-19).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The event's payload, of IDs (API-19).
    pub fn payload(&self) -> &serde_json::Value {
        &self.payload
    }

    /// The trace ID of the request or job that published the event, if it published inside a
    /// trace (D10).
    pub fn trace_id(&self) -> Option<&str> {
        self.trace_id.as_deref()
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
/// the order their units of work committed (PLATFORM-071). Dropping it removes the stream from
/// the hub.
#[derive(Debug)]
pub struct Subscription {
    pub(crate) opened: Option<Delivery>,
    pub(crate) replay: VecDeque<Arc<Event>>,
    pub(crate) queue: mpsc::Receiver<Arc<Event>>,
    pub(crate) closed: oneshot::Receiver<Closed>,
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
                    let reason = self.closed.try_recv().unwrap_or(Closed::Shutdown);
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
        // A hub that has stopped holds no stream to remove.
        self.commands
            .send(Command::Unregister { id: self.id })
            .unwrap_or_default();
    }
}
