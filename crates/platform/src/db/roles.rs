//! The two database roles: `bootstrap` creates them (PLATFORM-010), and `serve` checks the
//! role it connects as (PLATFORM-009).

use std::fmt;

use sqlx::{AssertSqlSafe, Connection, PgConnection};

use super::DatabaseUrl;
use crate::config::BootstrapConfig;

/// The role that runs migrations and owns the tables (DATA-12).
pub(super) const OWNER: &str = "owner";

/// The role that serves every request and runs every job (DATA-12).
pub(super) const APP: &str = "app";

/// The longest a statement of the `app` role may run (DATA-19).
const STATEMENT_TIMEOUT: &str = "30s";

/// The longest the `app` role may leave a transaction idle, which bounds how long a stuck
/// unit of work holds its locks (DATA-19).
const IDLE_IN_TRANSACTION_TIMEOUT: &str = "15s";

/// The attributes both roles are created with: they can log in, and hold no power beyond what
/// their grants give them.
const ATTRIBUTES: &str = "LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS";

/// `bootstrap` couldn't finish.
#[derive(Debug, thiserror::Error)]
pub enum BootstrapError {
    /// The URLs name different databases.
    #[error("DATABASE_ADMIN_URL, DATABASE_OWNER_URL and DATABASE_URL must name the same database")]
    DifferentDatabases,
    /// The administrator connection failed.
    #[error("can't connect with DATABASE_ADMIN_URL")]
    Connect(#[source] sqlx::Error),
    /// A statement failed, or the connection broke.
    #[error("can't set up the roles and grants")]
    Failed(#[source] sqlx::Error),
}

/// Creates or updates the roles `owner` and `app`, then grants them their privileges on the
/// database the URLs name (PLATFORM-010). Running it again leaves the roles, their attributes
/// and settings, and the grants as they are, and sets each role's password to the one in its
/// URL.
pub async fn bootstrap(config: &BootstrapConfig) -> Result<(), BootstrapError> {
    let database = config.admin.database();
    if config.owner.database() != database || config.app.database() != database {
        return Err(BootstrapError::DifferentDatabases);
    }
    let mut admin = Admin::connect(&config.admin).await?;
    admin.create_roles(&config.owner, &config.app).await?;
    admin.grant_database().await?;
    admin.close().await
}

/// An administrator connection to one database, such as the bundled Postgres superuser or a
/// managed provider's admin role.
pub struct Admin {
    conn: PgConnection,
    database: String,
}

impl Admin {
    /// Connects with an administrator's URL.
    pub async fn connect(url: &DatabaseUrl) -> Result<Self, BootstrapError> {
        let conn = PgConnection::connect_with(&url.connect_options())
            .await
            .map_err(BootstrapError::Connect)?;
        Ok(Self {
            conn,
            database: url.database().to_owned(),
        })
    }

    /// Creates the roles `owner` and `app` with their attributes, and sets each role's password
    /// to the one in its URL. The `app` role also gets its timeouts (DATA-19). An existing role
    /// keeps its attributes, because changing some of them takes a superuser, which a managed
    /// provider's admin role isn't; `serve` refuses a role whose attributes are unsafe
    /// (PLATFORM-009). Roles belong to the whole server, not to one database.
    pub async fn create_roles(
        &mut self,
        owner: &DatabaseUrl,
        app: &DatabaseUrl,
    ) -> Result<(), BootstrapError> {
        self.create_role(OWNER, owner).await?;
        self.create_role(APP, app).await?;
        self.execute(format!(
            "ALTER ROLE {app} SET statement_timeout = {statement};
             ALTER ROLE {app} SET idle_in_transaction_session_timeout = {idle};",
            app = identifier(APP),
            statement = literal(STATEMENT_TIMEOUT),
            idle = literal(IDLE_IN_TRANSACTION_TIMEOUT),
        ))
        .await
    }

    /// Grants the roles their privileges on the connected database (DATA-19): both may
    /// connect, only `owner` may create objects, and no role may create temporary tables.
    pub async fn grant_database(&mut self) -> Result<(), BootstrapError> {
        let database = identifier(&self.database);
        self.execute(format!(
            "REVOKE TEMPORARY ON DATABASE {database} FROM PUBLIC;
             GRANT CONNECT ON DATABASE {database} TO {owner}, {app};
             GRANT CREATE ON SCHEMA public TO {owner};",
            owner = identifier(OWNER),
            app = identifier(APP),
        ))
        .await
    }

    /// Ends the connection.
    pub async fn close(self) -> Result<(), BootstrapError> {
        self.conn.close().await.map_err(BootstrapError::Failed)
    }

    async fn create_role(&mut self, role: &str, url: &DatabaseUrl) -> Result<(), BootstrapError> {
        let exists = sqlx::query_scalar!(
            r#"SELECT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = $1) AS "exists!""#,
            role,
        )
        .fetch_one(&mut self.conn)
        .await
        .map_err(BootstrapError::Failed)?;
        // The password travels as a SCRAM verifier, so the clear text never reaches the
        // server, where statement logging or pg_stat_statements could record it.
        let verifier =
            postgres_protocol::password::scram_sha_256(url.password().expose().as_bytes());
        let attributes = if exists { "LOGIN" } else { ATTRIBUTES };
        let verb = if exists { "ALTER" } else { "CREATE" };
        self.execute(format!(
            "{verb} ROLE {role} {attributes} PASSWORD {password}",
            role = identifier(role),
            password = literal(&verifier),
        ))
        .await
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "role and grant statements take identifiers and a password verifier, which \
                  can't be bind parameters"
    )]
    async fn execute(&mut self, statements: String) -> Result<(), BootstrapError> {
        sqlx::raw_sql(AssertSqlSafe(statements))
            .execute(&mut self.conn)
            .await
            .map_err(BootstrapError::Failed)?;
        Ok(())
    }
}

/// `name` quoted as an SQL identifier.
fn identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `value` quoted as an SQL string literal.
fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// Why the connected role is unsafe for serving (PLATFORM-009).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoleProblem {
    /// The role isn't `app`, so the grants in the migrations don't apply to it.
    NotApp(String),
    /// The role is a superuser.
    Superuser,
    /// The role can create databases.
    CreatesDatabases,
    /// The role can create roles.
    CreatesRoles,
    /// The role can start replication, which streams every change in the server.
    Replicates,
    /// The role bypasses row-level security.
    BypassesRowSecurity,
    /// The role owns the database, or an object in it, or another database or tablespace.
    OwnsObjects,
    /// The role belongs to another role, and may hold that role's privileges.
    MemberOfRoles,
}

impl fmt::Display for RoleProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotApp(role) => write!(f, "it is {role}, not {APP}"),
            Self::Superuser => f.write_str("it is a superuser"),
            Self::CreatesDatabases => f.write_str("it can create databases"),
            Self::CreatesRoles => f.write_str("it can create roles"),
            Self::Replicates => f.write_str("it can start replication"),
            Self::BypassesRowSecurity => f.write_str("it bypasses row-level security"),
            Self::OwnsObjects => f.write_str("it owns objects"),
            Self::MemberOfRoles => f.write_str("it belongs to other roles"),
        }
    }
}

/// Every problem with the role the connection uses. An empty list means the role is the
/// `app` role that `bootstrap` creates.
pub(super) async fn check_role(conn: &mut PgConnection) -> Result<Vec<RoleProblem>, sqlx::Error> {
    let role = sqlx::query!(
        r#"
        SELECT
            r.rolname AS "name!",
            r.rolsuper AS "superuser!",
            r.rolcreatedb AS "creates_databases!",
            r.rolcreaterole AS "creates_roles!",
            r.rolreplication AS "replicates!",
            r.rolbypassrls AS "bypasses_row_security!",
            EXISTS (
                SELECT 1 FROM pg_shdepend d
                WHERE d.refclassid = 'pg_authid'::regclass
                  AND d.refobjid = r.oid
                  AND d.deptype = 'o'
                  AND d.dbid IN (0, (SELECT oid FROM pg_database WHERE datname = current_database()))
            ) AS "owns_objects!",
            EXISTS (SELECT 1 FROM pg_auth_members m WHERE m.member = r.oid) AS "member_of_roles!"
        FROM pg_roles r
        WHERE r.rolname = current_user
        "#
    )
    .fetch_one(conn)
    .await?;
    let checks = [
        (role.name != APP, RoleProblem::NotApp(role.name.clone())),
        (role.superuser, RoleProblem::Superuser),
        (role.creates_databases, RoleProblem::CreatesDatabases),
        (role.creates_roles, RoleProblem::CreatesRoles),
        (role.replicates, RoleProblem::Replicates),
        (role.bypasses_row_security, RoleProblem::BypassesRowSecurity),
        (role.owns_objects, RoleProblem::OwnsObjects),
        (role.member_of_roles, RoleProblem::MemberOfRoles),
    ];
    Ok(checks
        .into_iter()
        .filter_map(|(failed, problem)| failed.then_some(problem))
        .collect())
}

/// The name of the role the connection uses.
pub(super) async fn current_role(conn: &mut PgConnection) -> Result<String, sqlx::Error> {
    sqlx::query_scalar!(r#"SELECT current_user AS "role!""#)
        .fetch_one(conn)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_20_dynamic_identifiers_and_literals_are_quoted() {
        assert_eq!(identifier(r#"we"ird"#), r#""we""ird""#);
        assert_eq!(literal("it's"), "'it''s'");
    }
}
