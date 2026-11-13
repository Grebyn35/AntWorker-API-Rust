//! A stand-in of another system's HTTP endpoints, such as an OAuth provider's, on a loopback
//! address, which the configured client reaches through `OUTBOUND_TEST_ORIGIN` (SEC-10). A test
//! sets what each method and path answers, and reads the requests it received.
// part: files
//! The untrusted client accepts that origin too, so the stand-in also serves what a provider
//! names by an address, such as a user's picture.
// end part: files

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use serde_json::Value;

use super::api::Options;

/// A request that the stand-in received.
#[derive(Clone, Debug)]
pub struct Received {
    pub method: String,
    pub path: String,
    pub authorization: Option<String>,
    pub body: String,
}

impl Received {
    /// The value of `name` in the request's body, as an HTML form encodes it.
    pub fn field(&self, name: &str) -> Option<String> {
        url::form_urlencoded::parse(self.body.as_bytes())
            .find(|(field, _)| field == name)
            .map(|(_, value)| value.into_owned())
    }
}

/// What a route answers: a status with a JSON body, or with bytes, or nothing, ever.
enum Answer {
    Json(u16, Value),
    // part: files
    Bytes(u16, Vec<u8>),
    // end part: files
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

    /// Makes `method` of `path` answer `status`, with `body` as JSON.
    pub fn set(&self, method: &str, path: &str, status: u16, body: Value) {
        self.lock().by_route.insert(
            (method.to_owned(), path.to_owned()),
            Answer::Json(status, body),
        );
    }

    // part: files
    /// Makes `method` of `path` answer `status`, with `body` as its bytes, such as a picture's.
    pub fn set_bytes(&self, method: &str, path: &str, status: u16, body: &[u8]) {
        self.lock().by_route.insert(
            (method.to_owned(), path.to_owned()),
            Answer::Bytes(status, body.to_vec()),
        );
    }

    /// The URL of `path` here, as another system would give it, such as the address of a
    /// user's picture.
    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }
    // end part: files

    /// Makes `method` of `path` never answer, as a system that hangs.
    pub fn hang(&self, method: &str, path: &str) {
        self.lock()
            .by_route
            .insert((method.to_owned(), path.to_owned()), Answer::Never);
    }

    /// Every request received so far, in order.
    pub fn received(&self) -> Vec<Received> {
        self.lock().received.clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Answers> {
        self.answers.lock().unwrap_or_else(PoisonError::into_inner)
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
        let mut answers = answers.lock().unwrap_or_else(PoisonError::into_inner);
        record(&mut answers, &method, &uri, &headers, &body)
    };
    match answer {
        Some(Answer::Json(status, body)) => (
            StatusCode::from_u16(status).unwrap(),
            [(header::CONTENT_TYPE, "application/json")],
            body.to_string(),
        )
            .into_response(),
        // part: files
        Some(Answer::Bytes(status, body)) => (
            StatusCode::from_u16(status).unwrap(),
            [(header::CONTENT_TYPE, "application/octet-stream")],
            body,
        )
            .into_response(),
        // end part: files
        Some(Answer::Never) => std::future::pending().await,
        None => StatusCode::NOT_FOUND.into_response(),
    }
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
    });
    match answers
        .by_route
        .get(&(method.to_string(), uri.path().to_owned()))
    {
        Some(Answer::Json(status, body)) => Some(Answer::Json(*status, body.clone())),
        // part: files
        Some(Answer::Bytes(status, body)) => Some(Answer::Bytes(*status, body.clone())),
        // end part: files
        Some(Answer::Never) => Some(Answer::Never),
        None => None,
    }
}
