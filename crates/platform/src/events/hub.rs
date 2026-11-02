//! The real-time hub's handle: the instance's own connection, which `main.rs` opens, and the
//! subscription of a caller's stream, which the hub's task (`task`) then serves (the platform
//! spec's "Events and the real-time hub").

use std::{collections::VecDeque, sync::Arc, time::Duration};

use tokio::sync::{mpsc, oneshot, watch};

use super::{
    CHANNEL, EVENTS_CHANNEL, log,
    subscription::{Closed, Delivery, Registration, Subscription},
    task::Task,
};
use crate::{
    authn::{Principal, SessionVerifier},
    authz::{AccessPolicy, Memberships},
    clock::{Clock, SharedClock},
    db::{Checked, Database, Listener, UnitOfWork},
    errors::{self, Internal},
    lifecycle::Readiness,
    types::UserId,
};

/// The channels the hub listens on.
const CHANNELS: &[&str] = &[EVENTS_CHANNEL, CHANNEL];

/// A stream's number in its hub.
pub(crate) type StreamId = u64;

/// What the hub's task is asked to do.
#[derive(Debug)]
pub(crate) enum Command {
    /// Registers a stream under its user, unless the user holds the most streams already.
    Register {
        principal: Principal,
        reply: oneshot::Sender<Result<Registered, SubscribeError>>,
    },
    /// Keeps a registered stream under its memberships, and answers with the hub's head, or
    /// `None` when the stream has closed since it registered.
    Activate {
        id: StreamId,
        memberships: Memberships,
        reply: oneshot::Sender<Option<i64>>,
    },
    /// Removes a stream whose subscription was dropped.
    Unregister { id: StreamId },
}

/// A registered stream, as the hub's task hands it to `subscribe`.
#[derive(Debug)]
pub(crate) struct Registered {
    pub(crate) id: StreamId,
    pub(crate) queue: mpsc::Receiver<Arc<super::Event>>,
    pub(crate) closed: watch::Receiver<Option<Closed>>,
}

/// Why `Hub::subscribe` opened no stream.
#[derive(Debug, thiserror::Error)]
pub enum SubscribeError {
    /// The caller holds the most open streams a user may on this replica (REALTIME-006).
    #[error("the caller holds the most open streams a user may")]
    TooManyStreams,
    /// The instance is shutting down, so its hub opens no stream.
    #[error("the hub can't deliver events")]
    Unavailable,
    /// A read of the subscription failed.
    #[error(transparent)]
    Internal(#[from] Internal),
}

/// The instance's own connection to the database, listening, which `main.rs` opens as a step of
/// startup (ARCH-12) and the hub then runs on.
pub struct Connection {
    pub(super) database: Database,
    pub(super) listener: Listener,
    /// The last position given when it began to listen, from which the hub loads.
    pub(super) head: i64,
}

/// The instance's own connection couldn't listen.
#[derive(Debug, thiserror::Error)]
#[error("can't listen on the instance's own database connection: {0}")]
pub struct ListenError(String);

/// Opens the instance's own connection to `database`, outside its pool, and listens there for
/// what commits, within a check's timeout on `clock` (OPS-21); then reads the last position
/// given, so every event that commits from then on is one the hub loads.
pub async fn listen(database: &Database, clock: &dyn Clock) -> Result<Connection, ListenError> {
    let mut listener = database.listener(CHANNELS);
    if let Checked::Failed(failure) = listener.check(clock).await {
        return Err(ListenError(failure));
    }
    let failed = |error: Internal| ListenError(errors::chain(&error));
    let mut uow = database.unattributed().await.map_err(failed)?;
    let head = log::head(&mut uow).await.map_err(failed)?;
    Ok(Connection {
        database: database.clone(),
        listener,
        head,
    })
}

/// The handle of the replica's hub, which `subscribe` opens streams through. Clones share the
/// hub.
#[derive(Clone)]
pub struct Hub {
    commands: mpsc::UnboundedSender<Command>,
    database: Database,
    policy: Arc<dyn AccessPolicy>,
}

impl Hub {
    /// Starts the hub on `connection`, which it checks every 5 seconds and records in
    /// `readiness`, filtering events through `policy` and re-checking every stream through
    /// `verifier` and `policy` every `recheck`. It runs until the instance stops.
    pub fn start(
        connection: Connection,
        policy: Arc<dyn AccessPolicy>,
        verifier: Arc<dyn SessionVerifier>,
        readiness: Readiness,
        clock: SharedClock,
        recheck: Duration,
    ) -> Self {
        let (commands, received) = mpsc::unbounded_channel();
        let database = connection.database.clone();
        let task = Task::new(connection, policy.clone(), verifier, readiness, clock);
        tokio::spawn(task.run(received, recheck));
        Self {
            commands,
            database,
            policy,
        }
    }

    /// Opens `principal`'s stream, which resumes after the event `last_event_id` names, or,
    /// without one, starts with what commits next (PLATFORM-071, REALTIME-004).
    pub async fn subscribe(
        &self,
        principal: &Principal,
        last_event_id: Option<&str>,
    ) -> Result<Subscription, SubscribeError> {
        let (reply, registered) = oneshot::channel();
        self.send(Command::Register {
            principal: principal.clone(),
            reply,
        })?;
        let registered = registered
            .await
            .map_err(|_| SubscribeError::Unavailable)??;
        let registration = Registration {
            id: registered.id,
            commands: self.commands.clone(),
        };
        // Registered first, so a signal for the caller from now on closes the stream; then the
        // memberships, read as they stand once it is. No pooled connection is held while the
        // hub answers, since the hub's own reads may wait for one.
        let mut uow = self.database.unattributed().await?;
        let user = principal.user_id();
        let memberships = self
            .policy
            .memberships(&mut uow, &[user])
            .await?
            .remove(&user)
            .unwrap_or_default();
        let resumed = match last_event_id.and_then(|id| uuid::Uuid::parse_str(id.trim()).ok()) {
            Some(id) => Some(log::position(&mut uow, id).await?),
            None if last_event_id.is_some_and(|id| !id.trim().is_empty()) => Some(None),
            None => None,
        };
        drop(uow);
        let (reply, head) = oneshot::channel();
        self.send(Command::Activate {
            id: registered.id,
            memberships: memberships.clone(),
            reply,
        })?;
        let activated = head.await.map_err(|_| SubscribeError::Unavailable)?;
        let subscription = Subscription {
            opened: Some(Delivery::Opened {
                truncated: false,
                resume: None,
            }),
            replay: VecDeque::new(),
            queue: registered.queue,
            closed: registered.closed,
            // A stream that closed since it registered sends nothing but why.
            sent: activated.unwrap_or(i64::MAX),
            ended: false,
            _registration: registration,
        };
        let mut uow = self.database.unattributed().await?;
        begin(
            &mut uow,
            user,
            &memberships,
            resumed,
            activated,
            subscription,
        )
        .await
    }

    fn send(&self, command: Command) -> Result<(), SubscribeError> {
        self.commands
            .send(command)
            .map_err(|_| SubscribeError::Unavailable)
    }
}

/// Starts `subscription`, whose caller `user` holds `memberships`, from where it `resumed`, the
/// position of the client's `Last-Event-ID`, `None` inside for an ID the API doesn't hold, at
/// the head the hub `activated` it at, or `None` for a stream that closed since it registered
/// (REALTIME-004).
async fn begin(
    uow: &mut UnitOfWork,
    user: UserId,
    memberships: &Memberships,
    resumed: Option<Option<i64>>,
    activated: Option<i64>,
    mut subscription: Subscription,
) -> Result<Subscription, SubscribeError> {
    // A stream that closed since it registered still names where a reconnect resumes it, from
    // the log's head as it stands.
    let head = match activated {
        Some(head) => head,
        None => log::head(uow).await?,
    };
    let truncated = match resumed {
        None => false,
        // A closed stream's own `Last-Event-ID` resumes it.
        Some(Some(_)) if activated.is_none() => return Ok(subscription),
        Some(Some(position)) if position < head => {
            let replay = log::replay(uow, user, memberships, position, head).await?;
            if i64::try_from(replay.len()).unwrap_or(i64::MAX) <= log::REPLAY_LIMIT {
                subscription.replay = replay.into();
                return Ok(subscription);
            }
            true
        }
        // A client that received more than this hub has loaded skips what it has.
        Some(Some(position)) => {
            subscription.sent = position;
            return Ok(subscription);
        }
        Some(None) => true,
    };
    // The stream starts with what commits next, and names where a reconnect resumes it.
    let resume = log::newest_visible(uow, user, memberships, head)
        .await?
        .unwrap_or(uuid::Uuid::nil());
    subscription.opened = Some(Delivery::Opened {
        truncated,
        resume: Some(resume),
    });
    Ok(subscription)
}
