//! The API binary. Each operational task is a subcommand (ARCH-14).

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use platform::config::ServerConfig;

#[derive(Parser)]
#[command(version, about = "The API server and its operational tasks")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Runs the HTTP server.
    Serve,
}

fn main() -> ExitCode {
    match Cli::parse().command {
        Command::Serve => serve(),
    }
}

fn serve() -> ExitCode {
    let config = match platform::config::from_process().server() {
        Ok(config) => config,
        Err(error) => return failed_before_logging("config", &error),
    };
    if let Err(error) = platform::telemetry::init_logging(config.environment) {
        return failed_before_logging("logging", &error);
    }
    match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime.block_on(run(config)),
        Err(error) => failed("runtime", &error),
    }
}

async fn run(config: ServerConfig) -> ExitCode {
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

fn failed(area: &str, error: &dyn std::fmt::Display) -> ExitCode {
    tracing::error!(area, error = %error, "startup failed");
    ExitCode::FAILURE
}

#[expect(
    clippy::print_stderr,
    reason = "logging isn't set up yet, so stderr is the only output (OPS-03)"
)]
fn failed_before_logging(area: &str, error: &dyn std::fmt::Display) -> ExitCode {
    eprintln!("{}", platform::lifecycle::startup_failure_line(area, error));
    ExitCode::FAILURE
}
