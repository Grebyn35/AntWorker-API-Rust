//! The client of the malware scanner (PLATFORM-077), against the clamd that `just services`
//! starts and stand-ins for it.

use std::{sync::Arc, time::Duration};

use platform::{
    clock::TestClock,
    scanner::{ScanError, Verdict},
    telemetry::testing,
};

use crate::support::scanner::{self, Script};

/// How many scans ended as `outcome`, in every test that runs alongside.
fn scans(outcome: &str) -> f64 {
    testing::recorded("platform.scans")
        .into_iter()
        .filter(|point| point.labels.get("scanner.outcome").map(String::as_str) == Some(outcome))
        .map(|point| point.value)
        .sum()
}

#[tokio::test]
async fn platform_077_a_scan_is_clean_or_names_the_signature_it_found() {
    let client = scanner::scanner();
    let (clean, found) = (scans("clean"), scans("found"));

    let harmless = client.scan(b"hello").await.unwrap();
    let infected = client.scan(&scanner::eicar()).await.unwrap();

    assert_eq!(harmless, Verdict::Clean);
    assert_eq!(
        infected,
        Verdict::Found(scanner::EICAR_SIGNATURE.to_owned())
    );
    // Tests running alongside scan too, so the counts grow by at least these.
    assert!(scans("clean") >= clean + 1.0);
    assert!(scans("found") >= found + 1.0);
}

#[tokio::test]
async fn platform_077_clamd_takes_20_mib_and_fails_a_longer_stream() {
    let client = scanner::scanner();
    let largest = 20 * 1024 * 1024;
    let failed = scans("failed");

    let taken = client.scan(&vec![b'a'; largest]).await;
    let refused = client.scan(&vec![b'a'; largest + 1]).await;

    assert_eq!(taken.unwrap(), Verdict::Clean);
    let error = refused.unwrap_err();
    assert!(matches!(error, ScanError::Failed(_)), "{error:?}");
    let cause = format!("{:?}", std::error::Error::source(&error));
    assert!(cause.contains("size limit exceeded"), "{cause}");
    assert!(scans("failed") >= failed + 1.0);
}

#[tokio::test]
async fn platform_077_a_scanner_it_can_t_connect_to_is_unavailable() {
    // Nothing listens on port 9 of the loopback address.
    let client = scanner::client("127.0.0.1:9", Arc::new(TestClock::default()));
    let unavailable = scans("unavailable");

    let error = client.scan(b"hello").await.unwrap_err();

    assert!(matches!(error, ScanError::Unavailable(_)), "{error:?}");
    assert!(scans("unavailable") >= unavailable + 1.0);
}

#[tokio::test]
async fn platform_077_a_scanner_that_doesn_t_answer_within_30_seconds_is_unavailable() {
    let stand_in = scanner::scripted(Script::Never).await;
    let clock = Arc::new(TestClock::default());
    let client = scanner::client(&stand_in.address, clock.clone());

    let (scanned, ()) = tokio::join!(client.scan(b"hello"), async {
        stand_in.read.notified().await;
        clock.sleeping(Duration::from_secs(30)).await;
        clock.advance(Duration::from_secs(30));
    });

    let error = scanned.unwrap_err();
    assert!(matches!(error, ScanError::Unavailable(_)), "{error:?}");
}

#[tokio::test]
async fn platform_077_a_scanner_that_closes_the_connection_before_its_answer_is_unavailable() {
    let stand_in = scanner::scripted(Script::Close).await;
    let client = scanner::client(&stand_in.address, Arc::new(TestClock::default()));

    let error = client.scan(b"hello").await.unwrap_err();

    assert!(matches!(error, ScanError::Unavailable(_)), "{error:?}");
}

#[tokio::test]
async fn platform_077_an_answer_that_is_an_error_or_unreadable_fails_the_scan() {
    for line in ["Can't allocate memory ERROR", "PONG"] {
        let stand_in = scanner::scripted(Script::Answer(line)).await;
        let client = scanner::client(&stand_in.address, Arc::new(TestClock::default()));

        let error = client.scan(b"hello").await.unwrap_err();

        assert!(matches!(error, ScanError::Failed(_)), "{line}: {error:?}");
    }
}
