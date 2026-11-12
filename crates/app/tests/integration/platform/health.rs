//! The health probes (OPS-05).

use std::{
    io::Write,
    net::{SocketAddr, TcpListener},
    process::Output,
    time::Duration,
};

use http::StatusCode;
use serde_json::{Value, json};

use crate::support::{Api, TestDatabase, http_request, run_binary};

/// How often the instance checks the listener.
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

#[tokio::test]
async fn platform_038_the_live_probe_answers_alive() {
    let api = Api::new().await;

    let response = api.get("/health/live").send().await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json(), json!({"status": "alive"}));
}

#[tokio::test]
async fn platform_061_a_draining_instance_is_not_ready() {
    let api = Api::new().await;
    api.readiness().drain();

    let response = api.get("/health/ready").send().await;

    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.json(), json!({"status": "draining"}));
}

#[tokio::test]
async fn platform_061_readiness_follows_the_instance_s_own_database_connection() {
    let api = Api::new().await;
    let database = api.database();
    let clock = api.clock();
    clock.sleeping(CHECK_INTERVAL).await;
    let ready = api.get("/health/ready").send().await;

    // Nothing can connect as app any more, and its open connections end.
    database.cut_off_app().await;
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_INTERVAL).await;
    let unavailable = api.get("/health/ready").send().await;

    database
        .admin_executes(&format!(
            "GRANT CONNECT ON DATABASE \"{}\" TO app",
            database.name()
        ))
        .await;
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_INTERVAL).await;
    let recovered = api.get("/health/ready").send().await;

    assert_eq!(ready.status, StatusCode::OK);
    assert_eq!(ready.json(), json!({"status": "ready"}));
    assert_eq!(unavailable.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        unavailable.json(),
        json!({"status": "database_unavailable"})
    );
    assert_eq!(recovered.status, StatusCode::OK);
}

#[tokio::test]
async fn platform_061_a_pool_with_every_connection_in_use_stays_ready() {
    let api = Api::new().await;
    let clock = api.clock();
    let pool = api.pool();
    clock.sleeping(CHECK_INTERVAL).await;

    let mut held = Vec::new();
    for _ in 0..TestDatabase::POOL_SIZE {
        held.push(pool.begin(&crate::support::client()).await.unwrap());
    }
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_INTERVAL).await;
    let response = api.get("/health/ready").send().await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json(), json!({"status": "ready"}));
    drop(held);
}

/// Runs `health` against `address`, with no other setting.
async fn health(address: SocketAddr) -> Output {
    run_binary(&["health"], &[("LISTEN_ADDR", &address.to_string())]).await
}

/// Waits until the instance at `address` answers its readiness probe with 200: it has built its
/// services, which read the database as they start, after the line that says it listens, and its
/// hub reaches the database. A test that cuts the database off waits for it first, so the cut
/// meets the readiness it tests, not the startup (TEST-06).
async fn ready(address: SocketAddr) {
    loop {
        if let Ok(response) = http_request(address, "GET", "/health/ready", &[]).await
            && response.starts_with("HTTP/1.1 200")
        {
            return;
        }
        tokio::task::yield_now().await;
    }
}

/// The one line `health` wrote on stderr, as JSON, after it exited 1.
fn failure(output: &Output) -> Value {
    let stderr = String::from_utf8(output.stderr.clone()).unwrap();
    assert_eq!(output.status.code(), Some(1), "{stderr}");
    assert!(output.stdout.is_empty());
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    let line: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(line["message"], "startup failed");
    assert_eq!(line["startup.area"], "health");
    line
}

#[tokio::test]
async fn platform_063_health_exits_0_while_the_instance_is_ready() {
    let database = TestDatabase::migrated().await;
    let server = database.serve(&[("ENVIRONMENT", "production")]).await;

    let output = health(server.address()).await;

    assert_eq!(
        output.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[tokio::test]
async fn platform_063_health_exits_1_and_names_the_answer_while_the_instance_isn_t_ready() {
    let database = TestDatabase::migrated().await;
    let mut server = database.serve(&[("ENVIRONMENT", "production")]).await;
    ready(server.address()).await;
    // Nothing can connect as app any more, and its open connections end.
    database.cut_off_app().await;
    server
        .line(|line| line["message"] == "the database stopped answering")
        .await;

    let output = health(server.address()).await;

    assert_eq!(
        failure(&output)["exception.message"],
        format!("{} answered 503 database_unavailable", server.address())
    );
}

#[tokio::test]
async fn platform_063_health_exits_1_when_no_instance_listens() {
    let free = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = free.local_addr().unwrap();
    drop(free);

    let output = health(address).await;

    let message = failure(&output)["exception.message"].clone();
    let message = message.as_str().unwrap();
    assert!(
        message.starts_with(&format!("can't reach {address}: ")),
        "{message}"
    );
}

#[tokio::test]
async fn platform_063_health_exits_1_when_the_instance_doesn_t_answer_within_3_seconds() {
    // The system accepts connections into the listener's backlog, and nothing ever answers
    // them, as with a process that has stopped responding.
    let silent = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = silent.local_addr().unwrap();

    let output = health(address).await;

    assert_eq!(
        failure(&output)["exception.message"],
        format!("{address} didn't answer within 3 seconds")
    );
    drop(silent);
}

#[tokio::test]
async fn platform_063_health_exits_1_when_the_answer_isn_t_http() {
    // A server of another protocol on the address, which answers and hangs up.
    let other = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = other.local_addr().unwrap();
    let answered = std::thread::spawn(move || {
        let (mut stream, _) = other.accept().unwrap();
        stream.write_all(b"SSH-2.0-OpenSSH_9.9\r\n").unwrap();
    });

    let output = health(address).await;

    assert_eq!(
        failure(&output)["exception.message"],
        format!("{address} didn't answer with HTTP")
    );
    answered.join().unwrap();
}
