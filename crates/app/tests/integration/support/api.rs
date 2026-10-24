//! The API in process, called through HTTP requests, with every response checked against the
//! OpenAPI document of the routes that answered it (API-17, TEST-04).

use std::sync::Arc;

use axum::{Router, body::Body};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use http_body_util::BodyExt;
use platform::{
    clock::TestClock,
    http::{Pipeline, Routes},
    lifecycle::Readiness,
    telemetry::{self, Service},
};
use serde_json::Value;
use tower::ServiceExt;

use super::contract::Contract;

/// The service the tests' OpenAPI documents name. The name and version don't affect a check.
const SERVICE: Service = Service {
    name: "api",
    version: "0.0.0",
};

/// The routes under test inside the request pipeline, with a test clock.
#[derive(Clone)]
pub struct Api {
    router: Router,
    contract: Arc<Contract>,
    clock: Arc<TestClock>,
    readiness: Readiness,
}

impl Api {
    /// Every module's routes, with the pipeline's default settings.
    pub fn new() -> Self {
        Self::with(app::routes(), 100)
    }

    /// `routes` in the pipeline, serving at most `max_concurrent_requests` at once.
    pub fn with(routes: Routes, max_concurrent_requests: usize) -> Self {
        telemetry::testing::install(SERVICE).unwrap();
        let document = serde_json::to_value(routes.openapi(SERVICE)).unwrap();
        let clock = Arc::new(TestClock::default());
        let readiness = Readiness::new();
        let router = platform::http::app(
            routes,
            Pipeline {
                clock: clock.clone(),
                max_concurrent_requests,
                readiness: readiness.clone(),
            },
        );
        Self {
            router,
            contract: Arc::new(Contract::new(document)),
            clock,
            readiness,
        }
    }

    /// The clock the pipeline measures with.
    pub fn clock(&self) -> Arc<TestClock> {
        self.clock.clone()
    }

    /// What the readiness probe reports.
    pub fn readiness(&self) -> &Readiness {
        &self.readiness
    }

    /// A `GET` of `path`.
    pub fn get(&self, path: &str) -> Call<'_> {
        self.request(Method::GET, path)
    }

    /// A request with `method` to `path`, which may hold a query.
    pub fn request(&self, method: Method, path: &str) -> Call<'_> {
        Call {
            api: self,
            method,
            path: path.to_owned(),
            headers: HeaderMap::new(),
            body: Vec::new(),
        }
    }
}

/// A request being built.
pub struct Call<'a> {
    api: &'a Api,
    method: Method,
    path: String,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Call<'_> {
    /// Adds a header.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.insert(
            HeaderName::try_from(name).unwrap(),
            HeaderValue::try_from(value).unwrap(),
        );
        self
    }

    /// Sets the body.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// Sends the request, and checks the response against the document.
    pub async fn send(self) -> Response {
        let mut request = Request::builder()
            .method(self.method.clone())
            .uri(&self.path)
            .body(Body::from(self.body))
            .unwrap();
        *request.headers_mut() = self.headers;
        let response = self.api.router.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        self.api
            .contract
            .check(&self.method, &self.path, status, &headers, &body);
        Response {
            status,
            headers,
            body,
        }
    }
}

/// A response, already checked against the document.
pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl Response {
    /// The body as JSON.
    pub fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }

    /// The value of the header `name`, which the response must have.
    pub fn header(&self, name: &str) -> &str {
        let value = self.headers.get(name);
        assert!(value.is_some(), "no {name} header");
        value.unwrap().to_str().unwrap()
    }
}
