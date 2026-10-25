//! A reference deployment on a server of the tests' own (TEST-04), whose database is managed,
//! Compose's Postgres, or bundled, with its backups in a stand-in bucket on the same server.

use std::{
    process::Output,
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use rustls::pki_types::{CertificateDer, pem::PemObject};
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::{
    docker::{compose, docker},
    images::Pushed,
    server::{DEPLOYMENT, Server},
    support::TestDatabase,
};

/// The API's address in the tests: a name under `localhost`, for which Caddy signs the
/// certificate with an authority of its own instead of asking ACME.
pub const API_ADDRESS: &str = "api.localhost";
/// Grafana's address in the tests, under `localhost` as the API's is.
pub const GRAFANA_ADDRESS: &str = "grafana.localhost";

/// How many deployments of the suite run the self-hosted stack at once. Each server pulls the
/// stack's images into a store of its own, 3.4 GB of them, so the suite's disk stays within what
/// a CI runner has free (docs/operations/ci.md).
static STACKS: Semaphore = Semaphore::const_new(2);

/// A deployment on a server of its own.
pub struct Deployment {
    pub server: Server,
    /// The API's image under test, as the server pulls it.
    pub api: String,
    postgres: String,
    /// The managed database, or none for the bundled one.
    database: Option<TestDatabase>,
    /// The settings of its kind of deployment, which `changes` of `set` add to or replace.
    base: Vec<(String, String)>,
    /// Whether the settings that `set` last wrote run the self-hosted stack.
    runs_stack: AtomicBool,
    /// Its share of `STACKS`, from its first deploy that runs the stack. Declared after `server`,
    /// so it is released once the server and its store are gone.
    stack: OnceLock<SemaphorePermit<'static>>,
}

impl Deployment {
    /// A deployment whose managed database is a new test database, with the tests' settings and
    /// `changes`.
    pub async fn managed(changes: &[(&str, &str)]) -> Self {
        let database = TestDatabase::empty().await;
        let deployment = Self::new(Some(database), &[("DATABASE", "managed")], changes);
        let host = postgres_address();
        let database = deployment.database.as_ref().expect("the managed database");
        for (secret, setting) in [
            ("database_admin_url", "DATABASE_ADMIN_URL"),
            ("database_owner_url", "DATABASE_OWNER_URL"),
            ("database_url", "DATABASE_URL"),
        ] {
            let mut url = url::Url::parse(&std::env::var(setting).expect(setting)).expect(setting);
            url.set_host(Some(&host)).expect("a host");
            url.set_port(Some(5432)).expect("a port");
            url.set_path(database.name());
            deployment
                .server
                .write(&format!("{DEPLOYMENT}/secrets/{secret}"), url.as_str());
        }
        deployment
    }

    /// A deployment whose database is the bundled Postgres, which backs up to a stand-in bucket
    /// on the same server, with the tests' settings and `changes`.
    pub fn bundled(changes: &[(&str, &str)]) -> Self {
        let server = Server::start();
        let bucket = server.start_bucket();
        let mut kind = vec![("DATABASE".to_owned(), "bundled".to_owned())];
        kind.extend(bucket);
        let kind: Vec<(&str, &str)> = kind
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        Self::on(server, None, &kind, changes)
    }

    fn new(
        database: Option<TestDatabase>,
        kind: &[(&str, &str)],
        changes: &[(&str, &str)],
    ) -> Self {
        Self::on(Server::start(), database, kind, changes)
    }

    fn on(
        server: Server,
        database: Option<TestDatabase>,
        kind: &[(&str, &str)],
        changes: &[(&str, &str)],
    ) -> Self {
        let mut base: Vec<(String, String)> = [
            ("IMAGE_REPOSITORY", "registry:5000/image"),
            ("SOURCE", "https://example.invalid/e2e"),
            ("API_ADDRESS", API_ADDRESS),
            ("GRAFANA_ADDRESS", GRAFANA_ADDRESS),
            ("TELEMETRY", "off"),
            ("API_REPLICAS", "2"),
            ("DATABASE_MAX_CONNECTIONS", "2"),
            ("POSTGRES_MAX_CONNECTIONS", "100"),
        ]
        .iter()
        .chain(kind)
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
        base.sort();
        let deployment = Self {
            server,
            api: Pushed::api().in_network(),
            postgres: Pushed::postgres().in_network(),
            database,
            base,
            runs_stack: AtomicBool::new(false),
            stack: OnceLock::new(),
        };
        deployment.set(changes);
        deployment
    }

    /// Writes the deployment's settings: those of its kind, with `changes`.
    pub fn set(&self, changes: &[(&str, &str)]) {
        let mut settings = self.base.clone();
        for (name, value) in changes {
            settings.retain(|(existing, _)| existing != name);
            settings.push(((*name).to_owned(), (*value).to_owned()));
        }
        let runs_stack = settings
            .iter()
            .any(|(name, value)| name == "TELEMETRY" && value == "self-hosted");
        self.runs_stack.store(runs_stack, Ordering::Relaxed);
        let text: String = settings
            .iter()
            .map(|(name, value)| format!("{name}={value}\n"))
            .collect();
        self.server
            .write(&format!("{DEPLOYMENT}/settings.env"), &text);
    }

    /// Deploys `api`, the API's image, with `options`. With a managed database, no other test
    /// runs `bootstrap` meanwhile, since the deploy runs it on the roles every test database
    /// shares (the platform spec's Database roles). A deploy that runs the self-hosted stack
    /// first waits for a share of `STACKS`, before it takes that lock.
    pub async fn apply(&self, api: &str, options: &[&str]) -> Output {
        if self.runs_stack.load(Ordering::Relaxed) && self.stack.get().is_none() {
            let permit = STACKS.acquire().await.expect("STACKS stays open");
            self.stack
                .set(permit)
                .expect("one share for each deployment");
        }
        let mut args = vec!["apply", api, &self.postgres];
        args.extend(options);
        match &self.database {
            Some(database) => {
                database
                    .with_roles_locked(async { self.server.deploy(&args) })
                    .await
            }
            None => self.server.deploy(&args),
        }
    }

    /// The first column of each row that `sql` returns from the bundled database, as its
    /// administrator, one row per line.
    pub fn sql(&self, sql: &str) -> String {
        self.server.write("/root/statement.sql", sql);
        self.server.shell_ok(
            "docker exec --interactive deployment-postgres-1 \
             psql --username=postgres --dbname=api -qAt -v ON_ERROR_STOP=1 < /root/statement.sql",
        )
    }

    /// The address of the API's resource at `path`, through the port that the server publishes
    /// for its port `port`.
    pub fn url(&self, scheme: &str, port: u16, path: &str) -> String {
        self.url_at(API_ADDRESS, scheme, port, path)
    }

    /// The address of the resource at `path` of the site `site`, such as Grafana's, through the
    /// port that the server publishes for its port `port`.
    pub fn url_at(&self, site: &str, scheme: &str, port: u16, path: &str) -> String {
        let published = self.server.address(port).port();
        format!("{scheme}://{site}:{published}{path}")
    }
}

/// A client of the API, through Caddy, that trusts `root`, the authority of Caddy's certificates
/// for names under `localhost`, and follows no redirect. It runs a runtime of its own, so it is
/// built and dropped on a thread of its own, outside the test's runtime.
fn client(root: &str) -> reqwest::blocking::Client {
    let mut roots = rustls::RootCertStore::empty();
    for certificate in CertificateDer::pem_slice_iter(root.as_bytes()) {
        roots
            .add(certificate.expect("Caddy's root certificate"))
            .expect("a certificate to trust");
    }
    let tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("TLS versions")
    .with_root_certificates(roots)
    .with_no_client_auth();
    reqwest::blocking::Client::builder()
        .tls_backend_preconfigured(tls)
        .resolve(API_ADDRESS, ([127, 0, 0, 1], 0).into())
        .resolve(GRAFANA_ADDRESS, ([127, 0, 0, 1], 0).into())
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .expect("an HTTP client")
}

/// Sends `GET` to `url`, through a client that trusts `root`, and returns the response's status
/// and headers.
pub fn get(root: String, url: String) -> (u16, reqwest::header::HeaderMap) {
    thread::spawn(move || {
        let response = client(&root).get(&url).send().expect("an answer");
        (response.status().as_u16(), response.headers().clone())
    })
    .join()
    .expect("the request")
}

/// The address of Compose's Postgres on its network, which the deployment's containers reach
/// through the server.
fn postgres_address() -> String {
    let container = compose(&["ps", "--quiet", "postgres"]);
    docker(&[
        "inspect",
        "--format",
        "{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}",
        &container,
    ])
}

/// Requests sent to the API one after another, from a thread of their own, until stopped.
pub struct Traffic {
    running: Arc<AtomicBool>,
    thread: JoinHandle<Vec<String>>,
}

impl Traffic {
    /// Starts sending `GET` to `url`, through a client that trusts `root`.
    pub fn start(root: String, url: String) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let flag = running.clone();
        let thread = thread::spawn(move || {
            let client = client(&root);
            let mut outcomes = Vec::new();
            while flag.load(Ordering::Relaxed) {
                outcomes.push(match client.get(&url).send() {
                    Ok(response) => response.status().as_str().to_owned(),
                    Err(error) => format!("{error:?}"),
                });
            }
            outcomes
        });
        Self { running, thread }
    }

    /// Stops the requests, and returns each one's outcome: its status, or the error.
    pub fn stop(self) -> Vec<String> {
        self.running.store(false, Ordering::Relaxed);
        self.thread.join().expect("the requests")
    }
}

/// Asserts that every request answered 200, and that there were enough of them to cover the
/// deploy.
pub fn assert_all_succeeded(outcomes: &[String]) {
    let failed: Vec<&String> = outcomes
        .iter()
        .filter(|outcome| *outcome != "200")
        .collect();
    assert!(outcomes.len() > 20, "only {} requests", outcomes.len());
    assert!(
        failed.is_empty(),
        "{} of {} requests failed: {failed:?}",
        failed.len(),
        outcomes.len()
    );
}
