//! A reference deployment on a server of the tests' own (TEST-03), whose database is managed, on
//! a Postgres server of its own beside the tests' server, or bundled, with its backups in a
//! stand-in bucket on the same server.

use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use rustls::pki_types::{CertificateDer, pem::PemObject};
use serde_json::Value;
// part: files
use serde_json::json;
// end part: files

use crate::{
    docker::{Container, compose, docker, lossy, postgres_network},
    images::Pushed,
    memory,
    server::{DEPLOYMENT, RunningDeploy, Server},
};

/// The API's address in the tests: a name under `localhost`, for which Caddy signs the
/// certificate with an authority of its own instead of asking ACME.
pub const API_ADDRESS: &str = "api.localhost";
/// Grafana's address in the tests, under `localhost` as the API's is.
pub const GRAFANA_ADDRESS: &str = "grafana.localhost";

/// The xtask binary, which `just e2e` builds before the tests run, in the target directory and
/// profile the tests' own binary is in: target/<profile>/deps/e2e-<hash> beside
/// target/<profile>/xtask.
fn xtask() -> PathBuf {
    std::env::current_exe()
        .expect("the tests' binary")
        .parent()
        .and_then(Path::parent)
        .expect("the target directory's profile")
        .join(format!("xtask{}", std::env::consts::EXE_SUFFIX))
}

/// A deployment on a server of its own.
pub struct Deployment {
    pub server: Server,
    /// The API's image under test, as the server pulls it.
    pub api: String,
    /// The deployment's Postgres image under test, as the server pulls it.
    pub postgres: String,
    /// The settings of its kind of deployment, which `changes` of `set` add to or replace.
    base: Vec<(String, String)>,
    /// Whether the settings that `set` last wrote run the self-hosted stack.
    runs_stack: AtomicBool,
}

impl Deployment {
    /// A deployment whose managed database is on a new Postgres server, with the tests' settings
    /// and `changes`.
    pub fn managed(changes: &[(&str, &str)]) -> Self {
        // The server first, so the shares of memory bound how many databases' servers run, and
        // the server holds its database's, so its share goes once both have.
        let mut server = Server::start();
        let database = database_server(server.in_memory());
        let host = database.inspect("{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}");
        server.hold_database(database);
        let deployment = Self::on(server, &[("DATABASE", "managed")], changes);
        for (secret, setting) in [
            ("database_admin_url", "DATABASE_ADMIN_URL"),
            ("database_owner_url", "DATABASE_OWNER_URL"),
            ("database_url", "DATABASE_URL"),
        ] {
            let mut url = url::Url::parse(&std::env::var(setting).expect(setting)).expect(setting);
            url.set_host(Some(&host)).expect("a host");
            url.set_port(Some(5432)).expect("a port");
            deployment
                .server
                .write(&format!("{DEPLOYMENT}/secrets/{secret}"), url.as_str());
        }
        deployment
    }

    /// A deployment whose database is the bundled Postgres, which backs up to a stand-in bucket
    /// on the same server, with the tests' settings and `changes`.
    pub fn bundled(changes: &[(&str, &str)]) -> Self {
        Self::bundled_on(Server::start(), changes)
    }

    /// A deployment whose database is the bundled Postgres, as `bundled` makes one, on `server`.
    pub fn bundled_on(server: Server, changes: &[(&str, &str)]) -> Self {
        let bucket = server.start_bucket();
        let mut kind = vec![("DATABASE".to_owned(), "bundled".to_owned())];
        kind.extend(bucket);
        let kind: Vec<(&str, &str)> = kind
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        Self::on(server, &kind, changes)
    }

    /// A deployment of this one's kind on `server`, a new one, set up as backups.md says after
    /// losing a server: with this one's settings, and the copies of its secrets and of the
    /// bucket's authority, so it backs up to this one's bucket. The database must be bundled.
    pub fn on_a_new_server(&self, server: Server) -> Self {
        assert!(self.server.database().is_none(), "a bundled database");
        let deployment = Self {
            server,
            api: self.api.clone(),
            postgres: self.postgres.clone(),
            base: self.base.clone(),
            runs_stack: AtomicBool::new(false),
        };
        deployment.set(&[]);
        self.server
            .copy_to(&deployment.server, &["secrets", "certs"]);
        // part: files
        // The new server reaches this one's files bucket, so it starts none whose start would give
        // its scanner the signature.
        deployment.server.seed_signatures();
        // end part: files
        deployment
    }

    fn on(server: Server, kind: &[(&str, &str)], changes: &[(&str, &str)]) -> Self {
        let mut base: Vec<(String, String)> = [
            ("IMAGE_REPOSITORY", "registry:5000/image"),
            ("SOURCE", "https://example.invalid/e2e"),
            ("API_ADDRESS", API_ADDRESS),
            ("GRAFANA_ADDRESS", GRAFANA_ADDRESS),
            ("TELEMETRY", "off"),
            // A test that sends email starts a mail server on the server, and sets its own.
            ("EMAIL", "off"),
            ("API_REPLICAS", "2"),
            ("DATABASE_MAX_CONNECTIONS", "2"),
            ("POSTGRES_MAX_CONNECTIONS", "100"),
            // The tests' traffic comes from one address, far faster than a client's limit allows;
            // the integration tests cover the limit (PLATFORM-054).
            ("RATE_LIMIT_IP_REQUESTS", "1000000"),
        ]
        .iter()
        .chain(kind)
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect();
        // part: files
        // Every deployment's replicas reach a files bucket (the files spec's "Storage").
        base.extend(server.start_files_bucket());
        // end part: files
        base.sort();
        let deployment = Self {
            server,
            api: Pushed::api().in_network(),
            postgres: Pushed::postgres().in_network(),
            base,
            runs_stack: AtomicBool::new(false),
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

    /// Deploys `api`, the API's image, with `options`. A deploy that runs the self-hosted stack
    /// first waits for the heavy turn (memory.rs).
    pub async fn apply(&self, api: &str, options: &[&str]) -> Output {
        self.apply_images(api, &self.postgres, options).await
    }

    /// Deploys `api`, the API's image, and `postgres`, the deployment's Postgres image, with
    /// `options`, as `apply` does.
    pub async fn apply_images(&self, api: &str, postgres: &str, options: &[&str]) -> Output {
        self.take_the_heavy_turn().await;
        let mut args = vec!["apply", api, postgres];
        args.extend(options);
        self.server.deploy(&args)
    }

    /// Starts a deploy of `api`, the API's image, as `apply` does, and returns once it writes a
    /// line that contains `line`, for the test to stop it there.
    pub async fn apply_until(&self, api: &str, line: &str) -> RunningDeploy {
        self.take_the_heavy_turn().await;
        self.server
            .deploy_until(&["apply", api, &self.postgres], line)
    }

    /// Waits for the heavy turn, once the settings run the self-hosted stack.
    async fn take_the_heavy_turn(&self) {
        if self.runs_stack.load(Ordering::Relaxed) {
            self.server.take_the_heavy_turn().await;
        }
    }

    /// The path of the API's binary in its image under test, its entry point, so a test names
    /// no binary of its own.
    pub fn api_binary(&self) -> String {
        self.server.shell_ok(&format!(
            "docker pull --quiet {api} > /dev/null \
             && docker image inspect --format '{{{{index .Config.Entrypoint 0}}}}' {api}",
            api = self.api
        ))
    }

    /// The image `base`, such as the API's under test, with the Dockerfile `instructions` added,
    /// built on the server and pushed to Compose's registry as `name`. Returns it by its digest.
    pub fn image_from(&self, base: &str, name: &str, instructions: &str) -> String {
        self.image_tagged(base, name, "e2e", instructions)
    }

    /// The image `base` with the Dockerfile `instructions` added, as `image_from` builds it,
    /// pushed as the tag `tag` of `name`, so images of one repository can differ, as releases of
    /// the API do. Returns it by its digest.
    pub fn image_tagged(&self, base: &str, name: &str, tag: &str, instructions: &str) -> String {
        self.server.write(
            &format!("/root/{name}-{tag}/Dockerfile"),
            &format!("FROM {base}\n{instructions}\n"),
        );
        self.server.shell_ok(&format!(
            "docker build --quiet --tag registry:5000/{name}:{tag} /root/{name}-{tag} > /dev/null \
             && docker push --quiet registry:5000/{name}:{tag} > /dev/null \
             && docker image inspect --format '{{{{index .RepoDigests 0}}}}' \
                registry:5000/{name}:{tag}"
        ))
    }

    /// Restores the bundled database as `just restore` does: to `moment`, or, without one, to the
    /// latest state the backups hold, from the backup and along the timeline that xtask's
    /// `restore-plan` plans from what the bucket records.
    pub fn restore(&self, moment: Option<&str>) -> Output {
        self.restore_with(moment, &[])
    }

    /// Restores as `restore` does, with deploy.sh's `options`.
    pub fn restore_with(&self, moment: Option<&str>, options: &[&str]) -> Output {
        let (set, timeline) = self.restore_plan(moment);
        self.server.deploy(
            &[
                &["restore", moment.unwrap_or(""), &set, &timeline][..],
                options,
            ]
            .concat(),
        )
    }

    /// Starts a restore of the latest state, as `restore_with` does with `options`, and returns
    /// once it writes a line that contains `line`.
    pub fn restore_until(&self, options: &[&str], line: &str) -> RunningDeploy {
        let (set, timeline) = self.restore_plan(None);
        self.server.deploy_until(
            &[&["restore", "", &set, &timeline][..], options].concat(),
            line,
        )
    }

    /// The backup and the timeline of a restore to `moment`, or to the latest state, that xtask's
    /// `restore-plan` plans from what the bucket records.
    fn restore_plan(&self, moment: Option<&str>) -> (String, String) {
        let inputs = self.server.deploy(&["restore-inputs"]);
        assert!(inputs.status.success(), "{}", lossy(&inputs));
        let mut planner = Command::new(xtask())
            .arg("restore-plan")
            .args(moment)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("xtask, which `just e2e` builds before the tests run");
        planner
            .stdin
            .take()
            .expect("the planner's input")
            .write_all(&inputs.stdout)
            .expect("the bucket's records, for the planner");
        let planned = planner.wait_with_output().expect("the plan");
        assert!(planned.status.success(), "{}", lossy(&planned));
        let plan = String::from_utf8_lossy(&planned.stdout).trim().to_owned();
        let (set, timeline) = plan.split_once(' ').expect("a backup and a timeline");
        (set.to_owned(), timeline.to_owned())
    }

    /// The first column of each row that `sql` returns from the deployment's database, the
    /// bundled one or the managed one on its server, as its administrator, one row per line.
    pub fn sql(&self, sql: &str) -> String {
        if let Some(database) = self.server.database() {
            return docker(&[
                "exec",
                database.id(),
                "psql",
                "--username=postgres",
                "--dbname=api",
                "-qAt",
                "-v",
                "ON_ERROR_STOP=1",
                "--command",
                sql,
            ]);
        }
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
    client_within(root, Duration::from_secs(10))
}

/// A client that trusts `root`, Caddy's authority, and gives up on an answer after `timeout`.
fn client_within(root: &str, timeout: Duration) -> reqwest::blocking::Client {
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
        .timeout(timeout)
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

/// Sends `GET` to `url`, through a client that trusts `root` and waits at most `timeout` for
/// the answer, and returns the response's status, headers and body.
pub fn get_within(
    root: String,
    url: String,
    timeout: Duration,
) -> (u16, reqwest::header::HeaderMap, String) {
    thread::spawn(move || {
        let response = client_within(&root, timeout)
            .get(&url)
            .send()
            .expect("an answer in time");
        let status = response.status().as_u16();
        let headers = response.headers().clone();
        (status, headers, response.text().expect("the body"))
    })
    .join()
    .expect("the request")
}

/// Sends `GET` to `url` with the trace context `traceparent`, through a client that trusts
/// `root`, and returns the response's status and headers.
pub fn get_traced(
    root: String,
    url: String,
    traceparent: String,
) -> (u16, reqwest::header::HeaderMap) {
    thread::spawn(move || {
        let response = client(&root)
            .get(&url)
            .header("traceparent", traceparent)
            .send()
            .expect("an answer");
        (response.status().as_u16(), response.headers().clone())
    })
    .join()
    .expect("the request")
}

/// Sends `POST` to `url` with the JSON body `body`, through a client that trusts `root`, and
/// returns the response's status and body.
pub fn post(root: String, url: String, body: String) -> (u16, String) {
    post_as(root, url, None, body)
}

/// Sends `POST` to `url` with the JSON body `body`, with `token` as its Bearer access token if
/// given, through a client that trusts `root`, and returns the response's status and body.
pub fn post_as(root: String, url: String, token: Option<String>, body: String) -> (u16, String) {
    thread::spawn(move || {
        let mut request = client(&root)
            .post(&url)
            .header("content-type", "application/json")
            .body(body);
        if let Some(token) = token {
            request = request.bearer_auth(token);
        }
        let response = request.send().expect("an answer");
        let status = response.status().as_u16();
        (status, response.text().expect("the body"))
    })
    .join()
    .expect("the request")
}

/// Sends `DELETE` to `url`, with `token` as its Bearer access token, through a client that trusts
/// `root`, and returns the response's status.
pub fn delete_as(root: String, url: String, token: String) -> u16 {
    thread::spawn(move || {
        let response = client(&root)
            .delete(&url)
            .bearer_auth(token)
            .send()
            .expect("an answer");
        response.status().as_u16()
    })
    .join()
    .expect("the request")
}

/// A client of the API that trusts `root`, Caddy's authority, and gives up on each read of an
/// answer after `timeout`, for a stream whose answer never ends.
pub fn streaming_client(root: &str, timeout: Duration) -> reqwest::blocking::Client {
    client_within(root, timeout)
}

/// Starts the server of a managed database: another server of Compose's Postgres, for the reason
/// docs/operations/ci.md gives. It runs the service's image with its command and environment, and
/// its health check's test, interval, timeout and retries, joins its network, and keeps its data
/// in a volume of its own, which goes with it: `in_memory`, under a limit in its deployment's
/// share (D84), where the servers keep their stores in memory.
fn database_server(in_memory: bool) -> Container {
    let config: Value = serde_json::from_str(&compose(&["config", "--format", "json", "postgres"]))
        .expect("Compose's configuration");
    let service = &config["services"]["postgres"];
    let text = |value: &Value| value.as_str().expect("a string").to_owned();
    let health = &service["healthcheck"];
    let test: Vec<String> = health["test"]
        .as_array()
        .expect("Postgres's health check")
        .iter()
        .map(text)
        .collect();
    let (form, check) = test.split_first().expect("a health check");
    assert_eq!(form, "CMD", "a health check that names its program");
    let mut args = vec!["--network".to_owned(), postgres_network()];
    if in_memory {
        let limit = format!("{}m", memory::DATABASE_MIB);
        args.extend([
            "--memory".to_owned(),
            limit.clone(),
            "--memory-swap".to_owned(),
            limit,
            "--mount".to_owned(),
            "type=tmpfs,destination=/var/lib/postgresql".to_owned(),
        ]);
    }
    for (name, value) in service["environment"]
        .as_object()
        .expect("Postgres's environment")
    {
        args.extend(["--env".to_owned(), format!("{name}={}", text(value))]);
    }
    args.extend([
        "--health-cmd".to_owned(),
        check.join(" "),
        "--health-interval".to_owned(),
        text(&health["interval"]),
        "--health-timeout".to_owned(),
        text(&health["timeout"]),
        "--health-retries".to_owned(),
        health["retries"].to_string(),
        text(&service["image"]),
    ]);
    args.extend(
        service["command"]
            .as_array()
            .expect("Postgres's command")
            .iter()
            .map(text),
    );
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let container = Container::start(&args);
    assert_eq!(container.health(), "healthy", "{}", container.logs());
    container
}

// part: files
/// A workspace on a deployed API, whose owner is signed in: its files' URL, the owner's access
/// token, and the root certificate, in PEM, that a client trusts.
pub struct Workspace {
    pub root: String,
    pub files: String,
    pub token: String,
}

impl Deployment {
    /// Registers an owner on the deployed API, and creates their workspace.
    pub fn workspace(&self) -> Workspace {
        let root = self.server.caddy_root();
        let (registered, session) = post(
            root.clone(),
            self.url("https", 443, "/v1/users"),
            json!({ "email": "ada@example.com", "password": "plum-tractor-41-quiet", "name": "Ada" })
                .to_string(),
        );
        assert_eq!(registered, 201, "{session}");
        let token = serde_json::from_str::<Value>(&session).unwrap()["access_token"]
            .as_str()
            .unwrap()
            .to_owned();
        let (created, workspace) = post_as(
            root.clone(),
            self.url("https", 443, "/v1/workspaces"),
            Some(token.clone()),
            json!({ "name": "Acme" }).to_string(),
        );
        assert_eq!(created, 201, "{workspace}");
        let workspace = serde_json::from_str::<Value>(&workspace).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let files = self.url("https", 443, &format!("/v1/workspaces/{workspace}/files"));
        Workspace { root, files, token }
    }
}
// end part: files

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
