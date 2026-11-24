//! Ignored errors (ERR-11).

use axum::response::IntoResponse;
use bytes::Bytes;
use futures_util::stream;
use platform::{
    http::Download,
    ranges::Served,
    telemetry::{Service, testing},
};

// part: files
use platform::telemetry::testing::collector::Collector;

use crate::support::{Api, bucket, counts, files, jobs, users, workspaces};
// end part: files

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

#[tokio::test]
async fn platform_100_bytes_that_fail_once_the_status_is_sent_are_written_as_ignored() {
    testing::install(Service {
        name: "api",
        version: "0.0.0",
    })
    .unwrap();
    let bytes = stream::iter([
        Ok(Bytes::from_static(b"hello")),
        Err(std::io::Error::other("the bucket stopped answering")),
    ]);
    let download = Download {
        media_type: "text/plain".to_owned(),
        size: 11,
        served: Served::Whole,
        // part: files
        inline: false,
        // end part: files
        name: "a.txt".to_owned(),
        tag: "a-tag".to_owned(),
        // part: files
        public_for: None,
        // end part: files
        bytes: Some(bytes),
    };
    // The request's span, which ends once its handler returned the response.
    let request = tracing::info_span!("request");
    let trace_id = request.in_scope(platform::telemetry::trace_id);

    let response = request.in_scope(|| download.into_response());
    drop(request);
    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), usize::MAX).await;

    assert_eq!(status, 200);
    assert!(body.is_err(), "the body ended as one that held every byte");
    let lines: Vec<_> = testing::all_logged()
        .into_iter()
        .filter(|line| line["error.type"] == "download_cut" && line["trace_id"] == trace_id)
        .collect();
    assert_eq!(lines.len(), 1, "{lines:#?}");
    assert_eq!(lines[0]["level"], "WARN");
    assert_eq!(
        lines[0]["exception.message"],
        "the bucket stopped answering"
    );
    // `ignore` counts each line that it writes, which
    // `err_11_an_ignored_error_is_counted_by_its_kind` shows with a kind of its own (TEST-07).
}

// part: files
#[tokio::test]
async fn platform_100_a_download_that_the_bucket_cuts_short_counts_once_as_ignored() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(db, "Acme", &[(&ada.user_id, "owner")]).await;
    let id = files::arrange_row(db, &workspace, "Blue").await;
    jobs::not_due(db).await;
    let stand_in = bucket::cutting().await;
    let collector = Collector::start().await.unwrap();
    let env = [("S3_ENDPOINT", stand_in.endpoint.as_str())];
    let server = files::serving(db, &collector, &env).await;
    let bearer = format!("Bearer {}", ada.access);
    let content = format!("/v1/workspaces/{workspace}/files/{id}/content");

    let answer = server
        .request("GET", &content, &[("authorization", &bearer)])
        .await;
    server.stop().await;

    // The status and the first half of the bytes, which the bucket sent before it stopped.
    assert!(answer.starts_with("HTTP/1.1 200"), "{answer}");
    assert!(answer.ends_with("\r\n\r\nhe"), "{answer}");
    assert_eq!(
        counts::of(
            &collector.received(),
            "platform.errors_ignored",
            "error.type"
        ),
        ["download_cut 1"]
    );
}
// end part: files
