//! The database: the connection pool, the unit of work, and the `bootstrap` and `migrate`
//! tasks.
//!
//! The pool and its connections stay inside this module. Code outside it runs queries through
//! a [`UnitOfWork`] (DATA-21). The one exception is the test support, which gets raw
//! connections from `testing`, compiled only with the `test-support` feature.
#![expect(
    clippy::disallowed_types,
    reason = "this module owns the pool, its connections and its transactions (DATA-21)"
)]

mod migrations;
mod monitor;
mod queue;
mod roles;
#[cfg(feature = "test-support")]
pub mod testing;
mod url;

use std::time::Duration;

use sqlx::{
    PgConnection, PgPool, Postgres, Transaction,
    migrate::Migrator,
    postgres::{PgConnectOptions, PgPoolOptions},
};

pub use migrations::{MigrateError, SchemaProblem, Versions, migrate};
pub(crate) use queue::{ClaimedJob, ClaimedSchedule};
pub use roles::{Admin, BootstrapError, RoleProblem, bootstrap};
pub use url::{DatabaseUrl, InvalidDatabaseUrl};

use serde::{Serialize, de::DeserializeOwned};

use crate::{
    clock::SharedClock,
    errors::{Internal, PlatformError, RequestError},
    idempotency::{self, Claimed, IdempotencyKey, Outcome},
    jobs::JobContext,
    lifecycle::Readiness,
    types::{Client, UserId},
};

/// The SQLSTATE of a lock that `lock_timeout` stopped waiting for.
const LOCK_NOT_AVAILABLE: &str = "55P03";

/// How long an operation waits for a free connection before it fails.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// The API's connection pool, as the role `app`. A value exists only once the startup checks
/// have passed, so a server never runs against an unsafe role, or against a schema that lacks
/// one of its migrations or holds an edited one.
#[derive(Clone)]
pub struct Database {
    pool: PgPool,
    options: PgConnectOptions,
}

/// `serve` can't use the database.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The connection failed.
    #[error("can't connect with DATABASE_URL")]
    Connect(#[source] sqlx::Error),
    /// A query of the startup checks failed.
    #[error("can't run the startup checks")]
    Check(#[source] sqlx::Error),
    /// The role isn't safe to serve as (PLATFORM-009).
    #[error("DATABASE_URL connects as a role that can't serve: {}", describe(.0))]
    UnsafeRole(Vec<RoleProblem>),
    /// The schema doesn't match this release (PLATFORM-030).
    #[error(transparent)]
    Schema(#[from] SchemaProblem),
}

fn describe(problems: &[RoleProblem]) -> String {
    let problems: Vec<String> = problems.iter().map(RoleProblem::to_string).collect();
    problems.join("; ")
}

impl Database {
    /// Connects as the role `app`, with up to `max_connections` connections. It refuses a role
    /// with more power than `app` needs (PLATFORM-009), and a schema that lacks a migration in
    /// `migrations` or holds one with different content (PLATFORM-030). It never applies
    /// migrations.
    pub async fn connect(
        url: &DatabaseUrl,
        max_connections: u32,
        migrations: &Migrator,
    ) -> Result<Self, ConnectError> {
        let options = url.connect_options();
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .connect_with(options.clone())
            .await
            .map_err(ConnectError::Connect)?;
        let mut conn = pool.acquire().await.map_err(ConnectError::Connect)?;
        let problems = roles::check_role(&mut conn)
            .await
            .map_err(ConnectError::Check)?;
        if !problems.is_empty() {
            return Err(ConnectError::UnsafeRole(problems));
        }
        migrations::check_schema(&mut conn, migrations).await?;
        Ok(Self { pool, options })
    }

    /// The most connections the instance opens: the pool's, plus the one of its own that
    /// [`Database::watch`] checks the database on (OPS-21).
    pub fn connections(&self) -> u32 {
        self.pool.options().get_max_connections().saturating_add(1)
    }

    /// Checks the database on a connection of its own every five seconds, for as long as the
    /// async runtime runs, and records each result in `readiness` (PLATFORM-037).
    pub fn watch(&self, readiness: Readiness, clock: SharedClock) {
        tokio::spawn(monitor::watch(self.options.clone(), readiness, clock));
    }

    /// Opens a unit of work for a request from `client`, whose audit entries record it
    /// (AUDIT-003).
    pub async fn begin(&self, client: &Client) -> Result<UnitOfWork, Internal> {
        Ok(UnitOfWork {
            tx: self.pool.begin().await?,
            client: Some(client.clone()),
            claimed: None,
        })
    }

    /// Opens a unit of work for a job's run, which only a run's `JobContext` can ask for, as
    /// only a request's `Client` can ask for a request's. Its queries filter by the context's
    /// scope (PLATFORM-012), and its audit entries record no client.
    pub async fn for_job(&self, _context: &JobContext) -> Result<UnitOfWork, Internal> {
        Ok(UnitOfWork {
            tx: self.pool.begin().await?,
            client: None,
            claimed: None,
        })
    }

    /// Adds one request to the rate-limit bucket keyed by `key_hash`, in one statement that
    /// commits at once, and returns the window's count and how long until the window ends. A
    /// window that has ended starts again with this request, all by the database's clock
    /// (PLATFORM-026, PLATFORM-049).
    pub(crate) async fn count(
        &self,
        key_hash: &[u8; 32],
        window: Duration,
    ) -> Result<(u32, Duration), Internal> {
        let row = sqlx::query!(
            r#"INSERT INTO rate_limit_buckets AS bucket (key_hash, count, window_started_at)
            VALUES ($1, 1, now())
            ON CONFLICT (key_hash) DO UPDATE SET
                count = CASE
                    WHEN bucket.window_started_at + make_interval(secs => $2) <= now() THEN 1
                    ELSE LEAST(bucket.count, 2147483646) + 1
                END,
                window_started_at = CASE
                    WHEN bucket.window_started_at + make_interval(secs => $2) <= now() THEN now()
                    ELSE bucket.window_started_at
                END
            RETURNING
                count,
                extract(epoch FROM window_started_at + make_interval(secs => $2) - now())::float8
                    AS "resets_in!""#,
            key_hash.as_slice(),
            window.as_secs_f64(),
        )
        .fetch_one(&self.pool)
        .await?;
        let count = u32::try_from(row.count).unwrap_or(u32::MAX);
        let resets_in = Duration::try_from_secs_f64(row.resets_in.max(0.0)).unwrap_or(window);
        Ok((count, resets_in))
    }
}

/// One database transaction, holding every write of one operation (D1, ARCH-11). Dropping it
/// without [`UnitOfWork::commit`] rolls the transaction back.
pub struct UnitOfWork {
    tx: Transaction<'static, Postgres>,
    client: Option<Client>,
    claimed: Option<(UserId, String)>,
}

impl UnitOfWork {
    /// The client of the request that opened the unit of work, or `None` for a job's.
    pub fn client(&self) -> Option<&Client> {
        self.client.as_ref()
    }

    /// The connection to run this unit of work's queries on, inside its transaction:
    /// `sqlx::query!(...).fetch_one(uow.executor())`.
    pub fn executor(&mut self) -> &mut PgConnection {
        &mut self.tx
    }

    /// Commits every write the unit of work holds. A unit of work that claimed an idempotency
    /// key commits through [`UnitOfWork::commit_with`] instead, so its key keeps its result.
    pub async fn commit(self) -> Result<(), Internal> {
        if self.claimed.is_some() {
            return Err(Internal::new(
                "a unit of work that claimed an idempotency key committed without its result",
            ));
        }
        Ok(self.tx.commit().await?)
    }

    /// Claims `key` for `user`, as the unit of work's first statement, so the key's record
    /// commits or rolls back with the operation (PLATFORM-018). With no key, or no operation
    /// committed under it in the last 24 hours, the operation runs. The result of the one that
    /// committed with the same fingerprint answers instead, and another fingerprint is refused
    /// with 422. A record that another unit of work holds and hasn't committed is waited on for
    /// at most a second, then refused with 409.
    pub async fn claim<T: DeserializeOwned>(
        &mut self,
        user: UserId,
        key: Option<&IdempotencyKey>,
    ) -> Result<Claimed<T>, PlatformError> {
        let Some(key) = key else {
            return Ok(Claimed::Fresh);
        };
        sqlx::query!("SET LOCAL lock_timeout = '1s'")
            .execute(&mut *self.tx)
            .await
            .map_err(Internal::from)?;
        let inserted = sqlx::query_scalar!(
            r#"INSERT INTO idempotency_records AS record (user_id, key, fingerprint)
            VALUES ($1, $2, $3)
            ON CONFLICT ON CONSTRAINT idempotency_records_user_id_key_key DO UPDATE
                SET fingerprint = EXCLUDED.fingerprint, result = NULL, created_at = now()
                WHERE record.created_at <= now() - interval '24 hours'
            RETURNING true AS "inserted!""#,
            user.uuid(),
            key.key(),
            key.fingerprint().as_slice(),
        )
        .fetch_optional(&mut *self.tx)
        .await;
        let inserted = match inserted {
            Ok(row) => row.is_some(),
            Err(error) if is_lock_timeout(&error) => {
                idempotency::count(Outcome::InFlight);
                return Err(RequestError::IdempotencyKeyInFlight.into());
            }
            Err(error) => return Err(Internal::from(error).into()),
        };
        sqlx::query!("SET LOCAL lock_timeout TO DEFAULT")
            .execute(&mut *self.tx)
            .await
            .map_err(Internal::from)?;
        if inserted {
            idempotency::count(Outcome::New);
            self.claimed = Some((user, key.key().to_owned()));
            return Ok(Claimed::Fresh);
        }
        let record = sqlx::query!(
            "SELECT fingerprint, result FROM idempotency_records WHERE user_id = $1 AND key = $2",
            user.uuid(),
            key.key(),
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(Internal::from)?;
        if record.fingerprint.as_slice() != key.fingerprint().as_slice() {
            idempotency::count(Outcome::Reused);
            return Err(RequestError::IdempotencyKeyReused.into());
        }
        let result = record
            .result
            .ok_or_else(|| Internal::new("a committed idempotency record has no result"))?;
        let result = serde_json::from_value(result).map_err(Internal::new)?;
        idempotency::count(Outcome::Replayed);
        Ok(Claimed::Replay(result))
    }

    /// Commits every write the unit of work holds, with `result` kept under the idempotency key
    /// it claimed, if any, in the same transaction, so a committed operation always has its
    /// result (PLATFORM-018).
    pub async fn commit_with<T: Serialize>(mut self, result: &T) -> Result<(), Internal> {
        if let Some((user, key)) = self.claimed.take() {
            let result = serde_json::to_value(result).map_err(Internal::new)?;
            sqlx::query!(
                "UPDATE idempotency_records SET result = $3 WHERE user_id = $1 AND key = $2",
                user.uuid(),
                key,
                result,
            )
            .execute(&mut *self.tx)
            .await?;
        }
        Ok(self.tx.commit().await?)
    }
}

/// Whether `error` is a wait that `lock_timeout` ended.
fn is_lock_timeout(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .and_then(sqlx::error::DatabaseError::code)
        .is_some_and(|code| code == LOCK_NOT_AVAILABLE)
}
