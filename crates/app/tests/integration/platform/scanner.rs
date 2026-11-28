//! The client of the malware scanner (PLATFORM-079), against the clamd that `just services`
//! starts and stand-ins for it.

use std::{sync::Arc, time::Duration};

use platform::{
    clock::TestClock,
    scanner::{ScanError, Verdict},
    telemetry::testing::{collector::Collector, spans::EndedSpans},
};

use crate::{
    files::upload_to,
    support::{
        Api, counts, files,
        scanner::{self, Script},
        users, workspaces,
    },
};

/// The answers to Ada's uploads of `uploads`, each a name and its bytes, to a workspace of hers,
/// through a `serve` whose scanner is the one at `address`, and the scans that it counted, by how
/// each ended, such as `clean 1`, once it stopped (TEST-07).
async fn scanned(address: &str, uploads: &[(&str, &[u8])]) -> (Vec<u16>, Vec<String>) {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;
    let collector = Collector::start().await.unwrap();
    let server = files::serving(api.database(), &collector, &[("CLAMD_ADDRESS", address)]).await;
    let mut answers = Vec::new();
    for (name, bytes) in uploads {
        let (status, _) = upload_to(&server, &ada, &workspace, name, bytes, None).await;
        answers.push(status);
    }
    server.stop().await;
    let counted = counts::of(&collector.received(), "platform.scans", "scanner.outcome");
    (answers, counted)
}

#[tokio::test]
async fn platform_079_a_scan_is_clean_or_names_the_signature_it_found() {
    let client = scanner::scanner();

    let harmless = client.scan(b"hello").await.unwrap();
    let infected = client.scan(&scanner::eicar()).await.unwrap();

    assert_eq!(harmless, Verdict::Clean);
    assert_eq!(
        infected,
        Verdict::Found(scanner::EICAR_SIGNATURE.to_owned())
    );
}

#[tokio::test]
async fn platform_079_a_clean_scan_and_one_that_finds_a_signature_count_by_their_verdicts() {
    let eicar = scanner::eicar();

    // Each verdict a number of times of its own, so that counts which gave a verdict the other's
    // label differ from these.
    let (answers, counted) = scanned(
        scanner::ADDRESS,
        &[
            ("a.txt", b"hello".as_slice()),
            ("eicar.txt", &eicar),
            ("eicar.txt", &eicar),
        ],
    )
    .await;

    // Stored, then refused for the signature, twice (FILES-005).
    assert_eq!(answers, [201, 422, 422]);
    assert_eq!(counted, ["clean 1", "found 2"]);
}

#[tokio::test]
async fn platform_079_clamd_takes_20_mib_and_fails_a_longer_stream() {
    let client = scanner::scanner();
    let largest = 20 * 1024 * 1024;

    let taken = client.scan(&vec![b'a'; largest]).await;
    let refused = client.scan(&vec![b'a'; largest + 1]).await;

    assert_eq!(taken.unwrap(), Verdict::Clean);
    let error = refused.unwrap_err();
    assert!(matches!(error, ScanError::Failed(_)), "{error:?}");
    let cause = format!("{:?}", std::error::Error::source(&error));
    assert!(cause.contains("size limit exceeded"), "{cause}");
}

#[tokio::test]
async fn platform_079_a_scan_that_clamd_answers_with_an_error_counts_as_failed() {
    // What clamd answers a stream longer than it takes, which no upload is (FILES-004).
    let stand_in = scanner::scripted(Script::Answer("INSTREAM size limit exceeded. ERROR")).await;

    let (answers, counted) = scanned(&stand_in.address, &[("a.txt", b"hello")]).await;

    assert_eq!(answers, [500]);
    assert_eq!(counted, ["failed 1"]);
}

#[tokio::test]
async fn platform_079_a_scanner_it_can_t_connect_to_is_unavailable() {
    // Nothing listens on port 9 of the loopback address.
    let client = scanner::client("127.0.0.1:9", Arc::new(TestClock::default()));

    let error = client.scan(b"hello").await.unwrap_err();

    assert!(matches!(error, ScanError::Unavailable(_)), "{error:?}");
}

#[tokio::test]
async fn platform_079_a_scan_that_can_t_connect_counts_as_unavailable() {
    // Nothing listens on port 9 of the loopback address.
    let (answers, counted) = scanned("127.0.0.1:9", &[("a.txt", b"hello")]).await;

    assert_eq!(answers, [503]);
    assert_eq!(counted, ["unavailable 1"]);
}

#[tokio::test]
async fn platform_079_a_scanner_that_doesn_t_answer_within_30_seconds_is_unavailable() {
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
async fn platform_079_a_scanner_that_closes_the_connection_before_its_answer_is_unavailable() {
    let stand_in = scanner::scripted(Script::Close).await;
    let client = scanner::client(&stand_in.address, Arc::new(TestClock::default()));

    let error = client.scan(b"hello").await.unwrap_err();

    assert!(matches!(error, ScanError::Unavailable(_)), "{error:?}");
}

#[tokio::test]
async fn platform_079_an_answer_that_is_an_error_or_unreadable_fails_the_scan() {
    for line in ["Can't allocate memory ERROR", "PONG"] {
        let stand_in = scanner::scripted(Script::Answer(line)).await;
        let client = scanner::client(&stand_in.address, Arc::new(TestClock::default()));

        let error = client.scan(b"hello").await.unwrap_err();

        assert!(matches!(error, ScanError::Failed(_)), "{line}: {error:?}");
    }
}

#[tokio::test]
async fn obs_06_a_scan_runs_in_a_client_span_with_the_scanner_s_address_and_port() {
    let stand_in = scanner::scripted(Script::Answer("stream: OK")).await;
    let client = scanner::client(&stand_in.address, Arc::new(TestClock::default()));
    let spans = EndedSpans::default();
    let on_this_thread = tracing::dispatcher::set_default(&spans.dispatch());

    let scanned = client.scan(b"hello").await;
    drop(on_this_thread);

    assert_eq!(scanned.unwrap(), Verdict::Clean);
    let (host, port) = stand_in.address.rsplit_once(':').unwrap();
    let read = |key: &str| spans.attributes("clamd INSTREAM", key);
    assert_eq!(
        [read("server.address"), read("server.port")],
        [[Some(host.to_owned())], [Some(port.to_owned())]]
    );
}
