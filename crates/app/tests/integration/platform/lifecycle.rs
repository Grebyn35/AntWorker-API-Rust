//! The bounded shutdown (OPS-06), through `platform::lifecycle::serve` with a test clock.

use std::{io, net::SocketAddr, sync::Arc, time::Duration};

use platform::{
    clock::TestClock,
    lifecycle::{self, Readiness, Shutdown},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinHandle,
};

use crate::support::{
    TestDatabase,
    api::{Options, application},
    fixtures::{self, Gate},
    http_request,
};

const DELAY: Duration = Duration::from_secs(3);
const DRAIN: Duration = Duration::from_secs(7);

/// `serve` running on a free port, until the test sends the signal.
struct Running {
    database: TestDatabase,
    address: SocketAddr,
    clock: Arc<TestClock>,
    gate: Arc<Gate>,
    signal: Arc<Notify>,
    served: JoinHandle<io::Result<()>>,
}

async fn start() -> Running {
    let gate = Arc::new(Gate::default());
    let clock = Arc::new(TestClock::default());
    let readiness = Readiness::new();
    let database = TestDatabase::migrated().await;
    let (app, _, _, _) = application(
        &database.app().await,
        Options::routes(fixtures::routes(gate.clone())),
        clock.clone(),
        readiness.clone(),
    )
    .await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let signal = Arc::new(Notify::new());
    let received = signal.clone();
    let served = tokio::spawn(lifecycle::serve(
        listener,
        app,
        readiness,
        clock.clone(),
        Shutdown {
            delay: DELAY,
            drain: DRAIN,
        },
        async move { received.notified().await },
    ));
    Running {
        database,
        address,
        clock,
        gate,
        signal,
        served,
    }
}

/// Holds a request in the gate, and returns the task that waits for its response.
async fn hold(running: &Running) -> JoinHandle<io::Result<String>> {
    let entered = running.gate.entered.notified();
    let address = running.address;
    let held = tokio::spawn(http_request(address, "GET", "/v1/gate", &[]));
    entered.await;
    held
}

#[tokio::test]
async fn platform_062_the_server_drains_then_stops() {
    let running = start().await;
    let held = hold(&running).await;

    running.signal.notify_one();
    running.clock.sleeping(DELAY).await;
    let ready = http_request(running.address, "GET", "/health/ready", &[])
        .await
        .unwrap();
    let served_while_draining = http_request(running.address, "GET", "/v1/capabilities", &[])
        .await
        .unwrap();
    running.clock.advance(DELAY);
    running.clock.sleeping(DRAIN).await;
    while tokio::net::TcpStream::connect(running.address)
        .await
        .is_ok()
    {
        tokio::task::yield_now().await;
    }
    running.gate.release.notify_one();
    let finished = held.await.unwrap().unwrap();

    assert!(ready.starts_with("HTTP/1.1 503"), "{ready}");
    assert!(ready.ends_with(r#"{"status":"draining"}"#), "{ready}");
    assert!(
        served_while_draining.starts_with("HTTP/1.1 200"),
        "{served_while_draining}"
    );
    assert!(finished.starts_with("HTTP/1.1 204"), "{finished}");
    running.served.await.unwrap().unwrap();
}

#[tokio::test]
async fn platform_062_serve_returns_at_the_drain_deadline() {
    let running = start().await;
    let _held = hold(&running).await;

    running.signal.notify_one();
    running.clock.sleeping(DELAY).await;
    running.clock.advance(DELAY);
    running.clock.sleeping(DRAIN).await;
    running.clock.advance(DRAIN);

    running.served.await.unwrap().unwrap();
}

/// Registers a user on `address`, over a connection of its own, and returns their access token.
async fn access_token(address: SocketAddr) -> String {
    let body = r#"{"email":"ada@example.com","password":"plum-tractor-41-quiet","name":"Ada"}"#;
    let mut stream = TcpStream::connect(address).await.unwrap();
    let request = format!(
        "POST /v1/users HTTP/1.1\r\nHost: api\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (_, json) = response.split_once("\r\n\r\n").unwrap();
    let json: serde_json::Value = serde_json::from_str(json).unwrap();
    json["access_token"].as_str().unwrap().to_owned()
}

/// Opens the stream of the caller whose access token is `token` on `address`, and returns the
/// connection once its `stream.opened` has arrived.
async fn open_stream(address: SocketAddr, token: &str) -> TcpStream {
    let mut stream = TcpStream::connect(address).await.unwrap();
    let request = format!(
        "GET /v1/users/me/events HTTP/1.1\r\nHost: api\r\nAuthorization: Bearer {token}\r\n\
         Connection: close\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut read = Vec::new();
    while !String::from_utf8_lossy(&read).contains("event: stream.opened") {
        let mut chunk = [0; 1024];
        let n = stream.read(&mut chunk).await.unwrap();
        assert!(n > 0, "{}", String::from_utf8_lossy(&read));
        read.extend_from_slice(&chunk[..n]);
    }
    stream
}

#[tokio::test]
async fn platform_062_once_the_delay_ends_serve_closes_the_open_streams_with_shutdown() {
    let running = start().await;
    let token = access_token(running.address).await;
    // The account proved its address, so its stream opens (AUTH-025).
    running
        .database
        .owner_executes("UPDATE users SET email_verified = true")
        .await;
    let mut stream = open_stream(running.address, &token).await;

    running.signal.notify_one();
    running.clock.sleeping(DELAY).await;
    let mut waiting = [0; 1];
    // Nothing has come, and the connection is still open, while the delay runs.
    let during_delay = stream.try_read(&mut waiting);
    running.clock.advance(DELAY);
    let mut rest = String::new();
    stream.read_to_string(&mut rest).await.unwrap();

    assert!(
        during_delay
            .as_ref()
            .is_err_and(|error| error.kind() == io::ErrorKind::WouldBlock),
        "{during_delay:?}"
    );
    assert!(rest.contains("event: stream.closed"), "{rest}");
    assert!(rest.contains(r#""reason":"shutdown""#), "{rest}");
    running.served.await.unwrap().unwrap();
}
