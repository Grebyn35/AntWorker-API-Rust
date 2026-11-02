//! The hub's task: one per replica, which owns the instance's own connection, listens there for
//! what commits, loads each committed event once and delivers it to the streams allowed to
//! receive it, closes the streams that an access change affects, and re-checks every stream at an
//! interval (the platform spec's "Events and the real-time hub").

use std::{sync::Arc, time::Duration};

use tokio::sync::mpsc;

use super::{
    AccessChange, EVENTS_CHANNEL,
    hub::{Command, Connection, StreamId},
    log,
    streams::Streams,
    subscription::Closed,
};
use crate::{
    authn::{Principal, SessionVerifier},
    authz::{AccessPolicy, Memberships},
    clock::SharedClock,
    db::{Checked, Database, Listener, Received},
    errors::{self, Internal},
    lifecycle::Readiness,
    telemetry::{
        self,
        metrics::{Gauge, Histogram},
        names,
    },
    types::UserId,
};

/// How often the hub checks its connection, for readiness (PLATFORM-037), and tries again what
/// it owes (PLATFORM-073).
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

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

static UNLOADED: Gauge<1> = Gauge::new(
    "platform.events_unloaded",
    "{event}",
    "The events a replica's hub owes its streams: the positions after the last event it loaded, up \
     to the last that a notification it handled named, by the name of the database whose events \
     it delivers. It stays above 0 while the hub's loads fail (PLATFORM-076).",
    [names::DB_NAMESPACE],
);

/// What the hub's task holds.
pub(super) struct Task {
    database: Database,
    policy: Arc<dyn AccessPolicy>,
    verifier: Arc<dyn SessionVerifier>,
    readiness: Readiness,
    clock: SharedClock,
    listener: Listener,
    streams: Streams,
    reachable: bool,
    /// The last position that a notification the hub has handled named. A load never passes it,
    /// so no event passes a signal still waiting behind it.
    notified: i64,
    owed: Owed,
}

/// What failed, and the hub tries again at every check (PLATFORM-073).
#[derive(Clone, Copy, Debug, Default)]
struct Owed {
    /// The events up to the last position a notification named.
    load: bool,
    /// A re-check of every stream.
    recheck: bool,
}

impl Task {
    /// The task on `connection`, which filters through `policy` and re-checks through `verifier`
    /// and `policy`, recording in `readiness` whether it reaches the database.
    pub(super) fn new(
        connection: Connection,
        policy: Arc<dyn AccessPolicy>,
        verifier: Arc<dyn SessionVerifier>,
        readiness: Readiness,
        clock: SharedClock,
    ) -> Self {
        let Connection {
            database,
            listener,
            head,
        } = connection;
        Self {
            streams: Streams::new(database.name(), head),
            notified: head,
            database,
            policy,
            verifier,
            readiness,
            clock,
            listener,
            reachable: true,
            owed: Owed::default(),
        }
    }

    pub(super) async fn run(
        mut self,
        mut commands: mpsc::UnboundedReceiver<Command>,
        recheck: Duration,
    ) {
        let clock = self.clock.clone();
        let readiness = self.readiness.clone();
        self.record_unloaded();
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

    /// Checks the connection and records the result in readiness, which follows the connection
    /// alone (PLATFORM-037). On a new connection, the notifications sent without one are lost,
    /// so the hub closes every stream; otherwise it tries again what it owes.
    async fn check(&mut self) {
        match self.listener.check(self.clock.as_ref()).await {
            Checked::Failed(failure) => self.reached(Some(failure)),
            Checked::Connected => {
                self.reached(None);
                self.reconnected().await;
            }
            Checked::Answered => {
                self.reached(None);
                self.settle().await;
            }
        }
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

    /// After a new connection: the signals sent without one are lost, and no one can tell where
    /// they fell among the events, so every stream closes with `interrupted`, and its client's
    /// reconnect replays what committed meanwhile with its access as it then stands
    /// (PLATFORM-073). The head then moves to the log's, since no stream is left to deliver the
    /// events in between to.
    async fn reconnected(&mut self) {
        self.streams.interrupt();
        self.owed = Owed::default();
        match self.log_head().await {
            Ok(latest) if latest > self.streams.head() => {
                self.streams.set_head(latest);
                self.notified = self.notified.max(latest);
                self.record_unloaded();
            }
            Ok(_) => {}
            // The head stays: the next notification's load reaches the events in between, from
            // it, for the streams that open meanwhile.
            Err(error) => tracing::warn!(
                { names::EXCEPTION_MESSAGE } = errors::chain(&error),
                "the hub couldn't read the log's head"
            ),
        }
    }

    async fn log_head(&self) -> Result<i64, Internal> {
        let mut uow = self.database.unattributed().await?;
        log::head(&mut uow).await
    }

    /// Tries again what failed: a re-check, and a load up to the last position a notification
    /// named.
    async fn settle(&mut self) {
        if self.owed.recheck {
            self.recheck().await;
        }
        if self.owed.load {
            self.load_notified().await;
        }
    }

    async fn received(&mut self, received: Received) {
        match received {
            Received::Notification { channel, payload } if channel == EVENTS_CHANNEL => {
                let Ok(through) = payload.parse::<i64>() else {
                    tracing::warn!("a notification of events named no position");
                    return;
                };
                self.notified = self.notified.max(through);
                self.load_notified().await;
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

    /// Loads the events up to the last position a notification named, or owes them, so the
    /// next check loads them; a load from the head covers what a failed one left.
    async fn load_notified(&mut self) {
        match self.load(self.notified).await {
            Ok(()) => self.owed.load = false,
            Err(error) => {
                self.owed.load = true;
                tracing::warn!(
                    { names::EXCEPTION_MESSAGE } = errors::chain(&error),
                    "the hub couldn't load events"
                );
            }
        }
        self.record_unloaded();
    }

    /// Records the positions the hub owes its streams, from its head up to the last position a
    /// notification it handled named (PLATFORM-076).
    fn record_unloaded(&self) {
        let owed = self.notified.saturating_sub(self.streams.head()).max(0);
        UNLOADED.set(
            u64::try_from(owed).unwrap_or_default(),
            [self.streams.database().into()],
        );
    }

    /// Loads the events after the head, up to `through`, and delivers each to its streams, in
    /// the order of their positions.
    async fn load(&mut self, through: i64) -> Result<(), Internal> {
        while self.streams.head() < through {
            let after = self.streams.head();
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
    /// (PLATFORM-017), or owes the re-check, so the next check makes it.
    async fn recheck(&mut self) {
        let open = self.streams.open();
        for batch in open.chunks(RECHECK_BATCH) {
            if let Err(error) = self.recheck_batch(batch).await {
                self.owed.recheck = true;
                tracing::warn!(
                    { names::EXCEPTION_MESSAGE } = errors::chain(&error),
                    "the hub couldn't re-check its streams"
                );
                return;
            }
        }
        self.owed.recheck = false;
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
