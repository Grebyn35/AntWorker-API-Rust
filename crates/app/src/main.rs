//! The API binary. Each operational task is a subcommand (ARCH-14).

use std::{
    error::Error,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::Arc,
};

use clap::{Parser, Subcommand};
#[expect(
    clippy::disallowed_types,
    reason = "the binary is the one place that uses the system clock (CODE-06)"
)]
use platform::clock::SystemClock;
use platform::{
    blocking::Workers,
    clock::SharedClock,
    config::{
        BootstrapConfig, ConfigError, Environment, HealthConfig, MigrateConfig, ServerConfig,
        TelemetryConfig,
    },
    db::Database,
    lifecycle::{self, Readiness, Shutdown, ShutdownSignals},
    mail::Mail,
    telemetry::{Service, names},
};
use tokio::{net::TcpListener, sync::watch};

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
    /// Fills a development database with demo accounts and workspaces, through the API's routes,
    /// and prints how to sign in. Refuses unless ENVIRONMENT is development.
    Seed,
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
        Command::Serve => {
            let config = settings
                .server()
                .and_then(|server| Ok((server, app::Config::read(&settings)?)));
            start(settings.telemetry(), config, serve)
        }
        Command::Seed => {
            let config = settings
                .server()
                .and_then(|server| Ok((server, app::Config::read(&settings)?)));
            start(settings.telemetry(), config, seed)
        }
        Command::Health => health(settings.health()),
        Command::Openapi { path } => openapi(&path),
    }
}

/// Sets up telemetry, runs `task` with its configuration on a new async runtime, then writes
/// out the telemetry and the logs (PLATFORM-068).
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
    match platform::db::bootstrap(&config, &Workers::new()).await {
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
async fn serve((config, modules): (ServerConfig, app::Config)) -> ExitCode {
    let signals = match ShutdownSignals::install() {
        Ok(signals) => signals,
        Err(error) => return failed("signals", &error),
    };
    let database = match connect(&config).await {
        Ok(database) => database,
        Err(code) => return code,
    };
    let clock: SharedClock = Arc::new(SystemClock);
    let readiness = Readiness::new();
    let listener = match platform::events::listen(&database, clock.as_ref()).await {
        Ok(listener) => listener,
        Err(error) => return failed("listener", &error),
    };
    platform::telemetry::process::watch(clock.clone());
    let socket = match bind(config.listen_addr).await {
        Ok(socket) => socket,
        Err(error) => return failed("listen", &error),
    };
    let resources = match resources(database, clock, readiness, &config) {
        Ok(resources) => resources,
        Err(code) => return code,
    };
    let services = match app::compose(&resources, &config, modules, listener).await {
        Ok(services) => services,
        Err(error) => return failed("services", &error),
    };
    let pipeline = app::pipeline(&resources, &config, &services, SERVICE);
    let app = platform::http::app(services.routes, pipeline);
    let app::Resources {
        database,
        clock,
        readiness,
        ..
    } = resources;
    let shutdown = Shutdown {
        delay: config.shutdown_delay,
        drain: config.shutdown_drain,
    };
    // The jobs stop claiming when the signal arrives, and their runs end by the deadline of the
    // requests in flight (PLATFORM-018). Should the server stop first, the sender drops, which
    // stops them too.
    let (stop_jobs, stopping) = watch::channel(false);
    let grace = shutdown.delay.saturating_add(shutdown.drain);
    let jobs = services
        .jobs
        .run(database.clone(), clock.clone(), stopping, grace);
    let jobs = tokio::spawn(jobs);
    let signal = async move {
        signals.received().await;
        stop_jobs.send_replace(true);
    };
    let served = lifecycle::serve(socket, app, readiness, clock, shutdown, signal).await;
    if let Err(error) = jobs.await {
        tracing::error!(
            { names::EXCEPTION_MESSAGE } = platform::errors::chain(&error),
            "the jobs stopped"
        );
    }
    match served {
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

/// The process's resources for `serve`: `database`, `clock` and `readiness`, with the turns of
/// CPU-heavy work, which it logs, and the deployment's emails, as `config` describes them.
fn resources(
    database: Database,
    clock: SharedClock,
    readiness: Readiness,
    config: &ServerConfig,
) -> Result<app::Resources, ExitCode> {
    let workers = Workers::new();
    tracing::info!(
        { names::WORKERS_TURNS } = workers.turns(),
        "CPU-heavy work takes turns"
    );
    let mail =
        Mail::new(config.mail.clone(), clock.clone()).map_err(|error| failed("mail", &error))?;
    Ok(app::Resources {
        database,
        clock,
        readiness,
        workers,
        mail,
    })
}

/// Fills a development database through the API as `serve` builds it, without binding a port or
/// running a dispatcher, and prints the accounts, their password and the workspaces (the system
/// spec's Seed). Its emails that carry a code go to an inbox of its own, which proves its accounts'
/// addresses. It refuses before it connects unless `ENVIRONMENT` is development (SYSTEM-002).
#[expect(
    clippy::disallowed_methods,
    reason = "the seed reads its accounts' codes from an inbox of its own (the system spec's Seed)"
)]
async fn seed((config, modules): (ServerConfig, app::Config)) -> ExitCode {
    if let Err(error) = app::seed_allowed(config.environment) {
        return failed("seed", &error);
    }
    let database = match connect(&config).await {
        Ok(database) => database,
        Err(code) => return code,
    };
    let clock: SharedClock = Arc::new(SystemClock);
    let listener = match platform::events::listen(&database, clock.as_ref()).await {
        Ok(listener) => listener,
        Err(error) => return failed("listener", &error),
    };
    let (mail, inbox) = match Mail::new(config.mail.clone(), clock.clone()) {
        Ok(mail) => mail.with_inbox(),
        Err(error) => return failed("mail", &error),
    };
    let resources = app::Resources {
        database,
        clock,
        readiness: Readiness::new(),
        workers: Workers::new(),
        mail,
    };
    let services = match app::compose(&resources, &config, modules, listener).await {
        Ok(services) => services,
        Err(error) => return failed("services", &error),
    };
    let pipeline = app::pipeline(&resources, &config, &services, SERVICE);
    let router = platform::http::app(services.routes, pipeline);
    match app::seed(router, &resources, &services.jobs, &inbox).await {
        Ok(report) => {
            print_seeded(&report);
            ExitCode::SUCCESS
        }
        Err(error) => failed("seed", &error),
    }
}

/// Prints how to sign in to what `seed` created (SYSTEM-003).
#[expect(
    clippy::print_stdout,
    reason = "the accounts are the seed's output, for the developer who ran it (CODE-03)"
)]
fn print_seeded(report: &app::SeedReport) {
    println!(
        "Seeded the database. Sign in as any of these, with the password {}:",
        report.password
    );
    for (email, name) in &report.accounts {
        println!("  {email:<24}{name}");
    }
    println!("The workspaces:");
    for (name, id) in &report.workspaces {
        println!("  {name:<24}{id}");
    }
}

/// Connects to the database, after the startup checks, and logs the most connections the
/// instance opens (PLATFORM-007).
async fn connect(config: &ServerConfig) -> Result<Database, ExitCode> {
    let database = Database::connect(
        &config.database_url,
        config.database_max_connections,
        &app::MIGRATIONS,
    )
    .await
    .map_err(|error| failed("database", &error))?;
    tracing::info!(
        { names::DATABASE_CONNECTIONS } = database.connections(),
        "connected to the database"
    );
    Ok(database)
}

/// Binds `address`, and logs the address it bound, whose port the system chose when `address`
/// asks for port 0.
async fn bind(address: SocketAddr) -> std::io::Result<TcpListener> {
    let listener = TcpListener::bind(address).await?;
    let bound = listener.local_addr()?;
    tracing::info!(
        { names::SERVER_ADDRESS } = %bound.ip(),
        { names::SERVER_PORT } = bound.port(),
        "listening"
    );
    Ok(listener)
}

/// Asks the instance whether it is ready (PLATFORM-063). It runs no async runtime and writes no
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

/// Writes the OpenAPI document. It runs no async runtime and writes no logs. The document passes
/// through `serde_json::Value`, whose objects keep their keys in order, so a map that `utoipa`
/// keeps in a `HashMap`, such as an operation's extensions, is written the same every time
/// (API-17).
fn openapi(path: &Path) -> ExitCode {
    let document = serde_json::to_value(app::openapi(SERVICE))
        .and_then(|document| serde_json::to_string_pretty(&document));
    let document = match document {
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
