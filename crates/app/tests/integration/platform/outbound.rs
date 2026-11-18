//! The two clients of `platform::outbound`, against a server on a loopback address, the tests'
//! origin (SEC-10).

use std::sync::{Arc, Mutex, PoisonError};

use platform::{
    config::Environment,
    outbound::{Configured, FetchError, OutboundConfig, Request, Untrusted},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// A server on a loopback address, the tests' origin, that answers each request with what
/// `answer` gives for its path, or never, when it gives none; and the untrusted client, which
/// accepts it.
async fn serving(answer: impl Fn(&str) -> Option<Vec<u8>> + Send + 'static) -> (Untrusted, String) {
    let (config, origin) = origin(answer).await;
    (Untrusted::new(&config).unwrap(), origin)
}

/// The configured client of the test environment, which sends each request to a server that
/// answers it with what `answer` gives for its path, or never, when it gives none.
async fn configured(answer: impl Fn(&str) -> Option<Vec<u8>> + Send + 'static) -> Configured {
    let (config, _) = origin(answer).await;
    Configured::new(Environment::Test, &config).unwrap()
}

/// A server on a loopback address that answers each request with what `answer` gives for its
/// path, or never, when it gives none: its origin, and the settings that make it the tests'.
async fn origin(
    answer: impl Fn(&str) -> Option<Vec<u8>> + Send + 'static,
) -> (OutboundConfig, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                match stream.read(&mut buffer).await {
                    Ok(read) if read > 0 => request.extend_from_slice(&buffer[..read]),
                    _ => break,
                }
            }
            let head = String::from_utf8_lossy(&request).into_owned();
            let path = head.split(' ').nth(1).unwrap_or_default();
            match answer(path) {
                Some(response) => {
                    let sent = async {
                        stream.write_all(&response).await?;
                        stream.shutdown().await
                    };
                    // The client hangs up before the answer ends when it refuses the answer,
                    // as it does a body beyond its cap.
                    if let Err(gone) = sent.await {
                        platform::errors::ignore(&gone, "test_client_gone");
                    }
                }
                None => std::future::pending().await,
            }
        }
    });
    let config = OutboundConfig {
        test_origin: Some(url::Url::parse(&origin).unwrap()),
    };
    (config, origin)
}

/// An answer of `status`, with `headers` and `body`, which ends with its connection.
fn response(status: &str, headers: &[&str], body: &[u8]) -> Vec<u8> {
    let mut head = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
    for header in headers {
        head.push_str(header);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    [head.as_bytes(), body].concat()
}

fn redirect(to: &str) -> Vec<u8> {
    response(
        "302 Found",
        &[&format!("Location: {to}"), "Content-Length: 0"],
        b"",
    )
}

#[tokio::test]
async fn sec_10_the_untrusted_client_reads_the_body_of_an_answer_from_the_tests_origin() {
    let (client, origin) =
        serving(|_| Some(response("200 OK", &["Content-Length: 4"], b"\x89PNG"))).await;

    let fetched = client.get(&format!("{origin}/ada.png"), 16).await.unwrap();

    assert_eq!(fetched.as_ref(), b"\x89PNG");
}

#[tokio::test]
async fn sec_10_the_untrusted_client_caps_an_answer_s_size() {
    let (client, origin) = serving(|path| {
        Some(match path {
            // One that declares its size, and one that doesn't.
            "/declared" => response("200 OK", &["Content-Length: 17"], &[0; 17]),
            _ => response("200 OK", &[], &[0; 17]),
        })
    })
    .await;

    for path in ["/declared", "/undeclared"] {
        let refused = client.get(&format!("{origin}{path}"), 16).await;
        assert!(
            matches!(&refused, Err(FetchError::Refused(reason)) if reason.contains("over 16")),
            "{refused:?}"
        );
        assert!(client.get(&format!("{origin}{path}"), 17).await.is_ok());
    }
}

#[tokio::test]
async fn sec_10_the_untrusted_client_checks_every_redirect_and_follows_three() {
    let (client, origin) = serving(|path| {
        Some(match path {
            "/metadata" => redirect("http://169.254.169.254/latest/meta-data"),
            "/loopback" => redirect("https://127.0.0.1/"),
            "/plain" => redirect("http://pictures.example.com/ada.png"),
            "/4" => redirect("/3"),
            "/3" => redirect("/2"),
            "/2" => redirect("/1"),
            "/1" => redirect("/0"),
            _ => response("200 OK", &["Content-Length: 2"], b"ok"),
        })
    })
    .await;

    for path in ["/metadata", "/loopback", "/plain", "/4"] {
        let refused = client.get(&format!("{origin}{path}"), 16).await;
        assert!(
            matches!(refused, Err(FetchError::Refused(_))),
            "{path}: {refused:?}"
        );
    }
    let followed = client.get(&format!("{origin}/3"), 16).await.unwrap();
    assert_eq!(followed.as_ref(), b"ok");
}

#[tokio::test]
async fn sec_10_the_untrusted_client_reaches_no_name_that_resolves_to_a_private_address() {
    let client = Untrusted::new(&OutboundConfig::default()).unwrap();

    let refused = client.get("https://localhost/ada.png", 16).await;

    assert!(
        matches!(&refused, Err(FetchError::Refused(reason)) if reason.contains("no public")),
        "{refused:?}"
    );
}

#[tokio::test]
async fn sec_10_a_status_says_whether_another_fetch_may_get_the_answer() {
    let (client, origin) = serving(|path| {
        Some(match path {
            "/gone" => response("404 Not Found", &["Content-Length: 0"], b""),
            "/busy" => response("429 Too Many Requests", &["Content-Length: 0"], b""),
            _ => response("503 Service Unavailable", &["Content-Length: 0"], b""),
        })
    })
    .await;

    let gone = client.get(&format!("{origin}/gone"), 16).await;
    let busy = client.get(&format!("{origin}/busy"), 16).await;
    let down = client.get(&format!("{origin}/down"), 16).await;

    assert!(matches!(gone, Err(FetchError::Refused(_))), "{gone:?}");
    assert!(matches!(busy, Err(FetchError::Failed(_))), "{busy:?}");
    assert!(matches!(down, Err(FetchError::Failed(_))), "{down:?}");
}

#[tokio::test]
async fn sec_10_the_untrusted_client_gives_up_an_answer_that_doesn_t_come_in_time() {
    let (client, origin) = serving(|_| None).await;
    // The client's time limit runs on tokio's time, which the runtime advances once the fetch
    // waits for the answer alone (TEST-06).
    tokio::time::pause();

    let failed = client.get(&format!("{origin}/ada.png"), 16).await;

    assert!(matches!(failed, Err(FetchError::Failed(_))), "{failed:?}");
}

#[tokio::test]
async fn sec_10_the_configured_client_follows_no_redirect() {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let paths = asked.clone();
    let client = configured(move |path| {
        paths
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(path.to_owned());
        Some(match path {
            "/token" => redirect("/elsewhere"),
            _ => response("200 OK", &["Content-Length: 2"], b"ok"),
        })
    })
    .await;

    let answer = client
        .send(Request::post("https://provider.example.com/token"))
        .await
        .unwrap();

    assert_eq!(answer.status, 302);
    let asked = asked.lock().unwrap_or_else(PoisonError::into_inner);
    assert_eq!(*asked, ["/token"]);
}

#[tokio::test]
async fn sec_10_the_configured_client_caps_an_answer_s_size() {
    const MEBIBYTE: usize = 1 << 20;
    let client = configured(|path| {
        Some(match path {
            "/over" => response("200 OK", &[], &vec![0; MEBIBYTE + 1]),
            _ => response("200 OK", &[], &vec![0; MEBIBYTE]),
        })
    })
    .await;

    let over = client
        .send(Request::get("https://provider.example.com/over"))
        .await;
    let whole = client
        .send(Request::get("https://provider.example.com/whole"))
        .await;

    assert!(over.is_err(), "{over:?}");
    assert_eq!(whole.unwrap().body.len(), MEBIBYTE);
}

#[tokio::test]
async fn sec_10_the_configured_client_gives_up_an_answer_that_doesn_t_come_in_time() {
    let client = configured(|_| None).await;
    // The client's time limit runs on tokio's time, which the runtime advances once the request
    // waits for the answer alone (TEST-06).
    tokio::time::pause();

    let failed = client
        .send(Request::get("https://provider.example.com/token"))
        .await;

    assert!(failed.is_err(), "{failed:?}");
}
