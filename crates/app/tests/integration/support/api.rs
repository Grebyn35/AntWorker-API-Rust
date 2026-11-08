//! The API in process, called through HTTP requests. Every response is checked against the
//! OpenAPI document of the routes that answered it, and so is the body of every typed request
//! (API-17, TEST-03).

use std::{collections::HashMap, ffi::OsString, io, net::SocketAddr, sync::Arc};

use axum::{Router, body::Body, extract::ConnectInfo};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use http_body_util::BodyExt;
use platform::{
    blocking::Workers,
    clock::{SharedClock, TestClock},
    config::{ConfigError, ServerConfig, Settings},
    db::Database,
    http::Routes,
    jobs::Jobs,
    lifecycle::Readiness,
    telemetry::{self, Service},
};
use serde_json::Value;
use tower::ServiceExt;

use super::{Proxy, TestDatabase, contract::Contract};

/// The service the tests' OpenAPI documents name. The name and version don't affect a check.
pub(crate) const SERVICE: Service = Service {
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

    /// The options, with `routes` added to theirs.
    pub fn with_routes(mut self, routes: Routes) -> Self {
        self.routes = Some(match self.routes.take() {
            Some(theirs) => theirs.merge(routes),
            None => routes,
        });
        self
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
        ("SIGN_IN_EMAIL_ATTEMPTS", "100000"),
        // The emails go to the Mailpit that `just services` starts (PLATFORM-023), whose SMTP server
        // takes no TLS and no credentials.
        ("EMAIL", "smtp"),
        ("SMTP_HOST", "127.0.0.1"),
        ("SMTP_PORT", "1025"),
        ("SMTP_SECURITY", "none"),
        ("EMAIL_FROM", "AntWorker <noreply@example.com>"),
    ]
    .into_iter()
    .map(|(name, value)| (name.to_owned(), value.to_owned()))
    .collect();
    // part: files
    env.extend(
        [
            ("RATE_LIMIT_PUBLIC_REQUESTS", "1000000"),
            // The bucket of files' bytes, in the SeaweedFS that `just services` starts, which
            // takes no credentials. A test that runs the API names a bucket of its own
            // (`bucket`).
            ("S3_ENDPOINT", super::bucket::ENDPOINT),
            ("S3_REGION", "us-east-1"),
            ("S3_BUCKET", "files"),
            ("S3_URI_STYLE", "path"),
            ("S3_ACCESS_KEY_ID", "test"),
            ("S3_SECRET_ACCESS_KEY", "test"),
            // The malware scanner that `just services` starts, which knows only the EICAR test
            // file (the files spec's "Scanning").
            ("CLAMD_ADDRESS", super::scanner::ADDRESS),
        ]
        .map(|(name, value)| (name.to_owned(), value.to_owned())),
    );
    // end part: files
    for (name, value) in &options.settings {
        env.insert((*name).to_owned(), value.clone());
    }
    env
}

/// The configuration `serve` would read from `options`.
pub fn configuration(options: &Options) -> (ServerConfig, app::Config) {
    read(options).unwrap()
}

/// Why `serve` would refuse the settings of `options`, which it must refuse.
pub fn refusal(options: &Options) -> String {
    read(options).err().expect("a refusal").to_string()
}

fn read(options: &Options) -> Result<(ServerConfig, app::Config), ConfigError> {
    let env = environment(options);
    let settings = Settings::new(
        |name: &str| env.get(name).map(OsString::from),
        |_: &std::path::Path| Err(io::Error::other("the tests read no files")),
    );
    Ok((settings.server()?, app::Config::read(&settings)?))
}

/// The API's application as `serve` builds it on `database`, with `options`, its document, and
/// its jobs, which a test runs through [`Jobs::run_pending`]. It installs the tests' telemetry,
/// which watches the test that runs the API in process, should it run on for minutes (TEST-06).
pub async fn application(
    database: &Database,
    options: Options,
    clock: Arc<TestClock>,
    readiness: Readiness,
) -> (Router, Value, Jobs) {
    telemetry::testing::install(SERVICE).unwrap();
    let (config, modules) = configuration(&options);
    let connection = platform::events::listen(database, clock.as_ref())
        .await
        .unwrap();
    let resources = app::Resources {
        database: database.clone(),
        clock,
        readiness,
        workers: Workers::new(),
    };
    let services = app::compose(&resources, &config, modules, connection)
        .await
        .unwrap();
    let pipeline = app::pipeline(&resources, &config, &services, SERVICE);
    let jobs = services.jobs;
    let (document, routes) = match options.routes {
        Some(extra) => (
            platform::http::document([app::openapi(SERVICE), extra.openapi(SERVICE)]),
            services.routes.merge(extra),
        ),
        None => (app::openapi(SERVICE), services.routes),
    };
    let router = platform::http::app(routes, pipeline);
    (router, serde_json::to_value(document).unwrap(), jobs)
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
    pool: Database,
    jobs: Arc<Jobs>,
}

impl Api {
    /// Every module's routes, with the pipeline's default settings.
    pub async fn new() -> Self {
        Self::with(Options::default()).await
    }

    /// The API with `options`.
    pub async fn with(options: Options) -> Self {
        Self::on(TestDatabase::migrated().await, options).await
    }

    /// The API with `options`, on `database`, such as one that a subcommand of the binary filled.
    pub async fn on(database: TestDatabase, options: Options) -> Self {
        let pool = database.app().await;
        Self::connected(database, pool, options).await
    }

    /// The API with `options`, on `database`, whose every connection passes through `proxy`.
    pub async fn through(database: TestDatabase, proxy: &Proxy, options: Options) -> Self {
        let pool = database.app_through(proxy).await;
        Self::connected(database, pool, options).await
    }

    async fn connected(database: TestDatabase, pool: Database, options: Options) -> Self {
        // part: files
        // The test's own bucket, unless the test names another setting of the bucket's.
        let mut options = options;
        options
            .settings
            .insert(0, ("S3_BUCKET", database.bucket().await));
        // end part: files
        let database = Arc::new(database);
        let clock = Arc::new(TestClock::default());
        let readiness = Readiness::new();
        let (router, document, jobs) =
            application(&pool, options, clock.clone(), readiness.clone()).await;
        Self {
            router,
            contract: Arc::new(Contract::new(document)),
            clock,
            readiness,
            database,
            pool,
            jobs: Arc::new(jobs),
        }
    }

    // part: files
    /// Another instance of the API, on this one's database and bucket, with the default
    /// settings, as a deployment's other replica is.
    pub async fn replica(&self) -> Self {
        let mut options = Options::default();
        options
            .settings
            .insert(0, ("S3_BUCKET", self.database.bucket().await));
        let clock = Arc::new(TestClock::default());
        let readiness = Readiness::new();
        let (router, document, jobs) =
            application(&self.pool, options, clock.clone(), readiness.clone()).await;
        Self {
            router,
            contract: Arc::new(Contract::new(document)),
            clock,
            readiness,
            database: self.database.clone(),
            pool: self.pool.clone(),
            jobs: Arc::new(jobs),
        }
    }
    // end part: files

    /// The pool the API's routes and its hub run on.
    pub fn pool(&self) -> &Database {
        &self.pool
    }

    /// The clock the pipeline measures with.
    pub fn clock(&self) -> Arc<TestClock> {
        self.clock.clone()
    }

    /// Runs every queued job that the API's requests enqueued and that is runnable now, and
    /// returns how many runs it made.
    pub async fn run_jobs(&self) -> usize {
        let clock: SharedClock = self.clock.clone();
        self.jobs
            .run_pending(&self.database.app().await, &clock)
            .await
    }

    /// Runs the periodic job `name` once, now.
    pub async fn run_periodic(&self, name: &str) {
        self.jobs.run_periodic(name).await.unwrap();
    }

    /// The OpenAPI document of the routes, which every response is checked against.
    pub fn document(&self) -> &Value {
        self.contract.document()
    }

    /// The checks against the document, which a stream's frames go through.
    pub fn contract(&self) -> Arc<Contract> {
        self.contract.clone()
    }

    /// What the readiness probe reports.
    pub fn readiness(&self) -> &Readiness {
        &self.readiness
    }

    /// The API's router, for a test that reads a response as it streams.
    pub fn router(&self) -> Router {
        self.router.clone()
    }

    /// Checks a response against the document, as every response of [`Call::send`] is.
    pub fn check(
        &self,
        method: &Method,
        path: &str,
        status: StatusCode,
        headers: &HeaderMap,
        body: &[u8],
    ) {
        self.contract.check(method, path, status, headers, body);
    }

    /// The database the API runs on.
    pub fn database(&self) -> &TestDatabase {
        &self.database
    }

    /// A `GET` of `path`.
    pub fn get(&self, path: &str) -> Call<'_> {
        self.request(Method::GET, path)
    }

    /// A `POST` of `path`, with `body` as JSON. The body is checked against the operation's
    /// documented request body when sent; a test of malformed input uses
    /// [`Api::post_raw`] or [`Call::body`] instead.
    pub fn post(&self, path: &str, body: &Value) -> Call<'_> {
        let mut call = self.post_raw(path, body);
        call.typed = true;
        call
    }

    /// A `POST` of `path`, with `body` as JSON that is sent raw, never checked against the
    /// document: the call of a test of malformed input.
    pub fn post_raw(&self, path: &str, body: &Value) -> Call<'_> {
        self.request(Method::POST, path)
            .header("content-type", "application/json")
            .body(body.to_string())
    }

    /// A `PATCH` of `path`, with `body` as a JSON Merge Patch, which is checked against the
    /// operation's documented request body when sent (API-03).
    pub fn patch(&self, path: &str, body: &Value) -> Call<'_> {
        let mut call = self
            .request(Method::PATCH, path)
            .header("content-type", "application/merge-patch+json")
            .body(body.to_string());
        call.typed = true;
        call
    }

    /// A request with `method` to `path`, which may hold a query.
    pub fn request(&self, method: Method, path: &str) -> Call<'_> {
        Call {
            api: self,
            method,
            path: path.to_owned(),
            headers: HeaderMap::new(),
            body: Vec::new(),
            typed: false,
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
    /// Whether the body came from a typed call, and so is checked against the document.
    typed: bool,
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

    /// Sets the body raw: it is sent as given and never checked against the document.
    pub fn body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self.typed = false;
        self
    }

    /// Sends the request from `peer`, the address of the connection's other end.
    pub fn from(mut self, peer: &str) -> Self {
        self.peer = peer.parse().unwrap();
        self
    }

    /// Checks a typed body against the document, sends the request, and checks the response
    /// against the document.
    pub async fn send(mut self) -> Response {
        if self.typed {
            self.api
                .contract
                .check_request(&self.method, &self.path, &self.headers, &self.body);
        }
        let body = Body::from(std::mem::take(&mut self.body));
        self.dispatch(body).await
    }

    // part: files
    /// Sends the request with `body`, a stream that the test feeds, in place of a raw body, and
    /// checks the response against the document.
    pub async fn send_streamed(self, body: Body) -> Response {
        self.dispatch(body).await
    }
    // end part: files

    /// Sends the request with `body`, and checks the response against the document.
    async fn dispatch(self, body: Body) -> Response {
        let mut request = Request::builder()
            .method(self.method.clone())
            .uri(&self.path)
            .body(body)
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
