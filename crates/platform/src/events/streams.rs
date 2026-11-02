//! The hub's streams, kept by user and by workspace, which it delivers each event to and closes
//! (SEC-06, SEC-17).

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use tokio::sync::{
    mpsc::{self, error::TrySendError},
    watch,
};

use super::{
    AccessChange,
    hub::{Command, Registered, StreamId, SubscribeError},
    subscription::{Audience, Closed, Event},
};
use crate::{
    authn::Principal,
    authz::Memberships,
    telemetry::{
        metrics::{Counter, Gauge},
        names,
    },
    types::{UserId, WorkspaceId},
};

/// The most streams one user holds open on a replica (REALTIME-006).
const STREAMS_PER_USER: usize = 10;

/// The most frames that wait to be sent to one stream before the hub closes it (PLATFORM-072).
const QUEUE: usize = 256;

static STREAMS: Gauge<1> = Gauge::new(
    "platform.streams",
    "{stream}",
    "The streams open on the replica, by the name of the database whose events its hub \
     delivers (PLATFORM-074).",
    [names::DB_NAMESPACE],
);

static CLOSES: Counter<2> = Counter::new(
    "platform.stream_closes",
    "{stream}",
    "Streams the hub closed, by why: session_ended, access_changed, lagging or shutdown, and by \
     the name of the database whose events it delivers (PLATFORM-074).",
    [names::STREAM_CLOSE_REASON, names::DB_NAMESPACE],
);

/// A stream the hub holds.
struct Entry {
    principal: Principal,
    /// The caller's memberships, once the subscription has read them; until then, only their
    /// user events reach the stream, which its replay covers.
    memberships: Option<Memberships>,
    queue: mpsc::Sender<Arc<Event>>,
    /// Why the hub closed the stream, once it has.
    closed: watch::Sender<Option<Closed>>,
}

/// The streams, and the head: the last position the hub loaded.
pub(super) struct Streams {
    /// The name of the database whose events the hub delivers, which its metrics carry.
    database: String,
    head: i64,
    next: StreamId,
    streams: HashMap<StreamId, Entry>,
    by_user: HashMap<UserId, HashSet<StreamId>>,
    by_workspace: HashMap<WorkspaceId, HashSet<StreamId>>,
    stopped: bool,
}

impl Streams {
    /// No streams, for the hub of the database named `database`, which loads the events after
    /// `head`.
    pub(super) fn new(database: String, head: i64) -> Self {
        Self {
            database,
            head,
            next: 0,
            streams: HashMap::new(),
            by_user: HashMap::new(),
            by_workspace: HashMap::new(),
            stopped: false,
        }
    }

    /// The name of the database whose events the hub delivers.
    pub(super) fn database(&self) -> String {
        self.database.clone()
    }

    /// The last position the hub loaded.
    pub(super) fn head(&self) -> i64 {
        self.head
    }

    pub(super) fn set_head(&mut self, head: i64) {
        self.head = head;
    }

    /// Whether the instance is shutting down, so the streams have closed.
    pub(super) fn stopped(&self) -> bool {
        self.stopped
    }

    /// Closes every stream with `shutdown`, and opens none from then on (PLATFORM-038).
    pub(super) fn stop(&mut self) {
        self.stopped = true;
        let ids: Vec<StreamId> = self.streams.keys().copied().collect();
        for id in ids {
            self.close(id, Closed::Shutdown);
        }
    }

    pub(super) fn command(&mut self, command: Command) {
        match command {
            Command::Register { principal, reply } => {
                let registered = self.register(principal);
                // A subscription that stopped waiting has gone; the stream goes with it.
                if let Err(Ok(registered)) = reply.send(registered) {
                    self.remove(registered.id);
                }
            }
            Command::Activate {
                id,
                memberships,
                reply,
            } => {
                let head = self.activate(id, memberships);
                match reply.send(head) {
                    Ok(()) => {}
                    // The subscription stopped waiting: it has gone, and so does its stream.
                    Err(_) => self.remove(id),
                }
            }
            Command::Unregister { id } => self.remove(id),
        }
    }

    fn register(&mut self, principal: Principal) -> Result<Registered, SubscribeError> {
        let user = principal.user_id();
        if self.stopped {
            return Err(SubscribeError::Unavailable);
        }
        let held = self.by_user.get(&user).map_or(0, HashSet::len);
        if held >= STREAMS_PER_USER {
            return Err(SubscribeError::TooManyStreams);
        }
        let id = self.next;
        self.next += 1;
        let (queue, queued) = mpsc::channel(QUEUE);
        let (closed, closing) = watch::channel(None);
        self.streams.insert(
            id,
            Entry {
                principal,
                memberships: None,
                queue,
                closed,
            },
        );
        self.by_user.entry(user).or_default().insert(id);
        self.counted();
        Ok(Registered {
            id,
            queue: queued,
            closed: closing,
        })
    }

    /// Keeps the stream `id` under `memberships`, and gives the head, or `None` when the stream
    /// has closed since it registered.
    fn activate(&mut self, id: StreamId, memberships: Memberships) -> Option<i64> {
        let entry = self.streams.get_mut(&id)?;
        for workspace in memberships.workspaces() {
            self.by_workspace.entry(workspace).or_default().insert(id);
        }
        entry.memberships = Some(memberships);
        Some(self.head)
    }

    /// Closes the streams `change` affects: a session's, or all of a user's (PLATFORM-017).
    pub(super) fn changed(&mut self, change: AccessChange) {
        let Some(ids) = self.by_user.get(&change.user_id()) else {
            return;
        };
        let affected: Vec<StreamId> = ids
            .iter()
            .copied()
            .filter(|id| match change {
                AccessChange::SessionEnded { session_id, .. } => self
                    .streams
                    .get(id)
                    .is_some_and(|entry| entry.principal.session_id() == session_id),
                AccessChange::MembershipChanged { .. } => true,
            })
            .collect();
        let reason = match change {
            AccessChange::SessionEnded { .. } => Closed::SessionEnded,
            AccessChange::MembershipChanged { .. } => Closed::AccessChanged,
        };
        for id in affected {
            self.close(id, reason);
        }
    }

    /// Queues `event` for each stream allowed to receive it (SEC-17), and closes each stream whose
    /// queue is full (PLATFORM-072).
    pub(super) fn deliver(&mut self, event: &Arc<Event>) {
        let targets: Vec<StreamId> = match &event.audience {
            Audience::User(user) => self
                .by_user
                .get(user)
                .map(|ids| ids.iter().copied().collect())
                .unwrap_or_default(),
            Audience::Workspace(workspace, permission) => self
                .by_workspace
                .get(workspace)
                .map(|ids| {
                    ids.iter()
                        .copied()
                        .filter(|id| {
                            self.streams.get(id).is_some_and(|entry| {
                                entry.memberships.as_ref().is_some_and(|memberships| {
                                    // An event that holds no snapshot reaches no member.
                                    event.committed.as_ref().is_some_and(|committed| {
                                        memberships.admits(*workspace, permission, committed)
                                    })
                                })
                            })
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        for id in targets {
            let Some(entry) = self.streams.get(&id) else {
                continue;
            };
            match entry.queue.try_send(event.clone()) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => self.close(id, Closed::Lagging),
                Err(TrySendError::Closed(_)) => self.remove(id),
            }
        }
    }

    /// Every stream that has its memberships, with its principal and its memberships, for the
    /// re-check.
    pub(super) fn open(&self) -> Vec<(StreamId, Principal, Memberships)> {
        self.streams
            .iter()
            .filter_map(|(id, entry)| {
                entry
                    .memberships
                    .clone()
                    .map(|memberships| (*id, entry.principal.clone(), memberships))
            })
            .collect()
    }

    /// Closes the stream `id`: it sends what waits in its queue, then why it closed.
    pub(super) fn close(&mut self, id: StreamId, reason: Closed) {
        if let Some(entry) = self.take(id) {
            CLOSES.add(1, [reason.reason().into(), self.database.clone().into()]);
            // The reason goes before the queue's sender, so the stream finds it once its queue
            // ends.
            entry.closed.send_replace(Some(reason));
        }
    }

    /// Removes the stream `id`, whose subscription has gone.
    fn remove(&mut self, id: StreamId) {
        drop(self.take(id));
    }

    fn take(&mut self, id: StreamId) -> Option<Entry> {
        let entry = self.streams.remove(&id)?;
        let user = entry.principal.user_id();
        if let Some(ids) = self.by_user.get_mut(&user) {
            ids.remove(&id);
            if ids.is_empty() {
                self.by_user.remove(&user);
            }
        }
        if let Some(memberships) = &entry.memberships {
            for workspace in memberships.workspaces() {
                if let Some(ids) = self.by_workspace.get_mut(&workspace) {
                    ids.remove(&id);
                    if ids.is_empty() {
                        self.by_workspace.remove(&workspace);
                    }
                }
            }
        }
        self.counted();
        Some(entry)
    }

    fn counted(&self) {
        let open = u64::try_from(self.streams.len()).unwrap_or(u64::MAX);
        STREAMS.set(open, [self.database.clone().into()]);
    }
}
