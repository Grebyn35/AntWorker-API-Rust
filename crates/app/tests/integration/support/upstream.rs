//! A stand-in of another system's HTTP endpoints, such as an OAuth provider's, on a loopback
//! address, which the configured client reaches through `OUTBOUND_TEST_ORIGIN` (SEC-10). A test
//! sets what each method and path answers, and reads the requests it received.

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

#[derive(Default)]
struct Answers {
    by_route: HashMap<(String, String), (u16, Value)>,
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
        self.lock()
            .by_route
            .insert((method.to_owned(), path.to_owned()), (status, body));
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
    let mut answers = answers.lock().unwrap_or_else(PoisonError::into_inner);
    answers.received.push(Received {
        method: method.to_string(),
        path: uri.path().to_owned(),
        authorization: headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
        body: String::from_utf8_lossy(&body).into_owned(),
    });
    match answers
        .by_route
        .get(&(method.to_string(), uri.path().to_owned()))
    {
        Some((status, body)) => (
            StatusCode::from_u16(*status).unwrap(),
            [(header::CONTENT_TYPE, "application/json")],
            body.to_string(),
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}
