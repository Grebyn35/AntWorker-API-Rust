//! Migrations: `migrate` applies them (PLATFORM-027 to PLATFORM-029), and `serve` refuses to
//! start until they are applied (PLATFORM-030).

use std::fmt;

use sqlx::{
    Connection, PgConnection,
    migrate::{AppliedMigration, Migrate, MigrateError as SqlxMigrateError, Migration, Migrator},
};

use super::{ConnectError, DatabaseUrl, roles};

/// `migrate` couldn't finish. Every migration it applied before the failure stays applied.
#[derive(Debug, thiserror::Error)]
pub enum MigrateError {
    /// The connection failed.
    #[error("can't connect with DATABASE_OWNER_URL")]
    Connect(#[source] sqlx::Error),
    /// The connection isn't the role `owner`, so the tables would get the wrong owner.
    #[error("DATABASE_OWNER_URL connects as {0}, and must connect as {owner}", owner = roles::OWNER)]
    NotOwner(String),
    /// Applied migrations differ from their files, so the files were edited after they were
    /// applied (DATA-09).
    #[error(
        "migrations {0} differ from the copies already applied; restore the files and put the \
         change in a new migration"
    )]
    Modified(Versions),
    /// Migrations are marked `-- no-transaction`. A failure part way through one would leave
    /// its earlier statements applied (PLATFORM-028).
    #[error(
        "migrations {0} are marked to run outside a transaction; every migration runs in a \
         transaction together with its record"
    )]
    OutsideTransaction(Versions),
    /// A migration or the bookkeeping around it failed.
    #[error("migrations failed")]
    Failed(#[source] SqlxMigrateError),
}

impl From<sqlx::Error> for MigrateError {
    fn from(error: sqlx::Error) -> Self {
        Self::Failed(SqlxMigrateError::from(error))
    }
}

impl From<SqlxMigrateError> for MigrateError {
    fn from(error: SqlxMigrateError) -> Self {
        Self::Failed(error)
    }
}

/// Migration versions, in order.
#[derive(Debug, PartialEq, Eq)]
pub struct Versions(pub Vec<i64>);

impl fmt::Display for Versions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let versions: Vec<String> = self.0.iter().map(i64::to_string).collect();
        f.write_str(&versions.join(", "))
    }
}

/// Applies, as the role `owner`, every migration in `migrations` that the database hasn't
/// recorded, in version order, and returns their versions (PLATFORM-027).
///
/// Each migration runs in one transaction with its record, so a migration that fails leaves
/// no trace (PLATFORM-028). A migration marked `-- no-transaction` would run outside one, so
/// nothing is applied while `migrations` holds one. Nothing is applied while any applied
/// migration differs from its file, and a recorded migration that `migrations` doesn't contain
/// is ignored, so an earlier release can still run `migrate` (PLATFORM-029). Concurrent runs
/// take turns on an advisory lock, so each migration is applied once.
pub async fn migrate(url: &DatabaseUrl, migrations: &Migrator) -> Result<Vec<i64>, MigrateError> {
    let outside: Vec<i64> = forward(migrations)
        .filter(|migration| migration.no_tx)
        .map(|migration| migration.version)
        .collect();
    if !outside.is_empty() {
        return Err(MigrateError::OutsideTransaction(Versions(outside)));
    }
    let mut conn = PgConnection::connect_with(&url.connect_options())
        .await
        .map_err(MigrateError::Connect)?;
    let role = roles::current_role(&mut conn).await?;
    if role != roles::OWNER {
        return Err(MigrateError::NotOwner(role));
    }
    conn.lock().await?;
    let applied = apply_missing(&mut conn, migrations).await?;
    // Closing the connection ends the session, which releases the advisory lock.
    conn.close().await?;
    Ok(applied)
}

async fn apply_missing(
    conn: &mut PgConnection,
    migrations: &Migrator,
) -> Result<Vec<i64>, MigrateError> {
    let table: &str = &migrations.table_name;
    if !table_exists(conn, table).await? {
        conn.ensure_migrations_table(table).await?;
    }
    let applied = conn.list_applied_migrations(table).await?;
    let modified = modified(migrations, &applied);
    if !modified.is_empty() {
        return Err(MigrateError::Modified(Versions(modified)));
    }
    let mut newly_applied = Vec::new();
    for migration in forward(migrations) {
        if !is_applied(migration, &applied) {
            conn.apply(table, migration).await?;
            newly_applied.push(migration.version);
        }
    }
    Ok(newly_applied)
}

/// Why the schema doesn't match this release (PLATFORM-030).
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SchemaProblem {
    /// Migrations this release contains haven't been applied.
    #[error("the database lacks migrations {0}; run migrate first")]
    Missing(Versions),
    /// Applied migrations differ from this release's files.
    #[error("migrations {0} were applied with different content than this release's files")]
    Modified(Versions),
}

/// Checks that every migration in `migrations` is applied, unchanged. The `app` role can read
/// the migrations table but can't create it, so a database never migrated has no table yet.
pub(super) async fn check_schema(
    conn: &mut PgConnection,
    migrations: &Migrator,
) -> Result<(), ConnectError> {
    let table: &str = &migrations.table_name;
    let exists = table_exists(conn, table)
        .await
        .map_err(ConnectError::Check)?;
    let applied = if exists {
        conn.list_applied_migrations(table)
            .await
            .map_err(|error| ConnectError::Check(query_error(error)))?
    } else {
        Vec::new()
    };
    let missing: Vec<i64> = forward(migrations)
        .filter(|m| !is_applied(m, &applied))
        .map(|m| m.version)
        .collect();
    if !missing.is_empty() {
        return Err(SchemaProblem::Missing(Versions(missing)).into());
    }
    let modified = modified(migrations, &applied);
    if !modified.is_empty() {
        return Err(SchemaProblem::Modified(Versions(modified)).into());
    }
    Ok(())
}

/// The database error inside an error of the migrator, so a failed read of the migrations
/// table isn't reported as a failed migration.
fn query_error(error: SqlxMigrateError) -> sqlx::Error {
    match error {
        SqlxMigrateError::Execute(error) => error,
        other => sqlx::Error::Migrate(Box::new(other)),
    }
}

async fn table_exists(conn: &mut PgConnection, table: &str) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar!(r#"SELECT to_regclass($1) IS NOT NULL AS "exists!""#, table)
        .fetch_one(conn)
        .await
}

/// The migrations that move the schema forward. Migrations are forward-only (DATA-09), so a
/// down migration a fork adds by mistake is never applied.
fn forward(migrations: &Migrator) -> impl Iterator<Item = &Migration> {
    migrations
        .iter()
        .filter(|m| !m.migration_type.is_down_migration())
}

fn is_applied(migration: &Migration, applied: &[AppliedMigration]) -> bool {
    applied.iter().any(|a| a.version == migration.version)
}

/// The versions in `migrations` that were applied with different content.
fn modified(migrations: &Migrator, applied: &[AppliedMigration]) -> Vec<i64> {
    forward(migrations)
        .filter(|m| {
            applied
                .iter()
                .any(|a| a.version == m.version && a.checksum != m.checksum)
        })
        .map(|m| m.version)
        .collect()
}
