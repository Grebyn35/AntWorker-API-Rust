//! Migrations: `migrate` applies them (PLATFORM-004 to PLATFORM-006), and `serve` refuses to
//! start until they are applied (PLATFORM-030).

use std::fmt;

use sqlx::{
    AssertSqlSafe, Connection, PgConnection,
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
    /// Migrations are marked `-- no-transaction` and aren't the build of one index, which
    /// [`concurrent_index`] reads. A failure part way through one would leave its earlier
    /// statements applied (PLATFORM-005).
    #[error(
        "migrations {0} are marked to run outside a transaction, which only a migration that \
         holds one `CREATE [UNIQUE] INDEX CONCURRENTLY IF NOT EXISTS <name> ON ...` and nothing \
         else may"
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
/// recorded, in version order, and returns their versions (PLATFORM-004).
///
/// A migration that fails leaves no trace: its changes roll back, and it isn't recorded
/// (PLATFORM-005). One kind runs outside a transaction, and may leave one: a migration marked
/// `-- no-transaction` that builds one index, as [`concurrent_index`] reads it, so that the
/// build holds up no write of its table. A build that fails leaves the index unusable and the
/// migration unrecorded, and the next run drops that index before it builds it again. Nothing
/// is applied while `migrations` holds any other migration marked `-- no-transaction`, or while
/// any applied migration differs from its file. A recorded migration that `migrations` doesn't
/// contain is ignored, so an earlier release can still run `migrate` (PLATFORM-006). Concurrent
/// runs apply each migration once.
#[expect(
    clippy::disallowed_methods,
    reason = "platform::db opens the owner's connection for migrate (DATA-15)"
)]
pub async fn migrate(url: &DatabaseUrl, migrations: &Migrator) -> Result<Vec<i64>, MigrateError> {
    let outside: Vec<i64> = forward(migrations)
        .filter(|migration| migration.no_tx && concurrent_index(migration.sql.as_str()).is_none())
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
            if migration.no_tx
                && let Some(index) = concurrent_index(migration.sql.as_str())
            {
                drop_unusable(conn, &index).await?;
            }
            conn.apply(table, migration).await?;
            newly_applied.push(migration.version);
        }
    }
    Ok(newly_applied)
}

/// The name of the index that the migration `sql` builds outside a transaction, or `None` when
/// `sql` isn't such a build alone (PLATFORM-005). The reader takes each line up to its first
/// `--` outside a string in single quotes. It refuses a text that holds, in what is left, a
/// `/*`, a `"`, a `$` or a backslash, none of which it reads, anything but white space after
/// the first `;` outside a string, or a string that doesn't end. The rest must start, split at
/// white space and in any case, with `CREATE INDEX CONCURRENTLY IF NOT EXISTS <name> ON`, with
/// `UNIQUE` before `INDEX` or not, the name being lowercase ASCII letters, digits and `_`, a
/// digit not first.
pub(super) fn concurrent_index(sql: &str) -> Option<String> {
    let statement = statement(sql)?;
    let mut words = statement.split_whitespace().peekable();
    if !words.next()?.eq_ignore_ascii_case("create") {
        return None;
    }
    if words
        .peek()
        .is_some_and(|word| word.eq_ignore_ascii_case("unique"))
    {
        words.next();
    }
    for expected in ["index", "concurrently", "if", "not", "exists"] {
        if !words.next()?.eq_ignore_ascii_case(expected) {
            return None;
        }
    }
    let name = words.next()?;
    let named = name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !name.starts_with(|c: char| c.is_ascii_digit());
    let on = words.next()?.eq_ignore_ascii_case("on");
    (named && on).then(|| name.to_owned())
}

/// What stands outside the comments of `sql`, when it is one statement that the reader of
/// [`concurrent_index`] takes, without the `;` that ends it.
fn statement(sql: &str) -> Option<String> {
    let mut statement = String::new();
    let (mut quoted, mut ended) = (false, false);
    for line in sql.lines() {
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            let next = chars.peek().copied();
            if !quoted && c == '-' && next == Some('-') {
                break;
            }
            let unread = matches!(c, '"' | '$' | '\\') || (c == '/' && next == Some('*'));
            if unread || (ended && !c.is_whitespace()) {
                return None;
            }
            match c {
                '\'' => quoted = !quoted,
                ';' if !quoted => {
                    ended = true;
                    continue;
                }
                _ => {}
            }
            statement.push(c);
        }
        statement.push('\n');
    }
    let statement = statement.trim();
    (!quoted && !statement.is_empty()).then(|| statement.to_owned())
}

/// Drops the index `index` when it stands unusable, as a concurrent build that failed leaves
/// it, so that the build that follows doesn't take it for built (PLATFORM-005).
#[expect(
    clippy::disallowed_methods,
    reason = "`DROP INDEX` takes an identifier, which can't be a bind parameter; \
              `concurrent_index` read it as letters, digits and `_`"
)]
async fn drop_unusable(conn: &mut PgConnection, index: &str) -> Result<(), sqlx::Error> {
    let unusable = sqlx::query_scalar!(
        r#"SELECT EXISTS (
            SELECT 1 FROM pg_index WHERE indexrelid = to_regclass($1) AND NOT indisvalid
        ) AS "unusable!""#,
        index
    )
    .fetch_one(&mut *conn)
    .await?;
    if unusable {
        sqlx::raw_sql(AssertSqlSafe(format!("DROP INDEX CONCURRENTLY {index}")))
            .execute(conn)
            .await?;
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::concurrent_index;

    #[test]
    fn platform_005_a_migration_outside_a_transaction_is_one_concurrent_build_of_an_index() {
        for (sql, name) in [
            (
                "-- no-transaction\n-- Why.\nCREATE INDEX CONCURRENTLY IF NOT EXISTS \
                 things_name_idx ON things (name);\n",
                "things_name_idx",
            ),
            (
                "create unique index concurrently if not exists a_b_key\n    on a (b)",
                "a_b_key",
            ),
            // A string may hold what ends a statement or starts a comment.
            (
                "CREATE INDEX CONCURRENTLY IF NOT EXISTS a_b_idx ON a (b) \
                 WHERE c = 'x; -- y' AND d = 'it''s'; -- done\n-- more\n",
                "a_b_idx",
            ),
        ] {
            assert_eq!(concurrent_index(sql).as_deref(), Some(name), "{sql}");
        }
    }

    #[test]
    fn platform_005_any_other_migration_outside_a_transaction_is_refused() {
        for sql in [
            // Not the build of an index alone.
            "-- no-transaction\nCREATE TABLE first (id integer);",
            "CREATE INDEX a_b_idx ON a (b);",
            "CREATE INDEX CONCURRENTLY a_b_idx ON a (b);",
            "CREATE INDEX IF NOT EXISTS a_b_idx ON a (b);",
            "DROP INDEX CONCURRENTLY IF EXISTS a_b_idx;",
            "CREATE UNIQUE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS a_b_idx ON a (b);",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS ON a (b);",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS a_b_idx a (b);",
            "-- only a comment\n",
            "",
            // More than one statement, however the second is hidden.
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS a_b_idx ON a (b); DROP TABLE a;",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS a_b_idx ON a (b);\nDROP TABLE a;",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS a_b_idx ON a (b) WHERE c = '--'; DROP TABLE a;",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS a_b_idx ON a (\"b--c\"); DROP TABLE a;",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS a_b_idx ON a (b);;",
            // What the reader doesn't read.
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS a_b_idx ON a (b) /* why */;",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS a_b_idx ON a (b) WHERE c = $$x$$;",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS a_b_idx ON a (b) WHERE c = E'x\\'';",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS a_b_idx ON a (b) WHERE c = 'open;",
            // A name that isn't lowercase letters, digits and `_`, a digit not first.
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS A_b_idx ON a (b);",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS \"a_b_idx\" ON a (b);",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS public.a_b_idx ON a (b);",
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS 1_idx ON a (b);",
        ] {
            assert_eq!(concurrent_index(sql), None, "{sql}");
        }
    }
}
