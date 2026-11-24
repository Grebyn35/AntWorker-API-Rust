//! The logging setup (OBS-02, OBS-05), the answer of a request that the API's own server refuses
//! before the router gets it, which writes no line (PLATFORM-099), and the watch that prints a
//! long test's lines (TEST-06).

use std::{
    io,
    sync::{Arc, Mutex, mpsc},
    time::Duration,
};

use platform::{
    errors::Held,
    telemetry::{
        Service,
        testing::{self, Watch},
    },
};

use crate::support::{self, TestDatabase};

const SERVICE: Service = Service {
    name: "api",
    version: "0.0.0",
};

#[test]
fn obs_05_events_from_other_crates_are_not_written() {
    testing::install(SERVICE).unwrap();

    tracing::info_span!("request").in_scope(|| {
        tracing::warn!(target: "sqlx::query", summary = "SELECT 1", "slow statement");
        tracing::warn!("the tests wrote this");
    });

    let lines = testing::all_logged();
    assert!(!lines.iter().any(|line| line["message"] == "slow statement"));
    assert!(
        lines
            .iter()
            .any(|line| line["message"] == "the tests wrote this")
    );
}

#[test]
fn obs_02_a_line_written_while_another_thread_holds_a_scoped_dispatcher_carries_its_trace_id() {
    testing::install(SERVICE).unwrap();
    let (holding, held) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    // A thread that holds a dispatcher of its own, as a test of spans or of jobs holds one.
    let other = std::thread::spawn(move || {
        let scoped = tracing::subscriber::NoSubscriber::default();
        tracing::subscriber::with_default(scoped, || {
            holding.send(()).unwrap();
            released.recv().unwrap_or_default();
        });
    });
    held.recv().unwrap();

    tracing::info_span!("request").in_scope(|| {
        tracing::warn!("written while another thread holds a dispatcher");
    });

    release.send(()).unwrap();
    other.join().unwrap();
    let line = testing::logged_on_this_thread()
        .into_iter()
        .find(|line| line["message"] == "written while another thread holds a dispatcher")
        .unwrap();
    assert_eq!(line["trace_id"].as_str().map(str::len), Some(32), "{line}");
    assert_eq!(line["span_id"].as_str().map(str::len), Some(16), "{line}");
}

#[tokio::test]
async fn obs_02_a_json_line_carries_the_service_and_its_trace_context() {
    let database = TestDatabase::migrated().await;
    let mut server = database.serve(&[("ENVIRONMENT", "production")]).await;
    let name = support::service_name();

    let response = server
        .request(
            "GET",
            "/v1/capabilities",
            &[(
                "traceparent",
                "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
            )],
        )
        .await;
    let completed = server
        .line(|line| line["message"] == "request completed")
        .await;

    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    for line in [server.listening(), &completed] {
        assert_eq!(line["service.name"], name, "{line}");
        assert_eq!(line["service.version"], env!("CARGO_PKG_VERSION"), "{line}");
        assert_eq!(line["deployment.environment.name"], "production", "{line}");
        // The line's time, in UTC, as RFC 3339 writes it.
        let time = line["timestamp"].as_str().unwrap();
        assert!(time.ends_with('Z'), "{line}");
        assert!(
            time::OffsetDateTime::parse(time, &time::format_description::well_known::Rfc3339)
                .is_ok(),
            "{line}"
        );
    }
    assert!(
        server.listening().get("trace_id").is_none(),
        "a line outside any span carries no trace context"
    );
    assert_eq!(completed["trace_id"], "0af7651916cd43dd8448eb211c80319c");
    assert_eq!(completed["span_id"].as_str().unwrap().len(), 16);
}

/// What a watch printed. A lock that a panic poisoned still holds whole writes, since each change
/// of it is one call that appends a write's bytes, so it is taken as it is.
#[derive(Clone, Default)]
struct Printed(Arc<Mutex<Vec<u8>>>);

impl Printed {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().held()).into_owned()
    }
}

impl io::Write for Printed {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().held().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn test_06_a_watch_prints_its_test_s_lines_then_each_line_it_logs_until_it_ends() {
    testing::install(SERVICE).unwrap();
    let test = std::thread::current().name().unwrap().to_owned();
    tracing::info!("logged before the watch printed");
    std::thread::spawn(|| tracing::info!("logged by another thread"))
        .join()
        .unwrap();
    let printed = Printed::default();

    let watch = Watch::printing_to(Duration::ZERO, printed.clone());
    while !printed.text().contains("logged before the watch printed") {
        std::thread::yield_now();
    }
    tracing::info!("logged while the watch prints");
    drop(watch);
    tracing::info!("logged after the watch ended");

    let text = printed.text();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 3, "{text}");
    assert!(
        lines[0].starts_with(&format!("{test} has run for 0 seconds")),
        "{text}"
    );
    for (line, message) in lines[1..].iter().zip([
        "logged before the watch printed",
        "logged while the watch prints",
    ]) {
        let logged = line.strip_prefix(&format!("{test}: ")).unwrap();
        let logged: serde_json::Value = serde_json::from_str(logged).unwrap();
        assert_eq!(logged["message"], message, "{text}");
    }
}

#[tokio::test]
async fn platform_099_a_request_that_the_server_refuses_before_the_router_gets_its_bare_answer_and_no_line()
 {
    let db = TestDatabase::migrated().await;
    let mut server = db.serve(&[("ENVIRONMENT", "production")]).await;
    // With the two headers that every request of the test support carries, 101 and 100.
    let names: Vec<String> = (0..99).map(|n| format!("x-{n}")).collect();
    let headers = |count: usize| -> Vec<(&str, &str)> {
        names[..count]
            .iter()
            .map(|name| (name.as_str(), "1"))
            .collect()
    };
    let target = |bytes: usize| format!("/{}", "a".repeat(bytes - 1));

    let unread = server
        .request("GET", "/v1/capabilities and more", &[])
        .await;
    let crowded = server
        .request("GET", "/v1/capabilities", &headers(99))
        .await;
    let lengthy = server.request("GET", &target(65_535), &[]).await;
    // The most that the server takes of each reaches the router.
    let served = server
        .request("GET", "/v1/capabilities", &headers(98))
        .await;
    let missing = server.request("GET", &target(65_534), &[]).await;

    for (answer, status) in [
        (&unread, "400 Bad Request"),
        (&crowded, "431 Request Header Fields Too Large"),
        (&lengthy, "414 URI Too Long"),
    ] {
        let (head, body) = answer.split_once("\r\n\r\n").unwrap();
        let mut lines = head.lines();
        assert_eq!(lines.next(), Some(format!("HTTP/1.1 {status}").as_str()));
        let mut named: Vec<&str> = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, _)| name)
            .collect();
        named.sort_unstable();
        assert_eq!(named, ["connection", "content-length", "date"], "{answer}");
        assert_eq!(body, "", "{answer}");
    }
    assert!(served.starts_with("HTTP/1.1 200"), "{served}");
    assert!(missing.starts_with("HTTP/1.1 404"), "{missing}");
    assert!(
        missing.contains("content-type: application/problem+json"),
        "{missing}"
    );
    // The first request that the router answered is the first that the IP limit counted.
    let item = |name: &str, key: &str| -> u32 {
        let header = served
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .expect("a response that the IP limit counted names its bucket");
        let value = header.split(';').find_map(|part| part.strip_prefix(key));
        value.and_then(|value| value.parse().ok()).unwrap()
    };
    assert_eq!(
        item("ratelimit: ", "r=") + 1,
        item("ratelimit-policy: ", "q=")
    );
    // Of the lines that the API wrote up to the last request's, those that hold a request's
    // status are the two of the requests that the router answered. Another line may stand
    // among them, such as one of a periodic job.
    let mut lines = Vec::new();
    while lines
        .iter()
        .filter(|line: &&serde_json::Value| line["message"] == "request completed")
        .count()
        < 2
    {
        lines.push(server.line(|_| true).await);
    }
    let statuses: Vec<&serde_json::Value> = lines
        .iter()
        .filter_map(|line| line.get("http.response.status_code"))
        .collect();
    assert_eq!(statuses, [200, 404], "{lines:?}");
    let requests = lines
        .iter()
        .filter(|line| line.get("http.request.method").is_some())
        .count();
    assert_eq!(requests, 2, "{lines:?}");
}
