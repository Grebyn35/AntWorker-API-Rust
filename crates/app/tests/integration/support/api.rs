//! The API in process, called through HTTP requests, with every response checked against the
//! OpenAPI document of the routes that answered it (API-17, TEST-04).

use std::{collections::HashMap, ffi::OsString, io, net::SocketAddr, sync::Arc};

use axum::{Router, body::Body, extract::ConnectInfo};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use http_body_util::BodyExt;
use platform::{
    clock::TestClock,
    config::{ServerConfig, Settings},
    db::Database,
    http::Routes,
    lifecycle::Readiness,
    telemetry::{self, Service},
};
use serde_json::Value;
use tower::ServiceExt;

use super::{TestDatabase, contract::Contract};

/// The service the tests' OpenAPI documents name. The name and version don't affect a check.
const SERVICE: Service = Service {
    name: "api",
    version: "0.0.0",
};

/// The base64 of the tests' `HMAC_KEY`: 32 bytes of 7.
const HMAC_KEY: &str = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";

/// The address the tests' requests come from, unless a test names another.
pub const PEER: &str = "198.51.100.20:50000";

/// How the tests' API differs from its defaults: routes added to the API's, and settings.
#[derive(Default)]
pub struct Options {
    /// Routes the tests add, such as the fixtures.
    pub routes: Option<Routes>,
    /// The settings `serve` would read, beyond the tests' own.
    pub settings: Vec<(&'static str, String)>,
}

impl Options {
    /// The options with the fixtures' routes added.
    pub fn routes(routes: Routes) -> Self {
        Self {
            routes: Some(routes),
            ..Self::default()
        }
    }

    /// The options, with the setting `name` set to `value`.
    pub fn set(mut self, name: &'static str, value: impl ToString) -> Self {
        self.settings.push((name, value.to_string()));
        self
    }
}

/// The settings of the tests' API: the defaults, but limits high enough that only the tests of
/// the limits reach them, and `options`'s.
fn environment(options: &Options) -> HashMap<String, String> {
    let mut env: HashMap<String, String> = [
        ("ENVIRONMENT", "test"),
        ("LISTEN_ADDR", "127.0.0.1:0"),
        // The pipeline never connects with it: the tests hand it their own database.
        ("DATABASE_URL", "postgres://app:unused@127.0.0.1/api"),
        ("HMAC_KEY", HMAC_KEY),
        ("RATE_LIMIT_IP_REQUESTS", "1000000"),
        ("RATE_LIMIT_USER_REQUESTS", "1000000"),
        ("SIGN_IN_ATTEMPTS", "1000"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect();
    for (name, value) in &options.settings {
        env.insert((*name).to_owned(), value.clone());
    }
    env
}

/// The configuration `serve` would read from `options`.
pub fn configuration(options: &Options) -> (ServerConfig, app::Config) {
    let env = environment(options);
    let settings = Settings::new(
        |name: &str| env.get(name).map(OsString::from),
        |_: &std::path::Path| Err(io::Error::other("the tests read no files")),
    );
    (
        settings.server().unwrap(),
        app::Config::read(&settings).unwrap(),
    )
}

/// The API's application as `serve` builds it on `database`, with `options`, and its document.
pub async fn application(
    database: &Database,
    options: Options,
    clock: Arc<TestClock>,
    readiness: Readiness,
) -> (Router, Value) {
    let (config, modules) = configuration(&options);
    let services = app::compose(database, modules).await.unwrap();
    let pipeline = app::pipeline(&config, database, &services, clock, readiness);
    let mut document = app::openapi(SERVICE);
    let routes = match options.routes {
        Some(extra) => {
            document.merge(extra.openapi(SERVICE));
            services.routes.merge(extra)
        }
        None => services.routes,
    };
    let router = platform::http::app(routes, pipeline);
    (router, serde_json::to_value(document).unwrap())
}

/// The routes under test inside the request pipeline, on a database of their own, with a test
/// clock.
#[derive(Clone)]
pub struct Api {
    router: Router,
    contract: Arc<Contract>,
    clock: Arc<TestClock>,
    readiness: Readiness,
    database: Arc<TestDatabase>,
}

impl Api {
    /// Every module's routes, with the pipeline's default settings.
    pub async fn new() -> Self {
        Self::with(Options::default()).await
    }

    /// The API with `options`.
    pub async fn with(options: Options) -> Self {
        telemetry::testing::install(SERVICE).unwrap();
        let database = Arc::new(TestDatabase::migrated().await);
        let clock = Arc::new(TestClock::default());
        let readiness = Readiness::new();
        let (router, document) = application(
            &database.app().await,
            options,
            clock.clone(),
            readiness.clone(),
        )
        .await;
        Self {
            router,
            contract: Arc::new(Contract::new(document)),
            clock,
            readiness,
            database,
        }
    }

    /// The clock the pipeline measures with.
    pub fn clock(&self) -> Arc<TestClock> {
        self.clock.clone()
    }

    /// The OpenAPI document of the routes, which every response is checked against.
    pub fn document(&self) -> &Value {
        self.contract.document()
    }

    /// What the readiness probe reports.
    pub fn readiness(&self) -> &Readiness {
        &self.readiness
    }

    /// The database the API runs on.
    pub fn database(&self) -> &TestDatabase {
        &self.database
    }

    /// A `GET` of `path`.
    pub fn get(&self, path: &str) -> Call<'_> {
        self.request(Method::GET, path)
    }

    /// A `POST` of `path`, with `body` as JSON.
    pub fn post(&self, path: &str, body: &Value) -> Call<'_> {
        self.request(Method::POST, path)
            .header("content-type", "application/json")
            .body(body.to_string())
    }

    /// A request with `method` to `path`, which may hold a query.
    pub fn request(&self, method: Method, path: &str) -> Call<'_> {
        Call {
            api: self,
            method,
            path: path.to_owned(),
            headers: HeaderMap::new(),
            body: Vec::new(),
            peer: PEER.parse().unwrap(),
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
    peer: SocketAddr,
}

impl Call<'_> {
    /// Adds a header.
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.append(
            HeaderName::try_from(name).unwrap(),
            HeaderValue::try_from(value).unwrap(),
        );
        self
    }

    /// Authenticates the request with the Bearer header and `token`.
    pub fn bearer(self, token: &str) -> Self {
        self.header("authorization", &format!("Bearer {token}"))
    }

    /// Sets the body.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// Sends the request from `peer`, the address of the connection's other end.
    pub fn from(mut self, peer: &str) -> Self {
        self.peer = peer.parse().unwrap();
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
        request.extensions_mut().insert(ConnectInfo(self.peer));
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

    /// Every value of the header `name`.
    pub fn headers(&self, name: &str) -> Vec<&str> {
        self.headers
            .get_all(name)
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect()
    }
}
