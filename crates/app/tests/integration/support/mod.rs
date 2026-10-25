//! Test support (TEST-06, TEST-10).
//!
//! [`Api`] runs the routes in process, and checks every response against their OpenAPI
//! document (API-17).
//!
//! Every test gets a database of its own. A migrated one is cloned from a template that the
//! production `migrate` migrated. `Admin::create_roles` and `Admin::grant_database`, the steps
//! that `bootstrap` runs, create the roles and grants, so the API under test connects as `app`
//! with exactly its production privileges. Settings come from the environment, or from `.env`
//! when a test runs outside `just`.
#![expect(
    clippy::disallowed_types,
    reason = "the harness creates and drops databases and roles, and arranges tests on raw \
              connections, none of which a unit of work can do"
)]

pub mod api;
mod contract;
pub mod fixtures;

use std::{
    error::Error,
    ffi::{OsStr, OsString},
    hash::{DefaultHasher, Hash, Hasher},
    io::{self, BufRead, BufReader},
    net::SocketAddr,
    path::Path,
    process::{Child, Command, Output, Stdio},
    sync::{
        Mutex, PoisonError,
        atomic::{AtomicU32, Ordering},
    },
};

use platform::{
    config::BootstrapConfig,
    db::{Admin, Database, DatabaseUrl, migrate, testing},
};
use serde_json::Value;
use sqlx::{AssertSqlSafe, Connection, PgConnection, migrate::Migrator};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{OnceCell, mpsc},
};

pub use api::Api;

/// The API binary that Cargo built for the tests.
pub const BINARY: &str = env!("CARGO_BIN_EXE_antworker-api");

/// The binary's name, which is the service's name in its telemetry (OBS-02).
pub fn service_name() -> &'static str {
    Path::new(BINARY)
        .file_stem()
        .and_then(|name| name.to_str())
        .expect("the binary's name")
}

/// The advisory lock that serializes creating the roles and the template, across processes.
const SETUP_LOCK: i64 = 7_300_154_123;

/// The template every migrated test database is cloned from, created once per process.
static TEMPLATE: OnceCell<String> = OnceCell::const_new();

/// Numbers the databases and roles this process creates.
static NEXT: AtomicU32 = AtomicU32::new(0);

/// A database for one test, with the roles' grants. Dropping it drops the database and the
/// roles the test created.
pub struct TestDatabase {
    name: String,
    settings: BootstrapConfig,
    roles: Mutex<Vec<String>>,
}

impl TestDatabase {
    /// A database migrated with the API's migrations.
    pub async fn migrated() -> Self {
        let settings = settings();
        let template = template(&settings).await;
        Self::create(settings, Some(template)).await
    }

    /// A database with the roles' grants and no migrations.
    pub async fn empty() -> Self {
        let settings = settings();
        template(&settings).await;
        Self::create(settings, None).await
    }

    async fn create(settings: BootstrapConfig, template: Option<&str>) -> Self {
        let name = unique(&settings, "test");
        let mut admin = connect(&settings.admin).await;
        let template = template
            .map(|template| format!(" TEMPLATE {}", identifier(template)))
            .unwrap_or_default();
        execute(
            &mut admin,
            &format!("CREATE DATABASE {}{template}", identifier(&name)),
        )
        .await;
        close(admin).await;
        let database = Self {
            name,
            settings,
            roles: Mutex::default(),
        };
        let mut grants = Admin::connect(&database.admin_url())
            .await
            .expect("connect as the administrator");
        grants
            .grant_database()
            .await
            .expect("grant the roles their privileges");
        grants.close().await.expect("close the connection");
        database
    }

    /// The database's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The URL the API connects with, as `app`.
    pub fn app_url(&self) -> DatabaseUrl {
        self.settings.app.with_database(&self.name)
    }

    /// The URL `migrate` connects with, as `owner`.
    pub fn owner_url(&self) -> DatabaseUrl {
        self.settings.owner.with_database(&self.name)
    }

    fn admin_url(&self) -> DatabaseUrl {
        self.settings.admin.with_database(&self.name)
    }

    /// The settings `bootstrap` reads, pointing at this database.
    pub fn bootstrap_config(&self) -> BootstrapConfig {
        BootstrapConfig {
            admin: self.admin_url(),
            owner: self.owner_url(),
            app: self.app_url(),
        }
    }

    /// Runs `operation` while holding the lock that the tests of every process take before
    /// they change the roles. Roles belong to the whole server, so two tests that run
    /// `bootstrap` at once would update the same roles and fail with "tuple concurrently
    /// updated".
    pub async fn with_roles_locked<T>(&self, operation: impl Future<Output = T>) -> T {
        let mut admin = connect(&self.settings.admin).await;
        execute(
            &mut admin,
            &format!("SELECT pg_advisory_lock({SETUP_LOCK})"),
        )
        .await;
        let result = operation.await;
        // Ending the session releases the lock.
        close(admin).await;
        result
    }

    /// Runs the API binary with `args`, as an operator would, with its database settings
    /// pointing at this database and `env` added to its environment. Returns its exit status
    /// and everything it printed.
    pub async fn run_api<V: AsRef<OsStr>>(&self, args: &[&str], env: &[(&str, V)]) -> Output {
        run(self.api_command(args, env)).await
    }

    /// Starts `serve` from the API binary, as an operator would, on a free port of 127.0.0.1,
    /// with its database settings pointing at this database and `env` added to its
    /// environment.
    pub async fn serve(&self, env: &[(&str, &str)]) -> Server {
        let mut command = self.api_command(&["serve"], env);
        command.env("LISTEN_ADDR", "127.0.0.1:0");
        Server::start(command).await
    }

    fn api_command<V: AsRef<OsStr>>(&self, args: &[&str], env: &[(&str, V)]) -> Command {
        let mut command = Command::new(BINARY);
        command
            .args(args)
            .envs(env.iter().map(|(name, value)| (name, value)));
        for setting in ["DATABASE_ADMIN_URL", "DATABASE_OWNER_URL", "DATABASE_URL"] {
            let mut url =
                url::Url::parse(&std::env::var(setting).expect(setting)).expect("a database URL");
            url.set_path(&self.name);
            command.env(setting, url.as_str());
        }
        command
    }

    /// The most connections the pool from [`TestDatabase::app`] opens.
    pub const POOL_SIZE: u32 = 4;

    /// The API's pool, after the startup checks against the API's migrations.
    pub async fn app(&self) -> Database {
        Database::connect(&self.app_url(), Self::POOL_SIZE, &app::MIGRATIONS)
            .await
            .expect("connect as app")
    }

    /// Runs `sql` as `owner`, to arrange what a migration would create.
    pub async fn owner_executes(&self, sql: &str) {
        let mut owner = connect(&self.owner_url()).await;
        execute(&mut owner, sql).await;
        close(owner).await;
    }

    /// Runs `sql` as the administrator, to arrange what no role of the API may do.
    pub async fn admin_executes(&self, sql: &str) {
        let mut admin = connect(&self.admin_url()).await;
        execute(&mut admin, sql).await;
        close(admin).await;
    }

    /// The first column of every row `sql` returns, read as `owner` (TEST-06), for tables a
    /// test created itself.
    pub async fn owner_reads(&self, sql: &str) -> Vec<String> {
        reads(&self.owner_url(), sql).await
    }

    /// The first column of every row `sql` returns, read as the administrator, for state that
    /// only an administrator may read, such as password verifiers.
    pub async fn admin_reads(&self, sql: &str) -> Vec<String> {
        reads(&self.admin_url(), sql).await
    }

    /// Runs `sql` on a connection with `url`, and returns the database's error message if it
    /// fails.
    #[expect(
        clippy::disallowed_methods,
        reason = "raw SQL arranges and reads what no unit of work may (TEST-06)"
    )]
    pub async fn try_execute(&self, url: &DatabaseUrl, sql: &str) -> Result<(), String> {
        let mut conn = connect(url).await;
        let result = sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
            .execute(&mut conn)
            .await
            .map(|_| ())
            .map_err(|error| error.to_string());
        close(conn).await;
        result
    }

    /// The value of a setting for a connection with `url`.
    pub async fn setting(&self, url: &DatabaseUrl, name: &str) -> String {
        let mut conn = connect(url).await;
        let value = sqlx::query_scalar!(r#"SELECT current_setting($1) AS "value!""#, name)
            .fetch_one(&mut conn)
            .await
            .expect("read the setting");
        close(conn).await;
        value
    }

    /// The migration versions recorded in this database, in order.
    pub async fn applied_versions(&self) -> Vec<i64> {
        let mut owner = connect(&self.owner_url()).await;
        let versions = sqlx::query_scalar!("SELECT version FROM _sqlx_migrations ORDER BY version")
            .fetch_all(&mut owner)
            .await
            .expect("read the applied migrations");
        close(owner).await;
        versions
    }

    /// Each table in the schema, other than the migrations table, with its owner, by name.
    pub async fn tables(&self) -> Vec<(String, String)> {
        let mut owner = connect(&self.owner_url()).await;
        let tables = sqlx::query!(
            r#"
            SELECT tablename AS "name!", tableowner AS "owner!"
            FROM pg_tables
            WHERE schemaname = 'public' AND tablename <> '_sqlx_migrations'
            ORDER BY tablename
            "#
        )
        .fetch_all(&mut owner)
        .await
        .expect("read the tables");
        close(owner).await;
        tables.into_iter().map(|t| (t.name, t.owner)).collect()
    }

    /// Creates a login role for this test only, with `attributes` such as `SUPERUSER`, and
    /// returns its name and a URL that connects to this database as it.
    pub async fn role(&self, attributes: &str) -> (String, DatabaseUrl) {
        let name = unique(&self.settings, "role");
        self.roles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(name.clone());
        self.admin_executes(&format!(
            "CREATE ROLE {} LOGIN PASSWORD '{name}' {attributes}",
            identifier(&name)
        ))
        .await;
        let (host, port) = testing::address(&self.settings.admin);
        let url = format!("postgres://{name}:{name}@{host}:{port}/{}", self.name)
            .parse()
            .expect("a role URL");
        (name, url)
    }
}

impl Drop for TestDatabase {
    #[expect(
        clippy::disallowed_methods,
        reason = "raw SQL arranges and reads what no unit of work may (TEST-06)"
    )]
    fn drop(&mut self) {
        let admin = self.settings.admin.clone();
        let name = self.name.clone();
        let roles = std::mem::take(self.roles.get_mut().unwrap_or_else(PoisonError::into_inner));
        // Drop can't await, so the cleanup runs on a runtime of its own.
        let dropped = std::thread::spawn(move || -> Result<(), Box<dyn Error + Send + Sync>> {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?
                .block_on(async move {
                    let mut conn = testing::connect(&admin).await?;
                    // Each statement goes alone: DROP DATABASE can't run in the implicit
                    // transaction that a batch of statements shares.
                    let database =
                        format!("DROP DATABASE IF EXISTS {} WITH (FORCE)", identifier(&name));
                    let roles = roles
                        .iter()
                        .map(|role| format!("DROP ROLE IF EXISTS {}", identifier(role)));
                    for statement in std::iter::once(database).chain(roles) {
                        sqlx::raw_sql(AssertSqlSafe(statement))
                            .execute(&mut conn)
                            .await?;
                    }
                    conn.close().await?;
                    Ok(())
                })
        })
        .join();
        if !std::thread::panicking() {
            dropped
                .expect("the cleanup thread")
                .expect("drop the test database and roles");
        }
    }
}

/// Runs the API binary with `args`, with `env` as its whole environment, for a subcommand that
/// reads no database settings.
pub async fn run_binary(args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut command = Command::new(BINARY);
    command.args(args).env_clear().envs(env.iter().copied());
    // Windows can't open a socket in a process whose environment lacks SystemRoot.
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("SystemRoot", root);
    }
    run(command).await
}

async fn run(mut command: Command) -> Output {
    tokio::task::spawn_blocking(move || command.output())
        .await
        .expect("the thread that runs the API binary")
        .expect("run the API binary")
}

/// Sends a request with `method` to `path` on `address`, with `headers`, over a connection of
/// its own, and returns the whole response as text: the status line, the headers and the body.
pub async fn http_request(
    address: SocketAddr,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> io::Result<String> {
    let mut stream = TcpStream::connect(address).await?;
    let headers: String = headers
        .iter()
        .map(|(name, value)| format!("{name}: {value}\r\n"))
        .collect();
    let request =
        format!("{method} {path} HTTP/1.1\r\nHost: api\r\n{headers}Connection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    Ok(response)
}

/// A running `serve`, whose stdout a test reads line by line. Dropping it kills the process.
pub struct Server {
    child: Child,
    lines: mpsc::UnboundedReceiver<String>,
    address: SocketAddr,
    listening: Value,
}

impl Server {
    async fn start(mut command: Command) -> Self {
        let mut child = command
            .stdout(Stdio::piped())
            .spawn()
            .expect("start the API binary");
        let stdout = child.stdout.take().expect("the binary's stdout");
        let (send, lines) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        let mut server = Self {
            child,
            lines,
            address: SocketAddr::from(([127, 0, 0, 1], 0)),
            listening: Value::Null,
        };
        server.listening = server.line(|line| line["message"] == "listening").await;
        let port = server.listening["server.port"]
            .as_u64()
            .expect("the port it listens on");
        server.address.set_port(u16::try_from(port).unwrap());
        server
    }

    /// The address the server listens on.
    pub fn address(&self) -> SocketAddr {
        self.address
    }

    /// The line the server wrote when it started listening.
    pub fn listening(&self) -> &Value {
        &self.listening
    }

    /// The next line the server writes for which `matches` holds, as JSON. Panics if the
    /// server exits first.
    pub async fn line(&mut self, matches: impl Fn(&Value) -> bool) -> Value {
        loop {
            let line = self
                .lines
                .recv()
                .await
                .expect("the server exited before writing the line");
            let value: Value = serde_json::from_str(&line)
                .map_err(|_| format!("a line that isn't JSON: {line}"))
                .unwrap();
            if matches(&value) {
                return value;
            }
        }
    }

    /// Sends a request with `method` to `path`, with `headers`, and returns the whole response
    /// as text.
    pub async fn request(&self, method: &str, path: &str, headers: &[(&str, &str)]) -> String {
        http_request(self.address, method, path, headers)
            .await
            .expect("a response from the server")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Killing a process that has exited already succeeds.
        let stopped = self.child.kill().and_then(|()| self.child.wait());
        if !std::thread::panicking() {
            stopped.expect("stop the API binary");
        }
    }
}

/// A value that isn't valid Unicode, which the process environment can still hold.
pub fn not_unicode() -> OsString {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![b'i', b'n', b'f', b'o', 0x80])
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[u16::from(b'i'), u16::from(b'n'), 0xD800])
    }
}

/// Runs `sql` through `executor`, such as a unit of work's.
pub async fn execute_on(executor: &mut PgConnection, sql: &str) {
    execute(executor, sql).await;
}

fn settings() -> BootstrapConfig {
    if let Err(error) = dotenvy::dotenv() {
        assert!(error.not_found(), "can't read .env: {error}");
    }
    platform::config::from_process()
        .bootstrap()
        .expect("DATABASE_ADMIN_URL, DATABASE_OWNER_URL and DATABASE_URL; run `just setup`")
}

/// The name of the template database, which holds the API's migrations. The first test in a
/// process creates the roles, and creates the template unless one with the same migrations
/// exists, all under an advisory lock that other test processes wait on.
async fn template(settings: &BootstrapConfig) -> &'static str {
    TEMPLATE
        .get_or_init(|| async {
            let name = format!(
                "{}_template_{:016x}",
                settings.admin.database(),
                fingerprint(&app::MIGRATIONS)
            );
            let mut admin = connect(&settings.admin).await;
            execute(
                &mut admin,
                &format!("SELECT pg_advisory_lock({SETUP_LOCK})"),
            )
            .await;
            let mut roles = Admin::connect(&settings.admin)
                .await
                .expect("connect as the administrator");
            roles
                .create_roles(&settings.owner, &settings.app)
                .await
                .expect("create the roles");
            roles.close().await.expect("close the connection");
            if !database_exists(&mut admin, &name).await {
                build_template(&mut admin, settings, &name).await;
            }
            close(admin).await;
            name
        })
        .await
}

async fn build_template(admin: &mut PgConnection, settings: &BootstrapConfig, name: &str) {
    let prefix = format!("{}_template_", settings.admin.database());
    let stale = sqlx::query_scalar!(
        r#"SELECT datname AS "name!" FROM pg_database WHERE starts_with(datname, $1)"#,
        prefix
    )
    .fetch_all(&mut *admin)
    .await
    .expect("list the templates");
    for template in stale {
        let template = identifier(&template);
        execute(
            admin,
            &format!("ALTER DATABASE {template} IS_TEMPLATE false"),
        )
        .await;
        execute(admin, &format!("DROP DATABASE {template} WITH (FORCE)")).await;
    }
    let building = format!("{name}_building");
    execute(admin, &format!("CREATE DATABASE {}", identifier(&building))).await;
    let mut grants = Admin::connect(&settings.admin.with_database(&building))
        .await
        .expect("connect as the administrator");
    grants
        .grant_database()
        .await
        .expect("grant the roles their privileges");
    grants.close().await.expect("close the connection");
    migrate(&settings.owner.with_database(&building), &app::MIGRATIONS)
        .await
        .expect("migrate the template");
    let name = identifier(name);
    execute(
        admin,
        &format!("ALTER DATABASE {} RENAME TO {name}", identifier(&building)),
    )
    .await;
    execute(admin, &format!("ALTER DATABASE {name} IS_TEMPLATE true")).await;
}

async fn database_exists(admin: &mut PgConnection, name: &str) -> bool {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM pg_database WHERE datname = $1) AS "exists!""#,
        name
    )
    .fetch_one(admin)
    .await
    .expect("look up the database")
}

/// A hash of every migration's version and content, which names the template they built.
fn fingerprint(migrations: &Migrator) -> u64 {
    let mut hasher = DefaultHasher::new();
    for migration in migrations.iter() {
        migration.version.hash(&mut hasher);
        migration.checksum.hash(&mut hasher);
    }
    hasher.finish()
}

/// A name no other database or role on the server has, such as `api_test_4120_7`.
fn unique(settings: &BootstrapConfig, kind: &str) -> String {
    format!(
        "{}_{kind}_{}_{}",
        settings.admin.database(),
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

async fn connect(url: &DatabaseUrl) -> PgConnection {
    testing::connect(url)
        .await
        .expect("connect to the test database")
}

/// The first column of every row `sql` returns, on a connection with `url`.
#[expect(
    clippy::disallowed_methods,
    reason = "raw SQL arranges and reads what no unit of work may (TEST-06)"
)]
async fn reads(url: &DatabaseUrl, sql: &str) -> Vec<String> {
    let mut conn = connect(url).await;
    let values = sqlx::query_scalar(AssertSqlSafe(sql.to_owned()))
        .fetch_all(&mut conn)
        .await
        .map_err(|error| format!("{sql}: {error}"))
        .unwrap();
    close(conn).await;
    values
}

async fn close(conn: PgConnection) {
    conn.close().await.expect("close the connection");
}

#[expect(
    clippy::disallowed_methods,
    reason = "raw SQL arranges and reads what no unit of work may (TEST-06)"
)]
async fn execute(conn: &mut PgConnection, sql: &str) {
    sqlx::raw_sql(AssertSqlSafe(sql.to_owned()))
        .execute(conn)
        .await
        .map_err(|error| format!("{sql}: {error}"))
        .unwrap();
}

fn identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
