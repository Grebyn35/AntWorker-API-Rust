//! Ignored errors (ERR-11).

use platform::telemetry::{Service, testing};

#[test]
fn err_11_an_ignored_error_writes_one_warn_line_with_its_kind_and_cause() {
    testing::install(Service {
        name: "api",
        version: "0.0.0",
    })
    .unwrap();
    let span = tracing::info_span!("work");

    span.in_scope(|| {
        platform::errors::ignore(&std::io::Error::other("the disk is gone"), "test_kind");
    });

    let lines: Vec<_> = testing::all_logged()
        .into_iter()
        .filter(|line| line["error.type"] == "test_kind")
        .collect();
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert_eq!(lines[0]["level"], "WARN");
    assert_eq!(lines[0]["message"], "error ignored");
    assert_eq!(lines[0]["exception.message"], "the disk is gone");
    assert_eq!(lines[0]["trace_id"].as_str().unwrap().len(), 32);
}

#[test]
fn err_11_an_ignored_error_is_counted_by_its_kind() {
    testing::install(Service {
        name: "api",
        version: "0.0.0",
    })
    .unwrap();

    for _ in 0..2 {
        platform::errors::ignore(&std::io::Error::other("the disk is gone"), "counted_kind");
    }

    let counted: Vec<f64> = testing::recorded("platform.errors_ignored")
        .into_iter()
        .filter(|point| point.labels.get("error.type").map(String::as_str) == Some("counted_kind"))
        .map(|point| point.value)
        .collect();
    assert_eq!(counted, [2.0]);
}
