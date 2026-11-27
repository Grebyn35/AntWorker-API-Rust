//! Health probes and readiness (OPS-05), the probe that `health` sends (PLATFORM-063), the
//! bounded shutdown (OPS-06), the async runtime that a subcommand runs on, and the
//! startup-failure format (OPS-03).

use std::{
    error::Error,
    ffi::OsStr,
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
use tokio::{
    net::TcpListener,
    runtime::Runtime,
    sync::{Notify, watch},
    task::JoinError,
};

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
/// `GET /health/ready`, and succeeds only when it answers 200 (PLATFORM-063). An unspecified
/// address, such as `0.0.0.0`, is asked on the loopback address of its family, where an
/// instance that binds it also listens. It blocks, so the binary calls it with no async runtime.
pub fn probe(listen_addr: SocketAddr) -> Result<(), ProbeError> {
    let address = probe_address(listen_addr);
    let failed = |error: io::Error| match error.kind() {
        // A read that times out fails with `WouldBlock` on Unix and `TimedOut` on Windows.
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock => ProbeError::TimedOut { address },
        _ => ProbeError::NotHttp { address },
    };
    let mut stream = TcpStream::connect_timeout(&address, PROBE_TIMEOUT)
        .map_err(|source| not_connected(address, source))?;
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

/// Why [`probe`] couldn't connect to `address`, from the connection's failure `source`: a
/// connection that took longer than [`PROBE_TIMEOUT`] reached no instance that answered in time.
fn not_connected(address: SocketAddr, source: io::Error) -> ProbeError {
    match source.kind() {
        io::ErrorKind::TimedOut => ProbeError::TimedOut { address },
        _ => ProbeError::Unreachable { address, source },
    }
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
/// (PLATFORM-061).
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

/// Whether the instance can serve: it isn't draining, and the last check of the listener
/// succeeded; and whether it has stopped accepting connections, when its streams
/// close. Clones share the state.
#[derive(Clone, Debug, Default)]
pub struct Readiness {
    state: Arc<ReadinessState>,
}

#[derive(Debug, Default)]
struct ReadinessState {
    draining: AtomicBool,
    database_unavailable: AtomicBool,
    stopping: watch::Sender<bool>,
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

    /// Records that the server stops accepting connections, once the shutdown delay has passed,
    /// so the hub closes its streams (PLATFORM-062).
    pub fn stop(&self) {
        self.state.stopping.send_replace(true);
    }

    /// Completes once [`Readiness::stop`] has been called.
    pub(crate) async fn stopping(&self) {
        let mut stopping = self.state.stopping.subscribe();
        // The sender lives as long as the state, which this borrows, so the wait ends only once
        // `stop` has been called.
        if let Err(error) = stopping.wait_for(|stopping| *stopping).await {
            errors::ignore(&error, "readiness_dropped");
        }
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

/// Serves `app` on `listener` until `signal` completes, then shuts down (PLATFORM-062):
/// readiness turns to `draining`, the server keeps serving for the delay, stops accepting
/// connections and has the hub close the open streams, and waits for the requests in flight
/// until the drain deadline. A request still
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
    // Each request carries its connection's peer, from which its client IP comes (PLATFORM-053).
    let app = app.into_make_service_with_connect_info::<SocketAddr>();
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
    // The streams close, so the drain doesn't wait for them (OPS-06).
    readiness.stop();
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

/// Why the async runtime of a subcommand wasn't built.
#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    /// `TOKIO_WORKER_THREADS` is set, which tokio would read by itself.
    #[error(
        "TOKIO_WORKER_THREADS must be unset: no setting of the API gives the runtime its worker \
         threads"
    )]
    WorkerThreads,
    /// The runtime didn't start.
    #[error(transparent)]
    Build(#[from] io::Error),
}

/// The multi-threaded async runtime that a subcommand runs on, with as many worker threads as
/// tokio gives a runtime without `TOKIO_WORKER_THREADS`. `worker_threads` is that variable's
/// value: tokio reads it by itself for a runtime's worker threads, and panics on one that it
/// can't read, so while the variable is set, to any value, an empty one too, the runtime isn't
/// built (PLATFORM-109).
pub fn runtime(worker_threads: Option<&OsStr>) -> Result<Runtime, RuntimeError> {
    if worker_threads.is_some() {
        return Err(RuntimeError::WorkerThreads);
    }
    Ok(tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?)
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

/// Logs what a subcommand that an operator runs did, through the logging setup: one `INFO`
/// event with the message `subcommand finished`, the subcommand as its area, and what it did,
/// which holds no personal data (OPS-03, OBS-04).
pub fn report_outcome(area: &str, outcome: &dyn std::fmt::Display) {
    tracing::info!(
        { names::STARTUP_AREA } = area,
        { names::STARTUP_OUTCOME } = %outcome,
        "subcommand finished"
    );
}

/// A startup failure as one JSON line with the same message and fields, and its time as a
/// logged line gives it (OBS-02), for a failure before logging is set up. It names the
/// environment when the environment is known.
pub fn startup_failure_line(
    service: Service,
    environment: Option<Environment>,
    area: &str,
    error: &(dyn Error + 'static),
) -> String {
    let mut line = Map::new();
    line.insert("timestamp".to_owned(), telemetry::timestamp().into());
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
    use crate::types::Timestamp;

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

    /// The time that a JSON line carries, which must be one in UTC as `Timestamp::read` takes
    /// one.
    fn time(line: &str) -> Timestamp {
        let value: Value = serde_json::from_str(line).unwrap();
        Timestamp::read(value["timestamp"].as_str().expect("a time")).expect("a time in UTC")
    }

    #[test]
    fn platform_063_an_unspecified_address_is_probed_on_the_loopback_address_of_its_family() {
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
    fn platform_063_an_answer_is_read_as_its_status_code_and_its_body() {
        let answer = b"HTTP/1.1 503 Service Unavailable\r\ncontent-length: 21\r\n\r\n{\"status\":\"draining\"}";

        assert_eq!(
            parse_answer(answer),
            Some((503, &b"{\"status\":\"draining\"}"[..]))
        );
        assert_eq!(parse_answer(b"SSH-2.0-OpenSSH_9.9\r\n\r\n"), None);
        assert_eq!(parse_answer(b"HTTP/1.1 200 OK\r\n"), None);
    }

    #[test]
    fn platform_063_a_connection_that_times_out_names_an_address_that_didn_t_answer_in_time() {
        let address: SocketAddr = "127.0.0.1:8080".parse().unwrap();

        let timed_out = not_connected(address, io::ErrorKind::TimedOut.into());
        let refused = not_connected(address, io::ErrorKind::ConnectionRefused.into());

        assert_eq!(
            timed_out.to_string(),
            "127.0.0.1:8080 didn't answer within 3 seconds"
        );
        assert_eq!(refused.to_string(), "can't reach 127.0.0.1:8080");
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
    fn obs_02_a_startup_failure_before_logging_carries_a_time_in_utc_as_a_logged_one_does() {
        let error = BindFailed(io::Error::from(io::ErrorKind::AddrInUse));
        let captured = Captured::default();
        let subscriber = tracing_subscriber::registry().with(
            telemetry::json(SERVICE, Environment::Production, captured.clone())
                .with_filter(LevelFilter::INFO),
        );

        let before = startup_failure_line(SERVICE, None, "listen", &error);
        tracing::subscriber::with_default(subscriber, || {
            report_startup_failure("listen", &error);
        });
        let logged = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();

        // `time` panics on a line without a time in UTC.
        time(&before);
        time(logged.trim_end());
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

    /// A value of `TOKIO_WORKER_THREADS` that isn't Unicode.
    fn not_unicode() -> std::ffi::OsString {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            std::ffi::OsString::from_vec(vec![b'4', 0x80])
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            std::ffi::OsString::from_wide(&[u16::from(b'4'), 0xD800])
        }
    }

    #[test]
    fn platform_109_no_runtime_is_built_while_tokio_s_variable_of_worker_threads_is_set() {
        // A count that tokio takes, values that it panics on, and an empty one.
        let set = ["4", "0", "x", ""].map(std::ffi::OsString::from);

        for value in set.into_iter().chain([not_unicode()]) {
            let built = runtime(Some(&value));

            assert!(
                matches!(built, Err(RuntimeError::WorkerThreads)),
                "{value:?}: {built:?}"
            );
        }
    }

    #[test]
    fn platform_109_without_the_variable_the_runtime_is_built() {
        // A runtime that tokio's builder builds with no count of worker threads given.
        let tokio_s = tokio::runtime::Builder::new_multi_thread().build().unwrap();

        let built = runtime(None).unwrap();

        assert_eq!(
            built.metrics().num_workers(),
            tokio_s.metrics().num_workers()
        );
    }
}
