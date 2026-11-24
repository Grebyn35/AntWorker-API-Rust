//! A stand-in of another system's HTTP endpoints, such as an OAuth provider's, on a loopback
//! address, which the configured client reaches through `OUTBOUND_TEST_ORIGIN` (SEC-10). A test
//! sets what each method and path answers, and reads the requests it received.
// part: files
//! The untrusted client accepts that origin too, so the stand-in also serves what a provider
//! names by an address, such as a user's picture.
// end part: files
// part: client_telemetry
//! It also stands in for the collector's receiver of clients' batches, which the telemetry proxy
//! reaches through `CLIENT_TELEMETRY_ENDPOINT`.
// end part: client_telemetry

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use platform::errors::Held;
use serde_json::Value;
use tokio::sync::Notify;

use super::api::Options;

/// A request that the stand-in received.
#[derive(Clone, Debug)]
pub struct Received {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub body: String,
    // part: client_telemetry
    /// Each header of the request, its name in lowercase, and the body's bytes as they came,
    /// which the tests of a forwarded batch read.
    pub headers: Vec<(String, String)>,
    pub bytes: Vec<u8>,
    // end part: client_telemetry
}

impl Received {
    /// The value of `name` in the request's body, as an HTML form encodes it.
    pub fn field(&self, name: &str) -> Option<String> {
        url::form_urlencoded::parse(self.body.as_bytes())
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.into_owned())
    }
}

/// What a route answers: a status with a JSON body, at once or once the test releases it, or
/// with bytes, or nothing, ever.
enum Answer {
    Json(u16, Value),
    Held(Arc<Notify>, u16, Value),
    // part: files
    Bytes(u16, Vec<u8>),
    // end part: files
    // part: client_telemetry
    Redirect(String),
    // end part: client_telemetry
    Never,
}

#[derive(Default)]
struct Answers {
    by_route: HashMap<(String, String), Answer>,
    received: Vec<Received>,
}

/// The stand-in, which answers until the test ends.
pub struct Upstream {
    origin: String,
    /// What each route answers, and the requests received. A lock that a panic poisoned still
    /// holds whole answers and requests, since each change of them is one insert or one push, so
    /// it is taken as it is.
    answers: Arc<Mutex<Answers>>,
}

impl Upstream {
    /// Starts a stand-in that answers 404 until a test sets an answer.
    #[expect(
        clippy::disallowed_methods,
        reason = "the stand-in serves another system's endpoints, which no route of the API's \
                  builders describes"
    )]
    pub async fn start() -> Self {
        let answers = Arc::new(Mutex::new(Answers::default()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let app = Router::new().fallback(answer).with_state(answers.clone());
        tokio::spawn(async move { axum::serve(listener, app).await });
        Self { origin, answers }
    }

    /// The options of an API whose configured client sends every request here (SEC-10).
    pub fn options(&self) -> Options {
        Options::default().set("OUTBOUND_TEST_ORIGIN", &self.origin)
    }

    // part: client_telemetry
    /// The options of an API whose telemetry proxy forwards here, as to the collector's receiver
    /// of clients' batches.
    pub fn collector_options(&self) -> Options {
        Options::default().set("CLIENT_TELEMETRY_ENDPOINT", &self.origin)
    }

    /// The endpoint of a relay that forwards here, as the API reads it from the setting of
    /// [`Self::collector_options`].
    pub fn collector_endpoint(&self) -> platform::telemetry::OtlpEndpoint {
        self.origin.parse().unwrap()
    }
    // end part: client_telemetry

    /// Makes `method` of `path` answer `status`, with `body` as JSON.
    pub fn set(&self, method: &str, path: &str, status: u16, body: Value) {
        self.lock().by_route.insert(
            (method.to_owned(), path.to_owned()),
            Answer::Json(status, body),
        );
    }

    // part: files
    /// Makes `GET` of `path` answer `status`, with `body` as its bytes, such as a picture's.
    pub fn set_bytes(&self, path: &str, status: u16, body: &[u8]) {
        self.lock().by_route.insert(
            ("GET".to_owned(), path.to_owned()),
            Answer::Bytes(status, body.to_vec()),
        );
    }

    /// The URL of `path` here, as another system would give it, such as the address of a
    /// user's picture.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }
    // end part: files

    // part: client_telemetry
    /// Makes `method` of `path` answer 307, a redirect to `location`, which keeps the method and
    /// the body.
    pub fn redirect(&self, method: &str, path: &str, location: &str) {
        self.lock().by_route.insert(
            (method.to_owned(), path.to_owned()),
            Answer::Redirect(location.to_owned()),
        );
    }
    // end part: client_telemetry

    /// Makes `method` of `path` never answer, as a system that hangs.
    pub fn hang(&self, method: &str, path: &str) {
        self.lock()
            .by_route
            .insert((method.to_owned(), path.to_owned()), Answer::Never);
    }

    /// Makes `method` of `path` answer `status`, with `body` as JSON, as a system that answers
    /// late: a request waits until the test calls `notify_one` on the returned gate, which
    /// releases one request that waits, or, when none does, the next to come.
    pub fn hold(&self, method: &str, path: &str, status: u16, body: Value) -> Arc<Notify> {
        let gate = Arc::new(Notify::new());
        self.lock().by_route.insert(
            (method.to_owned(), path.to_owned()),
            Answer::Held(gate.clone(), status, body),
        );
        gate
    }

    /// Every request received so far, in order.
    pub fn received(&self) -> Vec<Received> {
        self.lock().received.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Answers> {
        self.answers.lock().held()
    }
}

async fn answer(
    State(answers): State<Arc<Mutex<Answers>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let answer = {
        let mut answers = answers.lock().held();
        record(&mut answers, &method, &uri, &headers, &body)
    };
    match answer {
        Some(Answer::Json(status, body)) => json(status, &body),
        Some(Answer::Held(gate, status, body)) => {
            gate.notified().await;
            json(status, &body)
        }
        // part: files
        Some(Answer::Bytes(status, body)) => (
            StatusCode::from_u16(status).unwrap(),
            [(header::CONTENT_TYPE, "application/octet-stream")],
            body,
        )
            .into_response(),
        // end part: files
        // part: client_telemetry
        Some(Answer::Redirect(location)) => (
            StatusCode::TEMPORARY_REDIRECT,
            [(header::LOCATION, location)],
        )
            .into_response(),
        // end part: client_telemetry
        Some(Answer::Never) => std::future::pending().await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// An answer of `status`, with `body` as JSON.
fn json(status: u16, body: &Value) -> Response {
    (
        StatusCode::from_u16(status).unwrap(),
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// Records the request, and gives what its route answers.
fn record(
    answers: &mut Answers,
    method: &Method,
    uri: &Uri,
    headers: &HeaderMap,
    body: &Bytes,
) -> Option<Answer> {
    answers.received.push(Received {
        method: method.to_string(),
        path: uri.path().to_owned(),
        authorization: headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        body: String::from_utf8_lossy(body).into_owned(),
        // part: client_telemetry
        headers: headers
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_owned(),
                    String::from_utf8_lossy(value.as_bytes()).into_owned(),
                )
            })
            .collect(),
        bytes: body.to_vec(),
        // end part: client_telemetry
    });
    match answers
        .by_route
        .get(&(method.to_string(), uri.path().to_owned()))
    {
        Some(Answer::Json(status, body)) => Some(Answer::Json(*status, body.clone())),
        Some(Answer::Held(gate, status, body)) => {
            Some(Answer::Held(gate.clone(), *status, body.clone()))
        }
        // part: files
        Some(Answer::Bytes(status, body)) => Some(Answer::Bytes(*status, body.clone())),
        // end part: files
        // part: client_telemetry
        Some(Answer::Redirect(location)) => Some(Answer::Redirect(location.clone())),
        // end part: client_telemetry
        Some(Answer::Never) => Some(Answer::Never),
        None => None,
    }
}
