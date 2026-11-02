//! The instance's own database connection, outside the pool, on which the hub listens for what
//! commits, and whose checks the readiness probe reports (PLATFORM-037, PLATFORM-073). A pool
//! with every connection in use doesn't delay a check, so load alone never takes the instance out
//! of rotation.

use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgListener, PgPoolOptions};

use super::Database;
use crate::{
    clock::Clock,
    errors,
    telemetry::{metrics::Gauge, names},
};

/// How long a check waits for the database before it counts as failed.
const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

static DATABASE_REACHABLE: Gauge<1> = Gauge::new(
    "platform.database_reachable",
    "",
    "1 while the instance's own database connection answered its last check, 0 otherwise, by \
     the database's name.",
    [names::DB_NAMESPACE],
);

/// The instance's own connection, which listens on a fixed set of channels. It holds no
/// connection until a check opens one, and drops one that fails, so the next check opens it
/// again and listens again.
pub(crate) struct Listener {
    options: PgConnectOptions,
    channels: &'static [&'static str],
    database: String,
    connection: Option<PgListener>,
}

/// What a check of the connection found.
#[derive(Debug)]
pub(crate) enum Checked {
    /// The connection that was open answered.
    Answered,
    /// A new connection opened and listens, so notifications sent without one were lost.
    Connected,
    /// The database didn't answer, for this reason.
    Failed(String),
}

/// A notification on one of the listened channels, or the loss of the connection.
#[derive(Debug)]
pub(crate) enum Received {
    /// A notification, with its channel and payload.
    Notification {
        /// The channel it was sent on.
        channel: String,
        /// Its payload.
        payload: String,
    },
    /// The connection broke, and notifications are lost until a check opens it again.
    Lost(String),
}

impl Database {
    /// The name of the database, which the hub's metrics carry as `db.namespace`.
    pub(crate) fn name(&self) -> String {
        self.options.get_database().unwrap_or_default().to_owned()
    }

    /// The instance's own connection, which listens on `channels` once a check opens it
    /// (OPS-21: the one connection beside the pool).
    pub(crate) fn listener(&self, channels: &'static [&'static str]) -> Listener {
        Listener {
            database: self.name(),
            options: self.options.clone(),
            channels,
            connection: None,
        }
    }
}

impl Listener {
    /// Checks the connection, or opens it and listens when it has none, within the check's
    /// timeout, and records the result in `platform.database_reachable` (PLATFORM-043).
    pub(crate) async fn check(&mut self, clock: &dyn Clock) -> Checked {
        let checked = self.answer(clock).await;
        let reachable = !matches!(checked, Checked::Failed(_));
        DATABASE_REACHABLE.set(u64::from(reachable), [self.database.clone().into()]);
        checked
    }

    async fn answer(&mut self, clock: &dyn Clock) -> Checked {
        if let Some(connection) = self.connection.as_mut() {
            let answered = within(
                clock,
                sqlx::query_scalar!(r#"SELECT 1 AS "one!""#).fetch_one(connection),
            )
            .await;
            if matches!(answered, Ok(Ok(_))) {
                return Checked::Answered;
            }
            // A connection that failed its check is dropped rather than closed, since closing it
            // would wait on the same database.
            self.connection = None;
        }
        match within(clock, self.connect()).await {
            Ok(Ok(connection)) => {
                self.connection = Some(connection);
                Checked::Connected
            }
            Ok(Err(error)) => Checked::Failed(errors::chain(&error)),
            Err(TimedOut) => Checked::Failed("no answer within the check's timeout".to_owned()),
        }
    }

    /// A connection of its own, outside the pool, that listens on the channels. The pool of one
    /// connection is the listener's own, which sqlx needs to open it; it never reconnects by
    /// itself, so a lost connection shows at once.
    async fn connect(&self) -> Result<PgListener, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .min_connections(0)
            .max_lifetime(None)
            .idle_timeout(None)
            .connect_lazy_with(self.options.clone());
        let mut connection = PgListener::connect_with(&pool).await?;
        connection.ignore_pool_close_event(true);
        connection.eager_reconnect(false);
        connection.listen_all(self.channels.iter().copied()).await?;
        Ok(connection)
    }

    /// The next notification, or the loss of the connection. Without a connection, it waits
    /// until the caller stops waiting, as for a check that opens one. It can be cancelled
    /// between notifications without losing one.
    pub(crate) async fn recv(&mut self) -> Received {
        let Some(connection) = self.connection.as_mut() else {
            return std::future::pending().await;
        };
        let lost = match connection.try_recv().await {
            Ok(Some(notification)) => {
                return Received::Notification {
                    channel: notification.channel().to_owned(),
                    payload: notification.payload().to_owned(),
                };
            }
            Ok(None) => "the connection closed".to_owned(),
            Err(error) => errors::chain(&error),
        };
        self.connection = None;
        DATABASE_REACHABLE.set(0, [self.database.clone().into()]);
        Received::Lost(lost)
    }
}

struct TimedOut;

async fn within<T>(clock: &dyn Clock, future: impl Future<Output = T>) -> Result<T, TimedOut> {
    tokio::select! {
        value = future => Ok(value),
        () = clock.sleep(CHECK_TIMEOUT) => Err(TimedOut),
    }
}
