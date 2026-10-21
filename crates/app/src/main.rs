//! The API binary. Each operational task is a subcommand (ARCH-14).

use std::{error::Error, process::ExitCode};

use clap::{Parser, Subcommand};
use platform::{
    config::{BootstrapConfig, ConfigError, LoggingConfig, MigrateConfig, ServerConfig},
    db::Database,
};

#[derive(Parser)]
#[command(version, about = "The API server and its operational tasks")]
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
}

fn main() -> ExitCode {
    let command = Cli::parse().command;
    let settings = platform::config::from_process();
    let logging = settings.logging();
    match command {
        Command::Bootstrap => start(logging, settings.bootstrap(), bootstrap),
        Command::Migrate => start(logging, settings.migrate(), migrate),
        Command::Serve => start(logging, settings.server(), serve),
    }
}

/// Sets up logging, then runs `task` with its configuration on a new async runtime.
fn start<C, F>(
    logging: Result<LoggingConfig, ConfigError>,
    config: Result<C, ConfigError>,
    task: impl FnOnce(C) -> F,
) -> ExitCode
where
    F: Future<Output = ExitCode>,
{
    let (logging, config) = match logging.and_then(|logging| Ok((logging, config?))) {
        Ok(loaded) => loaded,
        Err(error) => return failed_before_logging("config", &error),
    };
    if let Err(error) = platform::telemetry::init_logging(&logging) {
        return failed_before_logging("logging", &error);
    }
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(task(config)),
        Err(error) => failed("runtime", &error),
    }
}

async fn bootstrap(config: BootstrapConfig) -> ExitCode {
    match platform::db::bootstrap(&config).await {
        Ok(()) => {
            tracing::info!(database = config.admin.database(), "bootstrap finished");
            ExitCode::SUCCESS
        }
        Err(error) => failed("bootstrap", &error),
    }
}

async fn migrate(config: MigrateConfig) -> ExitCode {
    match platform::db::migrate(&config.owner_url, &app::MIGRATIONS).await {
        Ok(applied) => {
            tracing::info!(applied = ?applied, "migrations applied");
            ExitCode::SUCCESS
        }
        Err(error) => failed("migrate", &error),
    }
}

async fn serve(config: ServerConfig) -> ExitCode {
    if let Err(error) = Database::connect(
        &config.database_url,
        config.database_max_connections,
        &app::MIGRATIONS,
    )
    .await
    {
        return failed("database", &error);
    }
    let listener = match tokio::net::TcpListener::bind(config.listen_addr).await {
        Ok(listener) => listener,
        Err(error) => return failed("listen", &error),
    };
    tracing::info!(listen_addr = %config.listen_addr, "listening");
    match axum::serve(listener, app::router())
        .with_graceful_shutdown(shutdown_signal())
        .await
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(error = %error, "server stopped");
            ExitCode::FAILURE
        }
    }
}

async fn shutdown_signal() {
    let interrupt = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let terminate = async {
            if let Ok(mut signal) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                signal.recv().await;
            }
        };
        tokio::select! {
            _ = interrupt => {},
            () = terminate => {},
        }
    }
    #[cfg(not(unix))]
    {
        let _interrupted = interrupt.await;
    }
}

fn failed(area: &str, error: &(dyn Error + 'static)) -> ExitCode {
    platform::lifecycle::report_startup_failure(area, error);
    ExitCode::FAILURE
}

#[expect(
    clippy::print_stderr,
    reason = "logging isn't set up yet, so stderr is the only output (OPS-03)"
)]
fn failed_before_logging(area: &str, error: &(dyn Error + 'static)) -> ExitCode {
    eprintln!("{}", platform::lifecycle::startup_failure_line(area, error));
    ExitCode::FAILURE
}
