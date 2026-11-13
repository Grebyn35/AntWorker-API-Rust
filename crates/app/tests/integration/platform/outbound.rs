//! The untrusted client of `platform::outbound`, against a server on a loopback address, the
//! tests' origin (SEC-10).

use platform::outbound::{FetchError, OutboundConfig, Untrusted};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

/// A server on a loopback address, the tests' origin, that answers each request with what
/// `answer` gives for its path, or never, when it gives none; and a client that accepts it.
async fn serving(answer: impl Fn(&str) -> Option<Vec<u8>> + Send + 'static) -> (Untrusted, String) {
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
                    stream.write_all(&response).await.unwrap_or_default();
                    stream.shutdown().await.unwrap_or_default();
                }
                None => std::future::pending().await,
            }
        }
    });
    let config = OutboundConfig {
        test_origin: Some(url::Url::parse(&origin).unwrap()),
    };
    (Untrusted::new(&config).unwrap(), origin)
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
