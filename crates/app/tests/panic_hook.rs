//! The panic hook that telemetry installs (ERR-12), in a test binary of its own. Telemetry is the
//! process's, and its hook replaces the one that prints a failing test's message, so no other
//! test may share its process.

use platform::{
    config::{Environment, LogLevel, TelemetryConfig},
    telemetry::{
        self, Service,
        testing::collector::{Collector, attribute, text},
    },
};

#[test]
fn err_12_a_panic_on_any_thread_writes_one_error_record_and_is_counted() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let collector = runtime.block_on(Collector::start()).unwrap();
    let config = TelemetryConfig {
        environment: Environment::Production,
        level: LogLevel::Info,
        otlp_endpoint: Some(collector.endpoint().parse().unwrap()),
    };
    let service = Service {
        name: "api",
        version: "0.0.0",
    };
    let installed = telemetry::init(service, &config).unwrap();

    let panicked = std::thread::spawn(|| {
        let _request = tracing::info_span!("request").entered();
        #[expect(clippy::panic, reason = "the test proves the panic hook (ERR-12)")]
        {
            panic!("the test panicked");
        }
    })
    .join();
    drop(std::panic::take_hook());
    installed.finish();

    assert!(panicked.is_err());
    let received = collector.received();
    let records: Vec<_> = received
        .records()
        .into_iter()
        .filter(|(_, record)| record.body.as_ref().and_then(text).as_deref() == Some("panicked"))
        .collect();
    assert_eq!(records.len(), 1, "{records:#?}");
    let (_, record) = records[0];
    assert_eq!(record.severity_text, "ERROR");
    assert_eq!(
        attribute(&record.attributes, "exception.message").as_deref(),
        Some("the test panicked")
    );
    assert!(
        attribute(&record.attributes, "code.file.path")
            .unwrap()
            .ends_with("panic_hook.rs")
    );
    let line: u32 = attribute(&record.attributes, "code.line.number")
        .unwrap()
        .parse()
        .unwrap();
    assert!(line > 0);
    assert_eq!(
        record.trace_id.len(),
        16,
        "the record carries its span's trace"
    );
    assert_eq!(received.count("platform.panics", None), 1);
}
