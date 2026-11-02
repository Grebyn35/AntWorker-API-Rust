//! The real-time hub: one task per replica, which owns the instance's own connection, listens
//! there for what commits, loads each committed event once and delivers it to the streams allowed
//! to receive it, closes the streams that an access change affects, and re-checks every stream at
//! an interval (the platform spec's "Events and the real-time hub").

use std::{collections::VecDeque, sync::Arc, time::Duration};

use tokio::sync::{mpsc, oneshot};

use super::{
    AccessChange, CHANNEL, EVENTS_CHANNEL, log,
    streams::Streams,
    subscription::{Closed, Delivery, Registration, Subscription},
};
use crate::{
    authn::{Principal, SessionVerifier},
    authz::{AccessPolicy, Memberships},
    clock::SharedClock,
    db::{Checked, Database, Listener, Received, UnitOfWork},
    errors::{self, Internal},
    lifecycle::Readiness,
    telemetry::{self, metrics::Histogram, names},
    types::UserId,
};

/// How often the hub checks its connection, for readiness (PLATFORM-037).
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// The channels the hub listens on.
const CHANNELS: &[&str] = &[EVENTS_CHANNEL, CHANNEL];

/// The most streams and users one query of the re-check reads.
const RECHECK_BATCH: usize = 1_000;

static DELIVERY_LAG: Histogram<1> = Histogram::new(
    "platform.event_delivery_lag",
    "s",
    "How long after its unit of work committed each event was loaded by a replica's hub, which \
     delivers it to its streams at once, by the name of the database whose events it delivers \
     (PLATFORM-074).",
    [names::DB_NAMESPACE],
    &[
        0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0,
    ],
);

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
    pub(crate) closed: oneshot::Receiver<Closed>,
}

/// Why `Hub::subscribe` opened no stream.
#[derive(Debug, thiserror::Error)]
pub enum SubscribeError {
    /// The caller holds the most open streams a user may on this replica (REALTIME-006).
    #[error("the caller holds the most open streams a user may")]
    TooManyStreams,
    /// The hub can't deliver yet: it hasn't connected to the database since the instance started,
    /// or the instance is shutting down.
    #[error("the hub can't deliver events")]
    Unavailable,
    /// A read of the subscription failed.
    #[error(transparent)]
    Internal(#[from] Internal),
}

/// The handle of the replica's hub, which `subscribe` opens streams through. Clones share the
/// hub.
#[derive(Clone)]
pub struct Hub {
    commands: mpsc::UnboundedSender<Command>,
    database: Database,
    policy: Arc<dyn AccessPolicy>,
}

/// What the hub's task holds besides its streams.
struct Task {
    database: Database,
    policy: Arc<dyn AccessPolicy>,
    verifier: Arc<dyn SessionVerifier>,
    readiness: Readiness,
    clock: SharedClock,
    listener: Listener,
    streams: Streams,
    reachable: bool,
}

impl Hub {
    /// Starts the hub on `database`'s own connection, which it checks every 5 seconds and
    /// records in `readiness`, filtering events through `policy` and re-checking every stream
    /// through `verifier` and `policy` every `recheck`. It runs until the instance stops.
    pub fn start(
        database: &Database,
        policy: Arc<dyn AccessPolicy>,
        verifier: Arc<dyn SessionVerifier>,
        readiness: Readiness,
        clock: SharedClock,
        recheck: Duration,
    ) -> Self {
        let (commands, received) = mpsc::unbounded_channel();
        let task = Task {
            database: database.clone(),
            policy: policy.clone(),
            verifier,
            readiness,
            clock,
            listener: database.listener(CHANNELS),
            streams: Streams::new(database.name()),
            reachable: true,
        };
        tokio::spawn(task.run(received, recheck));
        Self {
            commands,
            database: database.clone(),
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
        // memberships, read as they stand once it is.
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

impl Task {
    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>, recheck: Duration) {
        let clock = self.clock.clone();
        let readiness = self.readiness.clone();
        self.check().await;
        let mut check = clock.sleep(CHECK_INTERVAL);
        let mut rechecked = clock.sleep(recheck);
        let mut stopping = std::pin::pin!(readiness.stopping());
        loop {
            tokio::select! {
                biased;
                command = commands.recv() => match command {
                    Some(command) => self.streams.command(command),
                    None => return,
                },
                () = &mut stopping, if !self.streams.stopped() => self.streams.stop(),
                () = &mut check => {
                    self.check().await;
                    check = clock.sleep(CHECK_INTERVAL);
                }
                received = self.listener.recv() => self.received(received).await,
                () = &mut rechecked => {
                    self.recheck().await;
                    rechecked = clock.sleep(recheck);
                }
            }
        }
    }

    /// Checks the connection, records the result in readiness, and on a new connection catches
    /// up with what committed without one (PLATFORM-073).
    async fn check(&mut self) {
        let checked = self.listener.check(self.clock.as_ref()).await;
        let failure = match &checked {
            Checked::Failed(failure) => Some(failure.clone()),
            Checked::Answered => None,
            Checked::Connected => self
                .caught_up()
                .await
                .err()
                .map(|error| errors::chain(&error)),
        };
        self.reached(failure);
    }

    /// Records whether the hub can hear what commits, in readiness and its log, which readiness
    /// changes before the line is written.
    fn reached(&mut self, failure: Option<String>) {
        self.readiness.set_database(failure.is_none());
        match (&failure, self.reachable) {
            (Some(failure), true) => tracing::warn!(
                { names::EXCEPTION_MESSAGE } = failure.as_str(),
                "the database stopped answering"
            ),
            (None, false) => tracing::info!("the database answers again"),
            _ => {}
        }
        self.reachable = failure.is_none();
    }

    /// After a new connection: the first sets the head; a later one re-checks every stream, then
    /// delivers what committed while no connection listened.
    async fn caught_up(&mut self) -> Result<(), Internal> {
        let mut uow = self.database.unattributed().await?;
        let latest = log::head(&mut uow).await?;
        match self.streams.head() {
            None => self.streams.set_head(latest),
            Some(_) => {
                drop(uow);
                self.recheck().await;
                self.load(latest).await?;
            }
        }
        Ok(())
    }

    async fn received(&mut self, received: Received) {
        match received {
            Received::Notification { channel, payload } if channel == EVENTS_CHANNEL => {
                let Ok(through) = payload.parse::<i64>() else {
                    tracing::warn!("a notification of events named no position");
                    return;
                };
                if let Err(error) = self.load(through).await {
                    // The events stay in the log: the next notification, or the next connection,
                    // loads them.
                    tracing::warn!(
                        { names::EXCEPTION_MESSAGE } = errors::chain(&error),
                        "the hub couldn't load events"
                    );
                }
            }
            Received::Notification { payload, .. } => {
                match serde_json::from_str::<AccessChange>(&payload) {
                    Ok(change) => self.streams.changed(change),
                    Err(_) => tracing::warn!("an access-change signal didn't parse"),
                }
            }
            Received::Lost(failure) => {
                self.reached(Some(failure));
                self.check().await;
            }
        }
    }

    /// Loads the events after the head, up to `through`, and delivers each to its streams, in
    /// the order of their positions.
    async fn load(&mut self, through: i64) -> Result<(), Internal> {
        while let Some(after) = self.streams.head().filter(|head| *head < through) {
            let mut uow = self.database.unattributed().await?;
            let events = log::load(&mut uow, after, through).await?;
            drop(uow);
            let Some(last) = events.last().map(|(event, _)| event.position) else {
                // Positions up to `through` that no event holds were given to units of work that
                // rolled back.
                self.streams.set_head(through);
                return Ok(());
            };
            for (event, lag) in events {
                let span =
                    tracing::info_span!("deliver event", { names::OTEL_NAME } = "deliver event");
                if let Some(traceparent) = &event.traceparent {
                    telemetry::link(&span, traceparent);
                }
                DELIVERY_LAG.observe(lag.max(0.0), [self.streams.database().into()]);
                span.in_scope(|| self.streams.deliver(&event));
            }
            self.streams.set_head(last);
        }
        Ok(())
    }

    /// Closes the streams whose session stopped lasting, or whose caller's memberships changed
    /// (PLATFORM-017).
    async fn recheck(&mut self) {
        let open = self.streams.open();
        for batch in open.chunks(RECHECK_BATCH) {
            if let Err(error) = self.recheck_batch(batch).await {
                tracing::warn!(
                    { names::EXCEPTION_MESSAGE } = errors::chain(&error),
                    "the hub couldn't re-check its streams"
                );
                return;
            }
        }
    }

    async fn recheck_batch(
        &mut self,
        batch: &[(StreamId, Principal, Memberships)],
    ) -> Result<(), Internal> {
        let principals: Vec<Principal> = batch
            .iter()
            .map(|(_, principal, _)| principal.clone())
            .collect();
        let mut users: Vec<UserId> = principals.iter().map(Principal::user_id).collect();
        users.sort_unstable();
        users.dedup();
        let mut uow = self.database.unattributed().await?;
        let lasting = self.verifier.lasting(&mut uow, &principals).await?;
        let current = self.policy.memberships(&mut uow, &users).await?;
        drop(uow);
        for (id, principal, memberships) in batch {
            let now = current
                .get(&principal.user_id())
                .cloned()
                .unwrap_or_default();
            if !lasting.contains(&principal.session_id()) {
                self.streams.close(*id, Closed::SessionEnded);
            } else if now != *memberships {
                self.streams.close(*id, Closed::AccessChanged);
            }
        }
        Ok(())
    }
}
