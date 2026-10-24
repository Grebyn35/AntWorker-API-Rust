//! Health probes and readiness (OPS-05), the bounded shutdown (OPS-06), and the
//! startup-failure format (OPS-03).

use std::{
    error::Error,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{Json, Router, extract::State, http::StatusCode};
use serde::Serialize;
use serde_json::{Map, Value};
use tokio::{net::TcpListener, sync::Notify, task::JoinError};

use crate::{
    clock::SharedClock,
    config::Environment,
    errors,
    telemetry::{self, Service, names},
};

/// The body of every health probe: `{"status": "<status>"}`.
#[derive(Debug, Serialize)]
pub(crate) struct ProbeStatus {
    status: &'static str,
}

/// `GET /health/live`: 200 while the process runs.
pub(crate) async fn live() -> Json<ProbeStatus> {
    Json(ProbeStatus { status: "alive" })
}

/// `GET /health/ready`: 200 while the instance can serve, and 503 with the reason otherwise
/// (PLATFORM-037).
pub(crate) async fn ready(State(readiness): State<Readiness>) -> (StatusCode, Json<ProbeStatus>) {
    let status = readiness.status();
    let code = if status == READY {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (code, Json(ProbeStatus { status }))
}

const READY: &str = "ready";

/// Whether the instance can serve: it isn't draining, and the last check of its own database
/// connection succeeded. Clones share the state.
#[derive(Clone, Debug, Default)]
pub struct Readiness {
    state: Arc<ReadinessState>,
}

#[derive(Debug, Default)]
struct ReadinessState {
    draining: AtomicBool,
    database_unavailable: AtomicBool,
}

impl Readiness {
    /// Ready, until a database check fails or the instance drains.
    pub fn new() -> Self {
        Self::default()
    }

    /// Turns readiness off for the rest of the process's life.
    pub fn drain(&self) {
        self.state.draining.store(true, Ordering::Relaxed);
    }

    /// Records the result of the last database check.
    pub(crate) fn set_database(&self, reachable: bool) {
        self.state
            .database_unavailable
            .store(!reachable, Ordering::Relaxed);
    }

    fn status(&self) -> &'static str {
        if self.state.draining.load(Ordering::Relaxed) {
            "draining"
        } else if self.state.database_unavailable.load(Ordering::Relaxed) {
            "database_unavailable"
        } else {
            READY
        }
    }
}

/// The handlers for the signals that shut the server down: SIGTERM and SIGINT, or Ctrl+C on
/// Windows.
#[derive(Debug)]
pub struct ShutdownSignals {
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(windows)]
    interrupt: tokio::signal::windows::CtrlC,
}

impl ShutdownSignals {
    /// Installs the handlers. `serve` calls it inside the async runtime, before it connects to
    /// anything, so an instance that couldn't hear SIGTERM never starts (OPS-06).
    pub fn install() -> io::Result<Self> {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Ok(Self {
                terminate: signal(SignalKind::terminate())?,
                interrupt: signal(SignalKind::interrupt())?,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                interrupt: tokio::signal::windows::ctrl_c()?,
            })
        }
    }

    /// Completes when the first of the signals arrives.
    pub async fn received(mut self) {
        #[cfg(unix)]
        tokio::select! {
            _ = self.terminate.recv() => {}
            _ = self.interrupt.recv() => {}
        }
        #[cfg(windows)]
        self.interrupt.recv().await;
    }
}

/// How `serve` shuts down (OPS-06).
#[derive(Clone, Copy, Debug)]
pub struct Shutdown {
    /// How long the server keeps serving after the signal, while readiness answers `draining`.
    pub delay: Duration,
    /// How long it waits for the requests in flight once it stops accepting connections.
    pub drain: Duration,
}

/// Serves `app` on `listener` until `signal` completes, then shuts down (PLATFORM-038):
/// readiness turns to `draining`, the server keeps serving for the delay, stops accepting
/// connections, and waits for the requests in flight until the drain deadline. A request still
/// running then is dropped.
pub async fn serve(
    listener: TcpListener,
    app: Router,
    readiness: Readiness,
    clock: SharedClock,
    shutdown: Shutdown,
    signal: impl Future<Output = ()>,
) -> io::Result<()> {
    let stop = Arc::new(Notify::new());
    let stopped = stop.clone();
    let server =
        axum::serve(listener, app).with_graceful_shutdown(async move { stopped.notified().await });
    let mut server = tokio::spawn(server.into_future());
    tokio::select! {
        () = signal => {}
        result = &mut server => return joined(result),
    }
    readiness.drain();
    tracing::info!("shutting down");
    clock.sleep(shutdown.delay).await;
    // Kept until the server waits for it, if it hasn't started waiting yet.
    stop.notify_one();
    tokio::select! {
        result = &mut server => joined(result),
        () = clock.sleep(shutdown.drain) => {
            server.abort();
            tracing::warn!("drain deadline passed with requests in flight");
            Ok(())
        }
    }
}

fn joined(result: Result<io::Result<()>, JoinError>) -> io::Result<()> {
    result.map_err(io::Error::other)?
}

/// Logs a startup failure through the logging setup: one `ERROR` event with the message
/// `startup failed`, the part of startup that failed, and the cause chain (OPS-03).
pub fn report_startup_failure(area: &str, error: &(dyn Error + 'static)) {
    tracing::error!(
        { names::STARTUP_AREA } = area,
        { names::EXCEPTION_MESSAGE } = errors::chain(error),
        "startup failed"
    );
}

/// A startup failure as one JSON line with the same message and fields, for a failure before
/// logging is set up. It names the environment when the environment is known.
pub fn startup_failure_line(
    service: Service,
    environment: Option<Environment>,
    area: &str,
    error: &(dyn Error + 'static),
) -> String {
    let mut line = Map::new();
    line.insert("level".to_owned(), "ERROR".into());
    line.insert("message".to_owned(), "startup failed".into());
    line.insert(names::STARTUP_AREA.to_owned(), area.into());
    line.insert(
        names::EXCEPTION_MESSAGE.to_owned(),
        errors::chain(error).into(),
    );
    line.insert(names::SERVICE_NAME.to_owned(), service.name.into());
    line.insert(names::SERVICE_VERSION.to_owned(), service.version.into());
    if let Some(environment) = environment {
        line.insert(
            names::DEPLOYMENT_ENVIRONMENT_NAME.to_owned(),
            telemetry::environment_name(environment).into(),
        );
    }
    Value::Object(line).to_string()
}

#[cfg(test)]
mod tests {
    use std::{
        io,
        sync::{Arc, Mutex},
    };

    use tracing_subscriber::{Layer, filter::LevelFilter, fmt::MakeWriter, layer::SubscriberExt};

    use super::*;

    const SERVICE: Service = Service {
        name: "api",
        version: "1.2.3",
    };

    #[derive(Debug, thiserror::Error)]
    #[error("can't bind 0.0.0.0:8080")]
    struct BindFailed(#[source] io::Error);

    /// Collects everything the logger writes.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<Vec<u8>>>);

    impl io::Write for Captured {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl MakeWriter<'_> for Captured {
        type Writer = Self;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    /// The fields a startup failure has, whichever way it is written.
    fn fields(line: &str) -> Value {
        let mut value: Value = serde_json::from_str(line).unwrap();
        value.as_object_mut().unwrap().remove("timestamp");
        value
    }

    #[test]
    fn ops_03_a_startup_failure_before_logging_is_one_json_line() {
        let error = BindFailed(io::Error::from(io::ErrorKind::AddrInUse));
        let line = startup_failure_line(SERVICE, Some(Environment::Production), "listen", &error);
        assert!(!line.contains('\n'));
        assert_eq!(
            fields(&line),
            serde_json::json!({
                "level": "ERROR",
                "message": "startup failed",
                "startup.area": "listen",
                "exception.message": format!(
                    "can't bind 0.0.0.0:8080: {}",
                    io::Error::from(io::ErrorKind::AddrInUse)
                ),
                "service.name": "api",
                "service.version": "1.2.3",
                "deployment.environment.name": "production",
            })
        );
    }

    #[test]
    fn ops_03_a_logged_startup_failure_has_the_same_fields_as_the_line_before_logging() {
        let error = BindFailed(io::Error::from(io::ErrorKind::AddrInUse));
        let captured = Captured::default();
        let subscriber = tracing_subscriber::registry().with(
            telemetry::json(SERVICE, Environment::Production, captured.clone())
                .with_filter(LevelFilter::INFO),
        );
        tracing::subscriber::with_default(subscriber, || {
            report_startup_failure("listen", &error);
        });
        let output = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(
            fields(lines[0]),
            fields(&startup_failure_line(
                SERVICE,
                Some(Environment::Production),
                "listen",
                &error
            ))
        );
    }
}
