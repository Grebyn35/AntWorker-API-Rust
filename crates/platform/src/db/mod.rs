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
mod roles;
#[cfg(feature = "test-support")]
pub mod testing;
mod url;

use std::time::Duration;

use sqlx::{
    PgConnection, PgPool, Postgres, Transaction, migrate::Migrator, postgres::PgPoolOptions,
};

pub use migrations::{MigrateError, SchemaProblem, Versions, migrate};
pub use roles::{Admin, BootstrapError, RoleProblem, bootstrap};
pub use url::{DatabaseUrl, InvalidDatabaseUrl};

use crate::errors::Internal;

/// How long an operation waits for a free connection before it fails.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);

/// The API's connection pool, as the role `app`. A value exists only once the startup checks
/// have passed, so a server never runs against an unsafe role, or against a schema that lacks
/// one of its migrations or holds an edited one.
#[derive(Clone)]
pub struct Database {
    pool: PgPool,
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
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .connect_with(url.connect_options())
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
        Ok(Self { pool })
    }

    /// Opens a unit of work.
    pub async fn begin(&self) -> Result<UnitOfWork, Internal> {
        Ok(UnitOfWork {
            tx: self.pool.begin().await?,
        })
    }
}

/// One database transaction, holding every write of one operation (D1, ARCH-11). Dropping it
/// without [`UnitOfWork::commit`] rolls the transaction back.
pub struct UnitOfWork {
    tx: Transaction<'static, Postgres>,
}

impl UnitOfWork {
    /// The connection to run this unit of work's queries on, inside its transaction:
    /// `sqlx::query!(...).fetch_one(uow.executor())`.
    pub fn executor(&mut self) -> &mut PgConnection {
        &mut self.tx
    }

    /// Commits every write the unit of work holds.
    pub async fn commit(self) -> Result<(), Internal> {
        Ok(self.tx.commit().await?)
    }
}
