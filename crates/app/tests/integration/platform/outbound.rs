//! The two clients of `platform::outbound`, against a server on a loopback address, the tests'
//! origin (SEC-10).

use std::sync::{Arc, Mutex};

use platform::{
    config::Environment,
    errors::Held,
    outbound::{Configured, FetchError, OutboundConfig, Request, Untrusted},
    telemetry::{self, testing::spans::EndedSpans},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::Notify,
};
use tracing::Instrument;

/// The host that the untrusted client of [`serving`] fetches from, beside the tests' origin: a
/// name that resolves on the machine alone, to a loopback address.
const HOST: &str = "localhost";

/// A server on a loopback address, the tests' origin, that answers each request with what
/// `answer` gives for its path, or never, when it gives none; and the untrusted client, which
/// accepts it, and fetches from [`HOST`] besides.
async fn serving(answer: impl Fn(&str) -> Option<Vec<u8>> + Send + 'static) -> (Untrusted, String) {
    let (config, origin) = origin(answer).await;
    let client = Untrusted::new(&config, vec![HOST.to_owned()]).unwrap();
    (client, origin)
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
    reading(move |head| answer(head.split(' ').nth(1).unwrap_or_default())).await
}

/// A server on a loopback address that answers each request with what `answer` gives for its
/// head, the request line and the headers, or never, when it gives none: its origin, and the
/// settings that make it the tests'.
async fn reading(
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
            match answer(&head) {
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

/// What makes a server answer no request, and what it notifies once it holds one.
fn held() -> (
    Arc<Notify>,
    impl Fn(&str) -> Option<Vec<u8>> + Send + 'static,
) {
    let asked = Arc::new(Notify::new());
    let holds = {
        let asked = asked.clone();
        move |_: &str| {
            asked.notify_one();
            None
        }
    };
    (asked, holds)
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
async fn sec_10_the_untrusted_client_follows_a_redirect_to_one_of_its_hosts_alone() {
    let (client, origin) = serving(|path| {
        Some(match path {
            "/listed" => redirect("https://localhost/ada.png"),
            "/port" => redirect("https://localhost:8443/ada.png"),
            _ => redirect("https://elsewhere.example.com/ada.png"),
        })
    })
    .await;

    let listed = client.get(&format!("{origin}/listed"), 16).await;
    let port = client.get(&format!("{origin}/port"), 16).await;
    let other = client.get(&format!("{origin}/other"), 16).await;

    // The redirect to its host is followed, as far as the host's addresses, which aren't public.
    assert!(
        matches!(&listed, Err(FetchError::Refused(reason)) if reason.contains("no public")),
        "{listed:?}"
    );
    assert!(
        matches!(&port, Err(FetchError::Refused(reason)) if reason.contains("names a port")),
        "{port:?}"
    );
    assert!(
        matches!(&other, Err(FetchError::Refused(reason)) if reason.contains("host is none")),
        "{other:?}"
    );
}

#[tokio::test]
async fn sec_10_the_untrusted_client_reaches_no_name_that_resolves_to_a_private_address() {
    let hosts = vec![HOST.to_owned()];
    let client = Untrusted::new(&OutboundConfig::default(), hosts).unwrap();

    let refused = client.get("https://localhost/ada.png", 16).await;

    assert!(
        matches!(&refused, Err(FetchError::Refused(reason)) if reason.contains("no public")),
        "{refused:?}"
    );
}

#[tokio::test]
async fn sec_10_the_untrusted_client_fetches_from_no_host_that_it_wasn_t_built_with() {
    let client = Untrusted::new(&OutboundConfig::default(), Vec::new()).unwrap();

    let refused = client.get("https://localhost/ada.png", 16).await;

    assert!(
        matches!(&refused, Err(FetchError::Refused(reason)) if reason.contains("host is none")),
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
    let (asked, holds) = held();
    let (client, origin) = serving(holds).await;
    let address = format!("{origin}/ada.png");

    let (failed, ()) = tokio::join!(client.get(&address, 16), async {
        asked.notified().await;
        // Paused once the server holds the request, so tokio's time runs straight to the
        // client's time limit, the only timer left (TEST-06).
        tokio::time::pause();
    });

    assert!(matches!(failed, Err(FetchError::Failed(_))), "{failed:?}");
}

#[tokio::test]
async fn sec_10_the_configured_client_follows_no_redirect() {
    // The path of each request that the server read. A lock that a panic poisoned still holds
    // whole paths, since each change of them is one push, so it is taken as it is.
    let asked = Arc::new(Mutex::new(Vec::new()));
    let paths = asked.clone();
    let client = configured(move |path| {
        paths.lock().held().push(path.to_owned());
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
    let asked = asked.lock().held();
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
    let (asked, holds) = held();
    let client = configured(holds).await;

    let (failed, ()) = tokio::join!(
        client.send(Request::get("https://provider.example.com/token")),
        async {
            asked.notified().await;
            // Paused once the server holds the request, so tokio's time runs straight to the
            // client's time limit, the only timer left (TEST-06).
            tokio::time::pause();
        }
    );

    assert!(failed.is_err(), "{failed:?}");
}

#[tokio::test]
async fn obs_07_neither_client_sends_the_trace_context_to_the_server_it_calls() {
    // The tests' tracer, which gives a span a trace context.
    telemetry::testing::install(crate::support::api::SERVICE).unwrap();
    // The head of each request that the server read, in lowercase. A lock that a panic poisoned
    // still holds whole heads, since each change of them is one push, so it is taken as it is.
    let heads = Arc::new(Mutex::new(Vec::new()));
    let (config, origin) = reading({
        let heads = heads.clone();
        move |head| {
            heads.lock().held().push(head.to_lowercase());
            Some(response("200 OK", &["Content-Length: 4"], b"\x89PNG"))
        }
    })
    .await;
    let configured = Configured::new(Environment::Test, &config).unwrap();
    let untrusted = Untrusted::new(&config, vec![HOST.to_owned()]).unwrap();

    // Each request goes out inside a span, as a request's or a job run's does.
    let (trace, sent, fetched) = async {
        let sent = configured
            .send(Request::get("https://provider.example.com/token"))
            .await;
        let fetched = untrusted.get(&format!("{origin}/ada.png"), 16).await;
        (telemetry::trace_id(), sent, fetched)
    }
    .instrument(tracing::info_span!("work"))
    .await;

    assert!(sent.is_ok(), "{sent:?}");
    assert!(fetched.is_ok(), "{fetched:?}");
    assert_ne!(trace, "00000000000000000000000000000000");
    let heads = heads.lock().held().clone();
    assert_eq!(heads.len(), 2, "{heads:?}");
    for head in &heads {
        for header in ["traceparent", "tracestate", "baggage"] {
            assert!(!head.contains(&format!("\r\n{header}:")), "{head}");
        }
    }
}

#[tokio::test]
async fn obs_06_a_request_of_either_client_runs_in_a_span_named_by_its_method_with_its_server() {
    let (config, origin) =
        origin(|_| Some(response("200 OK", &["Content-Length: 4"], b"\x89PNG"))).await;
    let configured = Configured::new(Environment::Test, &config).unwrap();
    let untrusted = Untrusted::new(&config, vec![HOST.to_owned()]).unwrap();
    let spans = EndedSpans::default();
    let on_this_thread = tracing::dispatcher::set_default(&spans.dispatch());

    let sent = configured
        .send(Request::post("https://provider.example.com/token"))
        .await;
    let fetched = untrusted.get(&format!("{origin}/ada.png"), 16).await;
    drop(on_this_thread);

    assert!(sent.is_ok(), "{sent:?}");
    assert!(fetched.is_ok(), "{fetched:?}");
    // The configured client sends its request to the tests' origin, the server whose address its
    // span names, in place of the URL's host.
    let read = |method: &str| {
        let attribute = |key: &str| spans.attributes(method, key);
        [
            attribute("server.address"),
            attribute("http.response.status_code"),
        ]
    };
    let answered = [
        vec![Some("127.0.0.1".to_owned())],
        vec![Some("200".to_owned())],
    ];
    assert_eq!([read("POST"), read("GET")], [answered.clone(), answered]);
    // Neither span holds the request's path.
    for (method, path) in [("POST", "/token"), ("GET", "/ada.png")] {
        let values = spans.values(method);
        assert!(
            values.iter().flatten().all(|value| !value.contains(path)),
            "{values:?}"
        );
    }
}

#[tokio::test]
async fn obs_06_the_configured_client_s_span_carries_the_status_of_an_answer_over_its_limit_or_cut_short()
 {
    const MEBIBYTE: usize = 1 << 20;
    let client = configured(|path| {
        Some(match path {
            "/over" => response("502 Bad Gateway", &[], &vec![0; MEBIBYTE + 1]),
            // A body shorter than its length, whose connection ends.
            _ => response("503 Service Unavailable", &["Content-Length: 4"], b"ok"),
        })
    })
    .await;
    let spans = EndedSpans::default();
    let on_this_thread = tracing::dispatcher::set_default(&spans.dispatch());

    let over = client
        .send(Request::get("https://provider.example.com/over"))
        .await;
    let cut = client
        .send(Request::get("https://provider.example.com/cut"))
        .await;
    drop(on_this_thread);

    assert!(over.is_err(), "{over:?}");
    assert!(cut.is_err(), "{cut:?}");
    assert_eq!(
        spans.attributes("GET", "http.response.status_code"),
        [Some("502".to_owned()), Some("503".to_owned())]
    );
}

#[tokio::test]
async fn obs_06_the_untrusted_client_s_span_names_the_host_it_was_given_and_the_status_after_its_redirects()
 {
    let (client, origin) = serving(|path| {
        Some(match path {
            "/moved" => redirect("/ada.png"),
            "/plain" => redirect("http://pictures.example.com/ada.png"),
            // One of the client's hosts, which it follows as far as the host's addresses.
            "/listed" => redirect("https://localhost/ada.png"),
            _ => response("200 OK", &["Content-Length: 4"], b"\x89PNG"),
        })
    })
    .await;
    let spans = EndedSpans::default();
    let on_this_thread = tracing::dispatcher::set_default(&spans.dispatch());

    let followed = client.get(&format!("{origin}/moved"), 16).await;
    let refused = client.get(&format!("{origin}/plain"), 16).await;
    let elsewhere = client.get(&format!("{origin}/listed"), 16).await;
    drop(on_this_thread);

    assert!(followed.is_ok(), "{followed:?}");
    assert!(
        matches!(refused, Err(FetchError::Refused(_))),
        "{refused:?}"
    );
    assert!(
        matches!(&elsewhere, Err(FetchError::Refused(reason)) if reason.contains("no public")),
        "{elsewhere:?}"
    );
    let read = |key: &str| spans.attributes("GET", key);
    let address = Some("127.0.0.1".to_owned());
    assert_eq!(
        [read("server.address"), read("http.response.status_code")],
        [
            vec![address.clone(), address.clone(), address],
            vec![Some("200".to_owned()), None, None]
        ]
    );
}
