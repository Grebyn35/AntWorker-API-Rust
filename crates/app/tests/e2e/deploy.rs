//! Deploys of the reference deployment (OPS-16, OPS-20, OPS-21), on a server of the tests' own,
//! with Compose's Postgres as a managed database.

use std::{
    process::Output,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use rustls::pki_types::{CertificateDer, pem::PemObject};

use crate::{
    docker::{compose, docker, lossy},
    images::Pushed,
    server::{DEPLOYMENT, Server},
    support::TestDatabase,
};

/// The API's address in the tests: a name under `localhost`, for which Caddy signs the
/// certificate with an authority of its own instead of asking ACME.
const API_ADDRESS: &str = "api.localhost";

/// A deployment on a server of its own, whose managed database is a test database.
struct Deployment {
    server: Server,
    api: String,
    postgres: String,
}

impl Deployment {
    /// A server with the deployment's settings, `changes` among them, and the URLs of
    /// `database`.
    fn new(database: &TestDatabase, changes: &[(&str, &str)]) -> Self {
        let server = Server::start();
        let deployment = Self {
            server,
            api: Pushed::api().in_network(),
            postgres: Pushed::postgres().in_network(),
        };
        deployment.set(changes);
        let host = postgres_address();
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

    /// Writes the deployment's settings: those of the tests, with `changes`.
    fn set(&self, changes: &[(&str, &str)]) {
        let mut settings = vec![
            ("IMAGE_REPOSITORY", "registry:5000/image"),
            ("SOURCE", "https://example.invalid/e2e"),
            ("API_ADDRESS", API_ADDRESS),
            ("DATABASE", "managed"),
            ("TELEMETRY", "off"),
            ("API_REPLICAS", "2"),
            ("DATABASE_MAX_CONNECTIONS", "2"),
            ("POSTGRES_MAX_CONNECTIONS", "100"),
        ];
        for (name, value) in changes {
            settings.retain(|(existing, _)| existing != name);
            settings.push((name, value));
        }
        let text: String = settings
            .iter()
            .map(|(name, value)| format!("{name}={value}\n"))
            .collect();
        self.server
            .write(&format!("{DEPLOYMENT}/settings.env"), &text);
    }

    /// Deploys `api`, the API's image, with `options`, while no other test runs `bootstrap`,
    /// which the deploy runs (the platform spec's Database roles).
    async fn apply(&self, database: &TestDatabase, api: &str, options: &[&str]) -> Output {
        let mut args = vec!["apply", api, &self.postgres];
        args.extend(options);
        database
            .with_roles_locked(async { self.server.deploy(&args) })
            .await
    }

    /// The address of the API's resource at `path`, through the port that the server publishes
    /// for its port `port`.
    fn url(&self, scheme: &str, port: u16, path: &str) -> String {
        let published = self.server.address(port).port();
        format!("{scheme}://{API_ADDRESS}:{published}{path}")
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
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .expect("an HTTP client")
}

/// Sends `GET` to `url`, through a client that trusts `root`, and returns the response's status
/// and headers.
fn get(root: String, url: String) -> (u16, reqwest::header::HeaderMap) {
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
struct Traffic {
    running: Arc<AtomicBool>,
    thread: JoinHandle<Vec<String>>,
}

impl Traffic {
    /// Starts sending `GET` to `url`, through a client that trusts `root`.
    fn start(root: String, url: String) -> Self {
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
    fn stop(self) -> Vec<String> {
        self.running.store(false, Ordering::Relaxed);
        self.thread.join().expect("the requests")
    }
}

/// Asserts that every request answered 200, and that there were enough of them to cover the
/// deploy.
fn assert_all_succeeded(outcomes: &[String]) {
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

#[tokio::test]
async fn ops_16_a_deploy_replaces_every_replica_while_every_request_succeeds() {
    let database = TestDatabase::empty().await;
    let deployment = Deployment::new(&database, &[]);
    let first = deployment.apply(&database, &deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let blue = deployment.server.running("api-blue");

    let traffic = Traffic::start(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
    );
    let second = deployment.apply(&database, &deployment.api, &[]).await;
    let outcomes = traffic.stop();

    assert!(second.status.success(), "{}", lossy(&second));
    assert_eq!(blue.len(), 2, "{}", lossy(&first));
    assert_eq!(deployment.server.running("api-green").len(), 2);
    assert!(deployment.server.running("api-blue").is_empty());
    assert_all_succeeded(&outcomes);
}

#[tokio::test]
async fn ops_16_a_release_whose_replicas_never_become_ready_is_removed_while_the_live_ones_serve() {
    let database = TestDatabase::empty().await;
    let deployment = Deployment::new(&database, &[]);
    let first = deployment.apply(&database, &deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let blue = deployment.server.running("api-blue");
    // The API's image, with a health check that always fails.
    let unready = deployment.server.shell_ok(&format!(
        "printf 'FROM {}\\nHEALTHCHECK --interval=1s --retries=2 CMD [\"/usr/local/bin/antworker-api\", \"openapi\", \"/proc/unwritable\"]\\n' \
         | docker build --quiet --tag registry:5000/unready:e2e - > /dev/null \
         && docker push --quiet registry:5000/unready:e2e > /dev/null \
         && docker image inspect --format '{{{{index .RepoDigests 0}}}}' registry:5000/unready:e2e",
        deployment.api
    ));

    let traffic = Traffic::start(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
    );
    let refused = deployment.apply(&database, &unready, &[]).await;
    let outcomes = traffic.stop();

    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("didn't become ready"),
        "{}",
        lossy(&refused)
    );
    assert_eq!(deployment.server.running("api-blue"), blue);
    assert!(deployment.server.running("api-green").is_empty());
    assert_all_succeeded(&outcomes);
}

#[tokio::test]
async fn ops_16_a_deploy_restarts_the_proxy_only_when_the_operator_allows_it() {
    let database = TestDatabase::empty().await;
    let deployment = Deployment::new(&database, &[]);
    let first = deployment.apply(&database, &deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let caddy = deployment.server.running("caddy");
    let blue = deployment.server.running("api-blue");

    deployment.set(&[("TRUSTED_PROXIES", "203.0.113.0/24")]);
    let refused = deployment.apply(&database, &deployment.api, &[]).await;
    let refused_caddy = deployment.server.running("caddy");
    let allowed = deployment
        .apply(&database, &deployment.api, &["--allow-restart"])
        .await;

    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("changes caddy"),
        "{}",
        lossy(&refused)
    );
    assert_eq!(refused_caddy, caddy);
    assert_eq!(blue.len(), 2);
    assert!(allowed.status.success(), "{}", lossy(&allowed));
    let restarted = deployment.server.running("caddy");
    assert_eq!(restarted.len(), 1);
    assert_ne!(restarted, caddy);
}

#[tokio::test]
async fn ops_21_a_deploy_refuses_replicas_whose_connections_would_not_fit() {
    let database = TestDatabase::empty().await;
    let deployment = Deployment::new(&database, &[("DATABASE_MAX_CONNECTIONS", "40")]);

    let refused = deployment.apply(&database, &deployment.api, &[]).await;

    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("2 colors of 2 replicas, each opening up to 41 connections"),
        "{}",
        lossy(&refused)
    );
    assert!(deployment.server.running("caddy").is_empty());
    assert!(deployment.server.running("api-blue").is_empty());
}

#[tokio::test]
async fn ops_20_the_proxy_answers_http_with_a_redirect_to_https_and_https_with_hsts() {
    let database = TestDatabase::empty().await;
    let deployment = Deployment::new(&database, &[]);
    let deployed = deployment.apply(&database, &deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    let root = deployment.server.caddy_root();

    let (plain, plain_headers) = get(root.clone(), deployment.url("http", 80, "/v1/capabilities"));
    let (secure, secure_headers) = get(root, deployment.url("https", 443, "/v1/capabilities"));

    assert_eq!(plain, 308);
    let location = plain_headers["location"].to_str().unwrap();
    assert!(
        location.starts_with(&format!("https://{API_ADDRESS}")),
        "{location}"
    );
    assert_eq!(secure, 200);
    assert_eq!(
        secure_headers["strict-transport-security"],
        "max-age=31536000; includeSubDomains"
    );
    assert!(secure_headers.get("server").is_none());
}
