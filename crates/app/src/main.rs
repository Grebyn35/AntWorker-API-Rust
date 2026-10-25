//! The API binary. Each operational task is a subcommand (ARCH-14).

use std::{
    error::Error,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use clap::{Parser, Subcommand};
use platform::{
    clock::{SharedClock, SystemClock},
    config::{
        BootstrapConfig, ConfigError, Environment, HealthConfig, MigrateConfig, ServerConfig,
        TelemetryConfig,
    },
    db::Database,
    http::Pipeline,
    lifecycle::{self, Readiness, Shutdown, ShutdownSignals},
    telemetry::{Service, names},
};
use tokio::net::TcpListener;

/// The image's static binary allocates through mimalloc, for the reason docs/architecture.md
/// gives in §10.
#[cfg(target_env = "musl")]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// What identifies the API in its telemetry and its OpenAPI document.
const SERVICE: Service = Service {
    name: env!("CARGO_BIN_NAME"),
    version: env!("CARGO_PKG_VERSION"),
};

#[derive(Parser)]
#[command(name = env!("CARGO_BIN_NAME"), version, about = "The API server and its operational tasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Creates the database roles and their grants. Runs with an administrator's connection.
    Bootstrap,
    /// Applies the migrations the database hasn't recorded.
    Migrate,
    /// Runs the HTTP server.
    Serve,
    /// Asks the instance that `LISTEN_ADDR` names whether it is ready, for a container's health
    /// check, and exits 0 only if it is.
    Health,
    /// Writes the OpenAPI document of every route to a file (API-17).
    Openapi {
        /// The file to write.
        path: PathBuf,
    },
}

fn main() -> ExitCode {
    let command = Cli::parse().command;
    let settings = platform::config::from_process();
    match command {
        Command::Bootstrap => start(settings.telemetry(), settings.bootstrap(), bootstrap),
        Command::Migrate => start(settings.telemetry(), settings.migrate(), migrate),
        Command::Serve => start(settings.telemetry(), settings.server(), serve),
        Command::Health => health(settings.health()),
        Command::Openapi { path } => openapi(&path),
    }
}

/// Sets up telemetry, runs `task` with its configuration on a new async runtime, then writes
/// out the telemetry and the logs (PLATFORM-042).
fn start<C, F>(
    telemetry: Result<TelemetryConfig, ConfigError>,
    config: Result<C, ConfigError>,
    task: impl FnOnce(C) -> F,
) -> ExitCode
where
    F: Future<Output = ExitCode>,
{
    let environment = telemetry
        .as_ref()
        .ok()
        .map(|telemetry| telemetry.environment);
    let (telemetry, config) = match telemetry.and_then(|telemetry| Ok((telemetry, config?))) {
        Ok(loaded) => loaded,
        Err(error) => return failed_before_logging(environment, "config", &error),
    };
    let telemetry = match platform::telemetry::init(SERVICE, &telemetry) {
        Ok(telemetry) => telemetry,
        Err(error) => return failed_before_logging(environment, "telemetry", &error),
    };
    let code = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(task(config)),
        Err(error) => failed("runtime", &error),
    };
    telemetry.finish();
    code
}

async fn bootstrap(config: BootstrapConfig) -> ExitCode {
    match platform::db::bootstrap(&config).await {
        Ok(()) => {
            tracing::info!(
                { names::DB_NAMESPACE } = config.admin.database(),
                "bootstrap finished"
            );
            ExitCode::SUCCESS
        }
        Err(error) => failed("bootstrap", &error),
    }
}

async fn migrate(config: MigrateConfig) -> ExitCode {
    match platform::db::migrate(&config.owner_url, &app::MIGRATIONS).await {
        Ok(applied) => {
            tracing::info!(
                { names::MIGRATION_VERSIONS } = ?applied,
                "migrations applied"
            );
            ExitCode::SUCCESS
        }
        Err(error) => failed("migrate", &error),
    }
}

/// Installs the shutdown signals' handlers before it connects to anything (OPS-06), then serves
/// until a signal arrives.
async fn serve(config: ServerConfig) -> ExitCode {
    let signals = match ShutdownSignals::install() {
        Ok(signals) => signals,
        Err(error) => return failed("signals", &error),
    };
    let database = match Database::connect(
        &config.database_url,
        config.database_max_connections,
        &app::MIGRATIONS,
    )
    .await
    {
        Ok(database) => database,
        Err(error) => return failed("database", &error),
    };
    tracing::info!(
        { names::DATABASE_CONNECTIONS } = database.connections(),
        "connected to the database"
    );
    let clock: SharedClock = Arc::new(SystemClock);
    let readiness = Readiness::new();
    database.watch(readiness.clone(), clock.clone());
    let (listener, address) = match bind(config.listen_addr).await {
        Ok(bound) => bound,
        Err(error) => return failed("listen", &error),
    };
    tracing::info!(
        { names::SERVER_ADDRESS } = %address.ip(),
        { names::SERVER_PORT } = address.port(),
        "listening"
    );
    let app = platform::http::app(
        app::routes(),
        Pipeline {
            clock: clock.clone(),
            max_concurrent_requests: usize::try_from(config.max_concurrent_requests)
                .unwrap_or(usize::MAX),
            readiness: readiness.clone(),
        },
    );
    let shutdown = Shutdown {
        delay: config.shutdown_delay,
        drain: config.shutdown_drain,
    };
    match lifecycle::serve(
        listener,
        app,
        readiness,
        clock,
        shutdown,
        signals.received(),
    )
    .await
    {
        Ok(()) => {
            tracing::info!("stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(
                { names::EXCEPTION_MESSAGE } = platform::errors::chain(&error),
                "server stopped"
            );
            ExitCode::FAILURE
        }
    }
}

/// Binds `address`, and returns the listener with the address it bound, whose port the system
/// chose when `address` asks for port 0.
async fn bind(address: SocketAddr) -> std::io::Result<(TcpListener, SocketAddr)> {
    let listener = TcpListener::bind(address).await?;
    let bound = listener.local_addr()?;
    Ok((listener, bound))
}

/// Asks the instance whether it is ready (PLATFORM-044). It runs no async runtime and writes no
/// logs, so a health check that runs it every few seconds adds nothing to them.
fn health(config: Result<HealthConfig, ConfigError>) -> ExitCode {
    let config = match config {
        Ok(config) => config,
        Err(error) => return failed_before_logging(None, "config", &error),
    };
    match lifecycle::probe(config.listen_addr) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => failed_before_logging(None, "health", &error),
    }
}

/// Writes the OpenAPI document. It runs no async runtime and writes no logs.
fn openapi(path: &Path) -> ExitCode {
    let document = match app::routes().openapi(SERVICE).to_pretty_json() {
        Ok(document) => document,
        Err(error) => return failed_before_logging(None, "openapi", &error),
    };
    #[expect(
        clippy::disallowed_methods,
        reason = "the document is written while no async runtime exists (CODE-05)"
    )]
    let written = std::fs::write(path, document + "\n");
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => failed_before_logging(None, "openapi", &error),
    }
}

fn failed(area: &str, error: &(dyn Error + 'static)) -> ExitCode {
    lifecycle::report_startup_failure(area, error);
    ExitCode::FAILURE
}

#[expect(
    clippy::print_stderr,
    reason = "logging isn't set up, so stderr is the only output (OPS-03)"
)]
fn failed_before_logging(
    environment: Option<Environment>,
    area: &str,
    error: &(dyn Error + 'static),
) -> ExitCode {
    eprintln!(
        "{}",
        lifecycle::startup_failure_line(SERVICE, environment, area, error)
    );
    ExitCode::FAILURE
}
