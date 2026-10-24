//! Health probes and readiness (OPS-05), the probe that `health` sends (PLATFORM-044), the
//! bounded shutdown (OPS-06), and the startup-failure format (OPS-03).

use std::{
    error::Error,
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use axum::{Json, Router, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
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

/// How long [`probe`] waits for the connection, and for each read of the answer. It is longer
/// than the 2 seconds that Windows takes to report a refused connection, so a probe of an
/// address nothing listens on fails the same way on every platform.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// The most of an answer [`probe`] reads. A probe's answer is a status line, a few headers and
/// `{"status": "<status>"}`.
const PROBE_ANSWER_LIMIT: u64 = 4096;

/// Why the instance that [`probe`] asked isn't ready.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// Nothing accepted the connection.
    #[error("can't reach {address}")]
    Unreachable {
        /// The address probed.
        address: SocketAddr,
        /// Why the connection failed.
        #[source]
        source: io::Error,
    },
    /// The connection or a read of the answer took longer than [`PROBE_TIMEOUT`].
    #[error("{address} didn't answer within {} seconds", PROBE_TIMEOUT.as_secs())]
    TimedOut {
        /// The address probed.
        address: SocketAddr,
    },
    /// The connection broke, or the answer isn't an HTTP response.
    #[error("{address} didn't answer with HTTP")]
    NotHttp {
        /// The address probed.
        address: SocketAddr,
    },
    /// The instance answered with a status other than 200.
    #[error("{address} answered {code} {status}")]
    NotReady {
        /// The address probed.
        address: SocketAddr,
        /// The answer's HTTP status code.
        code: u16,
        /// The `status` of the answer's body, or nothing if the body has none.
        status: String,
    },
}

/// Asks the instance whose `serve` binds `listen_addr` whether it is ready, with
/// `GET /health/ready`, and succeeds only when it answers 200 (PLATFORM-044). An unspecified
/// address, such as `0.0.0.0`, is asked on the loopback address of its family, where an
/// instance that binds it also listens. It blocks, so the binary calls it with no async runtime.
pub fn probe(listen_addr: SocketAddr) -> Result<(), ProbeError> {
    let address = probe_address(listen_addr);
    let failed = |error: io::Error| match error.kind() {
        // A read that times out fails with `WouldBlock` on Unix and `TimedOut` on Windows.
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ProbeError::TimedOut { address },
        _ => ProbeError::NotHttp { address },
    };
    let mut stream = TcpStream::connect_timeout(&address, PROBE_TIMEOUT).map_err(|source| {
        match source.kind() {
            io::ErrorKind::TimedOut => ProbeError::TimedOut { address },
            _ => ProbeError::Unreachable { address, source },
        }
    })?;
    stream
        .set_read_timeout(Some(PROBE_TIMEOUT))
        .map_err(failed)?;
    stream
        .set_write_timeout(Some(PROBE_TIMEOUT))
        .map_err(failed)?;
    let request =
        format!("GET /health/ready HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).map_err(failed)?;
    let mut answer = Vec::new();
    stream
        .take(PROBE_ANSWER_LIMIT)
        .read_to_end(&mut answer)
        .map_err(failed)?;
    let (code, body) = parse_answer(&answer).ok_or(ProbeError::NotHttp { address })?;
    if code == StatusCode::OK.as_u16() {
        return Ok(());
    }
    let status = serde_json::from_slice::<ProbeAnswer>(body)
        .map(|body| body.status)
        .unwrap_or_default();
    Err(ProbeError::NotReady {
        address,
        code,
        status,
    })
}

/// The body of a probe's answer, as [`probe`] reads it.
#[derive(Deserialize)]
struct ProbeAnswer {
    status: String,
}

/// The address [`probe`] connects to for an instance that binds `listen_addr`.
fn probe_address(listen_addr: SocketAddr) -> SocketAddr {
    let ip = match listen_addr.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, listen_addr.port())
}

/// The status code and the body of an HTTP/1.1 answer, or `None` if it isn't one.
fn parse_answer(answer: &[u8]) -> Option<(u16, &[u8])> {
    let end = answer.windows(4).position(|window| window == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&answer[..end]).ok()?;
    let mut status_line = head.lines().next()?.split(' ');
    if !status_line.next()?.starts_with("HTTP/1.") {
        return None;
    }
    let code = status_line.next()?.parse().ok()?;
    Some((code, &answer[end + 4..]))
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
/// running then runs on until it ends or the process does.
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
    fn platform_044_an_unspecified_address_is_probed_on_the_loopback_address_of_its_family() {
        for (listen, probed) in [
            ("0.0.0.0:8080", "127.0.0.1:8080"),
            ("[::]:8080", "[::1]:8080"),
            ("10.1.2.3:8080", "10.1.2.3:8080"),
            ("[fd00::1]:8080", "[fd00::1]:8080"),
        ] {
            assert_eq!(
                probe_address(listen.parse().unwrap()),
                probed.parse::<SocketAddr>().unwrap(),
                "{listen}"
            );
        }
    }

    #[test]
    fn platform_044_an_answer_is_read_as_its_status_code_and_its_body() {
        let answer = b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 21\r\n\r\n{\"status\":\"draining\"}";

        assert_eq!(
            parse_answer(answer),
            Some((503, &b"{\"status\":\"draining\"}"[..]))
        );
        assert_eq!(parse_answer(b"SSH-2.0-OpenSSH_9.9\r\n\r\n"), None);
        assert_eq!(parse_answer(b"HTTP/1.1 200 OK\r\n"), None);
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
