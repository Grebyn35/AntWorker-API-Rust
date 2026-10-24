//! The bounded shutdown (OPS-06), through `platform::lifecycle::serve` with a test clock.

use std::{io, net::SocketAddr, sync::Arc, time::Duration};

use platform::{
    clock::TestClock,
    http::Pipeline,
    lifecycle::{self, Readiness, Shutdown},
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

use crate::support::{
    fixtures::{self, Gate},
    http_request,
};

const DELAY: Duration = Duration::from_secs(3);
const DRAIN: Duration = Duration::from_secs(7);

/// `serve` running on a free port, until the test sends the signal.
struct Running {
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
    let app = platform::http::app(
        fixtures::routes(gate.clone()),
        Pipeline {
            clock: clock.clone(),
            max_concurrent_requests: 100,
            readiness: readiness.clone(),
        },
    );
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
async fn platform_038_the_server_drains_then_stops() {
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
async fn platform_038_serve_returns_at_the_drain_deadline() {
    let running = start().await;
    let _held = hold(&running).await;

    running.signal.notify_one();
    running.clock.sleeping(DELAY).await;
    running.clock.advance(DELAY);
    running.clock.sleeping(DRAIN).await;
    running.clock.advance(DRAIN);

    running.served.await.unwrap().unwrap();
}
