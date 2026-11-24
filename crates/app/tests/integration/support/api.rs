//! The API in process, called through HTTP requests. Every response is checked against the
//! OpenAPI document of the routes that answered it, and so is the body of every typed request
//! (API-17, TEST-03).

use std::{collections::HashMap, ffi::OsString, io, net::SocketAddr, sync::Arc, time::Duration};

use axum::{Router, body::Body, extract::ConnectInfo};
use http::{HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use http_body_util::BodyExt;
use platform::{
    blocking::{Turn, Workers},
    clock::{SharedClock, TestClock},
    config::{ConfigError, ServerConfig, Settings},
    db::{Database, UnitOfWork},
    events::Hub,
    hashing::HmacKey,
    http::Routes,
    jobs::Jobs,
    lifecycle::{self, Readiness, Shutdown},
    mail::Mail,
    telemetry::{self, Service},
};
use serde_json::Value;
use tower::ServiceExt;

use super::{Proxy, TestDatabase, contract::Contract, sent, users::SignedIn};

/// The service the tests' OpenAPI documents name. The name and version don't affect a check.
pub(crate) const SERVICE: Service = Service {
    name: "api",
    version: "0.0.0",
};

/// The base64 of the tests' `HMAC_KEY`: 32 bytes of 7.
const HMAC_KEY: &str = "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";

/// The key of the tests' `HMAC_KEY`, with which a test computes what an API with that setting
/// derives by an HMAC, such as a rate-limit bucket's key (PLATFORM-057).
pub fn hmac_key() -> HmacKey {
    let bytes = data_encoding::BASE64.decode(HMAC_KEY.as_bytes()).unwrap();
    HmacKey::new(&bytes).unwrap()
}

/// The tests' `ENCRYPTION_KEYS`: one key, version 1, of 32 bytes of 7.
const ENCRYPTION_KEYS: &str = "1:BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=";

// part: superadmins
/// The tests' key ring, which the test of the seed's superadmin gives the binary, so the API
/// that it then runs in process reads the secret that the seed stored (SYSTEM-010, AUTH-066).
pub const KEY_RING: &str = ENCRYPTION_KEYS;
// end part: superadmins

/// The address the tests' requests come from, unless a test names another.
pub const PEER: &str = "198.51.100.20:50000";

// part: client_telemetry
/// The route of a user's consent to the telemetry of their clients.
pub const CONSENT: &str = "/v1/users/me/telemetry-consent";
// end part: client_telemetry

/// How the tests' API differs from its defaults: routes added to the API's, and settings.
#[derive(Default)]
pub struct Options {
    /// Routes the tests add, such as the fixtures.
    pub routes: Option<Routes>,
    /// The settings `serve` would read, beyond the tests' own.
    pub settings: Vec<(String, String)>,
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
    pub fn set(mut self, name: impl Into<String>, value: impl ToString) -> Self {
        self.settings.push((name.into(), value.to_string()));
        self
    }

    /// The options of a deployment with email off (PLATFORM-024).
    pub fn email_off() -> Self {
        Self::default().set("EMAIL", "off")
    }

    /// The settings, as [`TestDatabase::serve`](super::TestDatabase::serve) takes them.
    pub fn env(&self) -> Vec<(&str, &str)> {
        self.settings
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect()
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
        ("ENCRYPTION_KEYS", ENCRYPTION_KEYS),
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
        env.insert(name.clone(), value.clone());
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

/// The API's application as `serve` builds it, with what a test reaches beside its routes.
pub struct Application {
    /// The routes, inside the request pipeline.
    pub router: Router,
    /// Its OpenAPI document.
    pub document: Value,
    /// Its jobs, which a test runs through [`Jobs::run_pending`].
    pub jobs: Jobs,
    /// Its hub, whose head a test reads.
    pub hub: Hub,
    /// The turns of its CPU-heavy work, which a test takes to hold its works back.
    pub workers: Workers,
}

/// The API's application as `serve` builds it on `database`, with `options`. It installs the
/// tests' telemetry, which watches the test that runs the API in process, should it run on for
/// minutes (TEST-06).
pub async fn application(
    database: &Database,
    options: Options,
    clock: Arc<TestClock>,
    readiness: Readiness,
) -> Application {
    telemetry::testing::install(SERVICE).unwrap();
    let (config, modules) = configuration(&options);
    let connection = platform::events::listen(database, clock.as_ref())
        .await
        .unwrap();
    let mail = Mail::new(config.mail.clone(), clock.clone()).unwrap();
    let workers = Workers::new();
    let resources = app::Resources {
        database: database.clone(),
        clock,
        readiness,
        workers: workers.clone(),
        mail,
    };
    let services = app::compose(&resources, &config, modules, connection)
        .await
        .unwrap();
    let pipeline = app::pipeline(&resources, &config, &services, SERVICE);
    let jobs = services.jobs;
    let hub = services.hub;
    let (document, routes) = match options.routes {
        Some(extra) => (
            platform::http::document([app::openapi(SERVICE), extra.openapi(SERVICE)]),
            services.routes.merge(extra),
        ),
        None => (app::openapi(SERVICE), services.routes),
    };
    Application {
        router: sent::recording(platform::http::app(routes, pipeline)),
        document: serde_json::to_value(document).unwrap(),
        jobs,
        hub,
        workers,
    }
}

/// The API's application with `options`, on a database of its own, which it returns, served by
/// `platform::lifecycle::serve` as `serve` serves it, on a free port of 127.0.0.1, which it
/// returns too, until the test's runtime ends: for a test of what a connection carries, such as
/// a response cut short.
pub async fn served(options: Options) -> (TestDatabase, SocketAddr) {
    let database = TestDatabase::migrated().await;
    let clock = Arc::new(TestClock::default());
    let readiness = Readiness::new();
    let router = application(
        &database.app().await,
        options,
        clock.clone(),
        readiness.clone(),
    )
    .await
    .router;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let shutdown = Shutdown {
        delay: Duration::ZERO,
        drain: Duration::ZERO,
    };
    tokio::spawn(lifecycle::serve(
        listener,
        router,
        readiness,
        clock,
        shutdown,
        std::future::pending(),
    ));
    (database, address)
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
    hub: Hub,
    workers: Workers,
}

impl Api {
    /// Every module's routes, with the pipeline's default settings.
    pub async fn new() -> Self {
        Self::with(Options::default()).await
    }

    /// The API with email off (PLATFORM-024).
    pub async fn without_email() -> Self {
        Self::with(Options::email_off()).await
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
            .insert(0, ("S3_BUCKET".to_owned(), database.bucket().await));
        // end part: files
        let database = Arc::new(database);
        let clock = Arc::new(TestClock::default());
        let readiness = Readiness::new();
        let application = application(&pool, options, clock.clone(), readiness.clone()).await;
        Self {
            router: application.router,
            contract: Arc::new(Contract::new(application.document)),
            clock,
            readiness,
            database,
            pool,
            jobs: Arc::new(application.jobs),
            hub: application.hub,
            workers: application.workers,
        }
    }

    // part: files
    /// Another instance of the API, on this one's database and bucket, with the default
    /// settings, as a deployment's other replica is.
    pub async fn replica(&self) -> Self {
        self.instance(Options::default()).await
    }
    // end part: files

    /// Another instance of the API, on this one's database, with `options`, as a deployment's
    /// replica with other settings is, such as once a deploy turns email on.
    pub async fn instance(&self, options: Options) -> Self {
        // part: files
        let mut options = options;
        options
            .settings
            .insert(0, ("S3_BUCKET".to_owned(), self.database.bucket().await));
        // end part: files
        let clock = Arc::new(TestClock::default());
        let readiness = Readiness::new();
        let application = application(&self.pool, options, clock.clone(), readiness.clone()).await;
        Self {
            router: application.router,
            contract: Arc::new(Contract::new(application.document)),
            clock,
            readiness,
            database: self.database.clone(),
            pool: self.pool.clone(),
            jobs: Arc::new(application.jobs),
            hub: application.hub,
            workers: application.workers,
        }
    }

    /// The pool the API's routes and its hub run on.
    pub fn pool(&self) -> &Database {
        &self.pool
    }

    /// The turns of the API's CPU-heavy work, which a test takes to hold its works back
    /// (PLATFORM-060).
    pub fn workers(&self) -> &Workers {
        &self.workers
    }

    /// Takes every turn of the API's CPU-heavy work, as a burst of sign-ins does, so that each
    /// work of a request waits for a turn until the test drops them (PLATFORM-060).
    pub async fn every_turn(&self) -> Vec<Turn> {
        let mut turns = Vec::new();
        for _ in 0..self.workers.turns() {
            turns.push(self.workers.turn().await.unwrap());
        }
        turns
    }

    /// Takes every connection of the API's pool, each in a unit of work that the test holds, so
    /// that the API's own units of work find none free until the test drops them.
    pub async fn every_connection(&self) -> Vec<UnitOfWork> {
        let mut held = Vec::new();
        for _ in 0..TestDatabase::POOL_SIZE {
            held.push(self.pool.begin(&super::client(), None).await.unwrap());
        }
        held
    }

    /// Waits until the API's hub has loaded every event that committed so far, so that a stream
    /// that a test then opens starts after them, rather than receive those that the hub hadn't
    /// loaded yet: the hub loads an event once its notification reaches it, which the answer of
    /// the request that published it doesn't wait for. What the test did leads there, so the
    /// wait has no bound of its own (TEST-06).
    pub async fn hub_holds_what_committed(&self) {
        let mut uow = self.pool.begin(&super::client(), None).await.unwrap();
        let newest =
            sqlx::query_scalar!(r#"SELECT coalesce(max(position), 0) AS "newest!" FROM events"#)
                .fetch_one(uow.executor())
                .await
                .unwrap();
        drop(uow);
        // A hub that has stopped holds nothing more.
        while self.hub.head().await.is_some_and(|head| head < newest) {
            tokio::task::yield_now().await;
        }
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

    /// The interval at which the scheduler runs the periodic job `name`, as its registration
    /// gives it.
    pub fn interval(&self, name: &str) -> Duration {
        self.jobs.schedule(name).expect("a periodic job").0
    }

    /// The OpenAPI document of the routes, which every response is checked against.
    pub fn document(&self) -> &Value {
        self.contract.document()
    }

    /// Every operation of the document, as its method, its path, with an ID that parses in place
    /// of each parameter, so a request to it reaches authentication, and its access level
    /// (API-14), for a generated test of the routes of an access level.
    pub fn operations(&self) -> Vec<(Method, String, String)> {
        let mut operations = Vec::new();
        for (template, item) in self.document()["paths"].as_object().unwrap() {
            let path: String = template
                .split('/')
                .map(|part| {
                    if part.starts_with('{') {
                        "01900000-0000-7000-8000-000000000000"
                    } else {
                        part
                    }
                })
                .collect::<Vec<_>>()
                .join("/");
            for (method, operation) in item.as_object().unwrap() {
                let method = Method::from_bytes(method.to_ascii_uppercase().as_bytes()).unwrap();
                let access = operation["x-access"].as_str().unwrap().to_owned();
                operations.push((method, path.clone(), access));
            }
        }
        operations
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

    /// Checks a response against the document, and the body of a 500 to any method but `HEAD`,
    /// which answers none, against ERR-08's, as every response of [`Call::send`] is.
    pub fn check(
        &self,
        method: &Method,
        path: &str,
        status: StatusCode,
        headers: &HeaderMap,
        body: &[u8],
    ) {
        self.contract.check(method, path, status, headers, body);
        if status == StatusCode::INTERNAL_SERVER_ERROR && method != Method::HEAD {
            let problem: Value = serde_json::from_slice(body).unwrap_or_default();
            let mut members: Vec<&str> = problem
                .as_object()
                .into_iter()
                .flat_map(|members| members.keys().map(String::as_str))
                .collect();
            members.sort_unstable();
            assert!(
                problem["code"] == "request.internal_error"
                    && problem["title"] == "Internal error"
                    && members == ["code", "status", "title", "trace_id", "type"],
                "{method} {path} answered 500 with another body than an internal error's fixed \
                 one (ERR-08): {problem}"
            );
        }
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

    // part: client_telemetry
    /// A `PUT` of [`CONSENT`], with `body` as JSON, which is checked against the operation's
    /// documented request body when sent (API-03).
    pub fn put(&self, body: &Value) -> Call<'_> {
        let mut call = self
            .request(Method::PUT, CONSENT)
            .header("content-type", "application/json")
            .body(body.to_string());
        call.typed = true;
        call
    }
    // end part: client_telemetry

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

    /// Sends, in a task of its own, with the access token `access`, `method` to `path`, or, with
    /// a `body`, a `PATCH` of it as a merge patch when `method` is `PATCH`, and a `POST` of it as
    /// JSON for any other `method`, and gives the response's status and its body's JSON, or null
    /// for an empty body: a request that a test holds at a lock while it changes what the request
    /// then finds.
    pub fn spawn(
        &self,
        method: Method,
        path: &str,
        access: &str,
        body: Option<Value>,
    ) -> tokio::task::JoinHandle<(StatusCode, Value)> {
        let (api, path, access) = (self.clone(), path.to_owned(), access.to_owned());
        tokio::spawn(async move {
            let call = match body {
                Some(body) if method == Method::PATCH => api.patch(&path, &body),
                Some(body) => api.post(&path, &body),
                None => api.request(method, &path),
            };
            let response = call.bearer(&access).send().await;
            let body = if response.body.is_empty() {
                Value::Null
            } else {
                response.json()
            };
            (response.status, body)
        })
    }

    /// Checks who published the events of the request that `response` answers, as its trace ID
    /// names them: its caller, when it carried a bearer token, which proved one, and no one when
    /// it carried no credential, so that an operation that publishes from a unit of work that it
    /// opened without its caller's `Principal` fails a test that runs it with a bearer token
    /// (PLATFORM-095). A request that carried only a cookie may have proved a caller or none, and
    /// isn't checked, and nor is the deletion of the caller's own account, whose events lose
    /// their publisher with the user's row.
    async fn check_publishers(
        &self,
        method: &Method,
        path: &str,
        response: &Response,
        bearer: bool,
        credential: bool,
    ) {
        let deletes_its_caller = method == Method::DELETE && path == "/v1/users/me";
        if bearer != credential || deletes_its_caller {
            return;
        }
        let published = self
            .database
            .owner_reads(&format!(
                "SELECT (publisher_id IS NOT NULL)::text FROM events \
                 WHERE split_part(traceparent, '-', 2) = '{}'",
                response.trace_id()
            ))
            .await;
        let expected = if bearer { "true" } else { "false" };
        assert!(
            published.iter().all(|published| published == expected),
            "{method} {path} published {} events, and {} of them name a publisher, where {}",
            published.len(),
            published.iter().filter(|named| *named == "true").count(),
            if bearer {
                "its bearer token proved its caller"
            } else {
                "it carried no credential"
            },
        );
    }

    /// Checks the permission of each entry that the request that `response` answers recorded in
    /// a workspace's log with one, as its trace ID names them: it is the permission of each
    /// workspace event that the request published in that workspace, or `workspace.read` when it
    /// published none there, so that an action that declares another permission than its
    /// change's event fails a test that runs it (the audit spec's "Reading a workspace's log").
    async fn check_log_permissions(&self, method: &Method, path: &str, response: &Response) {
        let trace_id = response.trace_id();
        let entries = self
            .database
            .owner_reads(&format!(
                "SELECT workspace_id || ' ' || permission || ' ' || action FROM audit_entries \
                 WHERE trace_id = '{trace_id}' AND scope = 'workspace' AND permission IS NOT NULL"
            ))
            .await;
        if entries.is_empty() {
            return;
        }
        let events = self
            .database
            .owner_reads(&format!(
                "SELECT DISTINCT workspace_id || ' ' || permission FROM events \
                 WHERE split_part(traceparent, '-', 2) = '{trace_id}' AND scope = 'workspace'"
            ))
            .await;
        for entry in entries {
            let (workspace, rest) = entry.split_once(' ').unwrap();
            let (permission, action) = rest.split_once(' ').unwrap();
            let published: Vec<&str> = events
                .iter()
                .filter_map(|event| event.strip_prefix(workspace))
                .map(str::trim_start)
                .collect();
            let expected = if published.is_empty() {
                vec!["workspace.read"]
            } else {
                published
            };
            assert!(
                expected.iter().all(|of_event| *of_event == permission),
                "{method} {path} recorded `{action}` in a workspace's log with the permission \
                 {permission}, where the workspace events that it published there carry \
                 {expected:?}, or `workspace.read` when it published none"
            );
        }
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
        let bearer = self
            .headers
            .get("authorization")
            .is_some_and(|value| value.as_bytes().starts_with(b"Bearer "));
        let credential = bearer || self.headers.contains_key("cookie");
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
            .check(&self.method, &self.path, status, &headers, &body);
        let response = Response {
            status,
            headers,
            body,
        };
        if status.is_success() && !self.method.is_safe() {
            self.api
                .check_publishers(&self.method, &self.path, &response, bearer, credential)
                .await;
            self.api
                .check_log_permissions(&self.method, &self.path, &response)
                .await;
        }
        response
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

    /// The trace ID of the request, from `server-timing: trace;desc=00-<trace ID>-<span
    /// ID>-<flags>` (API-16).
    pub fn trace_id(&self) -> &str {
        self.header("server-timing").split('-').nth(1).unwrap()
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

    /// The field and the code of each error of the response, which must be a 400
    /// `request.invalid_fields` (PLATFORM-051), in their order.
    pub fn refused_fields(&self) -> Vec<(String, String)> {
        self.refused("request.invalid_fields")
    }

    /// The parameter and the code of each error of the response, which must be a 400
    /// `request.invalid_query` (PLATFORM-045), in their order.
    pub fn refused_parameters(&self) -> Vec<(String, String)> {
        self.refused("request.invalid_query")
    }

    /// The field and the code of each error of the response, which must be a 400 whose problem's
    /// code is `code`, in their order.
    fn refused(&self, code: &str) -> Vec<(String, String)> {
        assert_eq!(self.status, 400);
        let problem = self.json();
        assert_eq!(problem["code"], code);
        problem["errors"]
            .as_array()
            .unwrap()
            .iter()
            .map(|error| {
                (
                    error["field"].as_str().unwrap().to_owned(),
                    error["code"].as_str().unwrap().to_owned(),
                )
            })
            .collect()
    }
}

/// The first column of each row of `sql`, as the `owner` reads it on `api`'s database (TEST-05).
pub async fn reads(api: &Api, sql: &str) -> Vec<String> {
    api.database().owner_reads(sql).await
}

/// The one value that `sql` reads, as [`reads`] reads it.
pub async fn one(api: &Api, sql: &str) -> String {
    reads(api, sql).await.remove(0)
}

/// Sends `method` to `path` as `caller`, with no body.
pub async fn call(api: &Api, method: Method, path: &str, caller: &SignedIn) -> Response {
    api.request(method, path)
        .bearer(&caller.access)
        .send()
        .await
}
