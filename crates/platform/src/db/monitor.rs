//! The instance's own database connection, outside the pool, whose checks the readiness probe
//! reports (PLATFORM-037). A pool with every connection in use doesn't delay a check, so load
//! alone never takes the instance out of rotation.

use std::time::Duration;

use sqlx::{Connection, PgConnection, postgres::PgConnectOptions};

use crate::{
    clock::{Clock, SharedClock},
    errors,
    lifecycle::Readiness,
    telemetry::names,
};

/// How often the connection is checked.
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// How long a check waits for the database before it counts as failed.
const CHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// Checks the database every interval, for as long as the runtime runs, and records each result
/// in `readiness`.
pub(super) async fn watch(options: PgConnectOptions, readiness: Readiness, clock: SharedClock) {
    let mut connection = None;
    let mut reachable = true;
    loop {
        let checked = check(&mut connection, &options, clock.as_ref()).await;
        match (&checked, reachable) {
            (Err(failure), true) => tracing::warn!(
                { names::EXCEPTION_MESSAGE } = failure.as_str(),
                "the database stopped answering"
            ),
            (Ok(()), false) => tracing::info!("the database answers again"),
            _ => {}
        }
        reachable = checked.is_ok();
        readiness.set_database(reachable);
        clock.sleep(CHECK_INTERVAL).await;
    }
}

/// Pings the connection, and on failure replaces it with a new one, so a database that
/// restarted counts as reachable at once.
async fn check(
    connection: &mut Option<PgConnection>,
    options: &PgConnectOptions,
    clock: &dyn Clock,
) -> Result<(), String> {
    if let Some(open) = connection.as_mut()
        && within(clock, open.ping())
            .await
            .is_ok_and(|pinged| pinged.is_ok())
    {
        return Ok(());
    }
    // A connection that failed its check is dropped rather than closed, since closing it would
    // wait on the same database.
    *connection = None;
    #[expect(
        clippy::disallowed_methods,
        reason = "platform::db opens the readiness connection (DATA-21)"
    )]
    let connecting = PgConnection::connect_with(options);
    match within(clock, connecting).await {
        Ok(Ok(open)) => {
            *connection = Some(open);
            Ok(())
        }
        Ok(Err(error)) => Err(errors::chain(&error)),
        Err(TimedOut) => Err("no answer within the check's timeout".to_owned()),
    }
}

struct TimedOut;

async fn within<T>(clock: &dyn Clock, future: impl Future<Output = T>) -> Result<T, TimedOut> {
    tokio::select! {
        value = future => Ok(value),
        () = clock.sleep(CHECK_TIMEOUT) => Err(TimedOut),
    }
}
