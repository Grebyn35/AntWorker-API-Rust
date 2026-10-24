//! The panic hook (ERR-12), in a test binary of its own. The hook replaces the one that prints
//! a failing test's message, so no other test may share its process.

use platform::telemetry::{Service, testing};

#[test]
fn err_12_a_panic_writes_one_error_line_with_its_message_and_location_and_is_counted() {
    testing::install(Service {
        name: "api",
        version: "0.0.0",
    })
    .unwrap();
    testing::install_panic_hook();

    let caught = std::panic::catch_unwind(|| {
        let _request = tracing::info_span!("request").entered();
        #[expect(clippy::panic, reason = "the test proves the panic hook (ERR-12)")]
        {
            panic!("the test panicked");
        }
    });
    drop(std::panic::take_hook());

    assert!(caught.is_err());
    let lines: Vec<_> = testing::all_logged()
        .into_iter()
        .filter(|line| line["message"] == "panicked")
        .collect();
    assert_eq!(lines.len(), 1, "{lines:#?}");
    let line = &lines[0];
    assert_eq!(line["level"], "ERROR");
    assert_eq!(line["exception.message"], "the test panicked");
    assert!(
        line["code.file.path"]
            .as_str()
            .unwrap()
            .ends_with("panic_hook.rs"),
        "{line}"
    );
    assert!(line["code.line.number"].as_u64().unwrap() > 0);
    assert_eq!(line["trace_id"].as_str().unwrap().len(), 32);
    let panics: Vec<f64> = testing::recorded("platform.panics")
        .into_iter()
        .map(|point| point.value)
        .collect();
    assert_eq!(panics, [1.0]);
}
