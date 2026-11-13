//! The hub's streams, kept by user and by workspace, which it delivers each event to and closes
//! (SEC-06, SEC-19).

#[cfg(feature = "test-support")]
use std::io;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use tokio::sync::{
    mpsc::{self, error::TrySendError},
    watch,
};

use super::{
    AccessChange, Signal,
    hub::{Command, Registered, StreamId, SubscribeError},
    subscription::{Audience, Closed, Event},
};
#[cfg(feature = "test-support")]
use crate::errors;
use crate::{
    authn::Principal,
    authz::{Memberships, Snapshot},
    telemetry::{
        metrics::{Counter, Gauge},
        names,
    },
    types::{UserId, WorkspaceId},
};

/// The most streams one user holds open on a replica (REALTIME-006).
const STREAMS_PER_USER: usize = 10;

/// The most frames that wait to be sent to one stream before the hub closes it (PLATFORM-032).
const QUEUE: usize = 256;

static STREAMS: Gauge<1> = Gauge::new(
    "platform.streams",
    "{stream}",
    "The streams open on the replica, by the name of the database whose events its hub \
     delivers (PLATFORM-034).",
    [names::DB_NAMESPACE],
);

static CLOSES: Counter<2> = Counter::new(
    "platform.stream_closes",
    "{stream}",
    "Streams the hub closed, by why: session_ended, access_changed, lagging, shutdown or \
     interrupted, and by the name of the database whose events it delivers (PLATFORM-034).",
    [names::STREAM_CLOSE_REASON, names::DB_NAMESPACE],
);

/// A stream the hub holds.
struct Entry {
    principal: Principal,
    /// The caller's memberships, with the snapshot they were read after, once the subscription
    /// has read them; until then, only their user events reach the stream, which its replay
    /// covers.
    memberships: Option<Read>,
    /// The transactions of the membership changes signalled while the subscription read the
    /// memberships, each of which they must hold, or the stream closes once they arrive.
    unread: Vec<Option<u64>>,
    queue: mpsc::Sender<Arc<Event>>,
    /// Why the hub closed the stream, once it has.
    closed: watch::Sender<Option<Closed>>,
}

/// A stream's memberships, as its subscription read them, after taking `snapshot`: they hold the
/// change of each transaction it holds, and may hold others (PLATFORM-031).
struct Read {
    memberships: Memberships,
    snapshot: Snapshot,
}

impl Read {
    /// Whether the memberships hold the membership change that the transaction `id` made: never
    /// for a signal that names none, as the release before sends (DATA-10).
    fn holds(&self, id: Option<u64>) -> bool {
        id.is_some_and(|id| self.snapshot.includes(id))
    }
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

    /// Closes every stream with `shutdown`, and opens none from then on (PLATFORM-062).
    pub(super) fn stop(&mut self) {
        self.stopped = true;
        let ids: Vec<StreamId> = self.streams.keys().copied().collect();
        for id in ids {
            self.close(id, Closed::Shutdown);
        }
    }

    /// Closes every stream with `interrupted`, after the listener was lost (PLATFORM-033).
    pub(super) fn interrupt(&mut self) {
        let ids: Vec<StreamId> = self.streams.keys().copied().collect();
        for id in ids {
            self.close(id, Closed::Interrupted);
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
                snapshot,
                reply,
            } => {
                let head = self.activate(id, memberships, snapshot);
                match reply.send(head) {
                    Ok(()) => {}
                    // The subscription stopped waiting: it has gone, and so does its stream.
                    Err(_) => self.remove(id),
                }
            }
            Command::Unregister { id } => self.remove(id),
            #[cfg(feature = "test-support")]
            Command::Head { reply } => {
                if reply.send(self.head).is_err() {
                    // The test that asked for the head stopped waiting for it.
                    errors::ignore(
                        &io::Error::other("the head's reader has gone"),
                        "head_reader_gone",
                    );
                }
            }
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
                unread: Vec::new(),
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

    /// Keeps the stream `id` under `memberships`, read after `snapshot`, and gives the head, or
    /// `None` when the stream has closed since it registered, or closes now because a membership
    /// change signalled meanwhile isn't among those its memberships hold (PLATFORM-031).
    fn activate(
        &mut self,
        id: StreamId,
        memberships: Memberships,
        snapshot: Snapshot,
    ) -> Option<i64> {
        let read = Read {
            memberships,
            snapshot,
        };
        let entry = self.streams.get_mut(&id)?;
        if !entry
            .unread
            .iter()
            .all(|transaction| read.holds(*transaction))
        {
            self.close(id, Closed::AccessChanged);
            return None;
        }
        for workspace in read.memberships.workspaces() {
            self.by_workspace.entry(workspace).or_default().insert(id);
        }
        entry.memberships = Some(read);
        Some(self.head)
    }

    /// Closes the streams `signal` affects: a session's, or each of a user's whose memberships
    /// don't hold the membership change, and records the change for each that is still reading
    /// its memberships (PLATFORM-031).
    pub(super) fn changed(&mut self, signal: Signal) {
        let Some(ids) = self.by_user.get(&signal.change.user_id()) else {
            return;
        };
        let mut affected = Vec::new();
        for id in ids {
            let Some(entry) = self.streams.get_mut(id) else {
                continue;
            };
            let closes = match signal.change {
                AccessChange::SessionEnded { session_id, .. } => {
                    entry.principal.session_id() == session_id
                }
                AccessChange::MembershipChanged { .. } => match &entry.memberships {
                    Some(read) => !read.holds(signal.transaction),
                    None => {
                        entry.unread.push(signal.transaction);
                        false
                    }
                },
            };
            if closes {
                affected.push(*id);
            }
        }
        let reason = match signal.change {
            AccessChange::SessionEnded { .. } => Closed::SessionEnded,
            AccessChange::MembershipChanged { .. } => Closed::AccessChanged,
        };
        for id in affected {
            self.close(id, reason);
        }
    }

    /// Queues `event` for each stream allowed to receive it (SEC-19), and closes each stream whose
    /// queue is full (PLATFORM-032).
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
                                entry.memberships.as_ref().is_some_and(|read| {
                                    // An event that holds no snapshot reaches no member.
                                    event.committed.as_ref().is_some_and(|committed| {
                                        read.memberships.admits(*workspace, permission, committed)
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
                    .as_ref()
                    .map(|read| (*id, entry.principal.clone(), read.memberships.clone()))
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
        if let Some(read) = &entry.memberships {
            for workspace in read.memberships.workspaces() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::SessionId;

    /// A stream of `user`'s, registered on `streams`.
    #[expect(
        clippy::disallowed_methods,
        reason = "the hub keeps the principal that auth proved; the test stands in for auth"
    )]
    fn register(streams: &mut Streams, user: UserId) -> Registered {
        let session = SessionId::from_uuid(uuid::Uuid::new_v4());
        streams
            .register(Principal::new(user, session))
            .expect("the stream registers")
    }

    /// `snapshot` in its text form, as `pg_current_snapshot()` gives it.
    fn snapshot(text: &str) -> Snapshot {
        Snapshot::parse(text).expect("a snapshot")
    }

    /// The signal that `user`'s membership changed, in the transaction `transaction`.
    fn membership_changed(user: UserId, transaction: Option<u64>) -> Signal {
        Signal {
            change: AccessChange::MembershipChanged {
                user_id: user,
                workspace_id: WorkspaceId::from_uuid(uuid::Uuid::new_v4()),
            },
            transaction,
        }
    }

    fn closed(registered: &Registered) -> Option<Closed> {
        *registered.closed.borrow()
    }

    #[test]
    fn platform_031_a_membership_change_closes_only_the_streams_whose_memberships_don_t_hold_it() {
        let mut streams = Streams::new("api".to_owned(), 0);
        let ada = UserId::from_uuid(uuid::Uuid::new_v4());
        let before = register(&mut streams, ada);
        let after = register(&mut streams, ada);
        // Read before the transaction 150 committed, and after.
        streams.activate(before.id, Memberships::default(), snapshot("150:150:"));
        streams.activate(after.id, Memberships::default(), snapshot("140:160:145"));

        streams.changed(membership_changed(ada, Some(150)));

        assert_eq!(closed(&before), Some(Closed::AccessChanged));
        assert_eq!(closed(&after), None);
    }

    #[test]
    fn platform_031_a_change_signalled_while_a_stream_opens_closes_it_unless_its_memberships_hold_it()
     {
        let mut streams = Streams::new("api".to_owned(), 0);
        let ada = UserId::from_uuid(uuid::Uuid::new_v4());
        let before = register(&mut streams, ada);
        let after = register(&mut streams, ada);

        streams.changed(membership_changed(ada, Some(150)));
        let held = streams.activate(before.id, Memberships::default(), snapshot("140:160:150"));
        let holding = streams.activate(after.id, Memberships::default(), snapshot("151:151:"));

        assert_eq!(held, None);
        assert_eq!(closed(&before), Some(Closed::AccessChanged));
        assert_eq!(holding, Some(0));
        assert_eq!(closed(&after), None);
    }

    #[test]
    fn data_10_a_membership_signal_of_the_release_before_closes_every_stream_of_its_user() {
        let mut streams = Streams::new("api".to_owned(), 0);
        let ada = UserId::from_uuid(uuid::Uuid::new_v4());
        let open = register(&mut streams, ada);
        let opening = register(&mut streams, ada);
        streams.activate(open.id, Memberships::default(), snapshot("151:151:"));
        // The release before sends the change alone, with no transaction.
        let signal: Signal = serde_json::from_value(serde_json::json!({
            "kind": "membership_changed",
            "user_id": ada,
            "workspace_id": WorkspaceId::from_uuid(uuid::Uuid::new_v4()),
        }))
        .expect("the release before's signal parses");

        streams.changed(signal);
        let activated = streams.activate(opening.id, Memberships::default(), snapshot("151:151:"));

        assert_eq!(signal.transaction, None);
        assert_eq!(closed(&open), Some(Closed::AccessChanged));
        assert_eq!(activated, None);
        assert_eq!(closed(&opening), Some(Closed::AccessChanged));
    }
}
