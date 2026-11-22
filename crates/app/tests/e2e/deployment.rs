//! A reference deployment on a server of the tests' own (TEST-03), whose database is managed, on
//! a Postgres server of its own beside the tests' server, or bundled, with its backups in a
//! stand-in bucket on the same server.

use std::{
    io::Write,
    path::PathBuf,
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

use crate::{
    docker::{Container, compose, docker, lossy, postgres_network},
    images::Pushed,
    memory,
    server::{DEPLOYMENT, RunningDeploy, Server},
    support::users,
};

/// The API's address in the tests: a name under `localhost`, for which Caddy signs the
/// certificate with an authority of its own instead of asking ACME.
pub const API_ADDRESS: &str = "api.localhost";
/// Grafana's address in the tests, under `localhost` as the API's is.
pub const GRAFANA_ADDRESS: &str = "grafana.localhost";

/// The name that the deployments' Grafana's administrator signs in with, which the operator
/// chooses at the setup.
pub const GRAFANA_ADMIN_USER: &str = "operator";

/// The password that the deployments' Grafana's administrator signs in with, which the operator
/// chooses at the setup.
pub const GRAFANA_ADMIN_PASSWORD: &str = "the password of the e2e tests' Grafana";

/// The line a restore writes once its first check has passed and it has marked its full backup,
/// before the replicas stop and its transaction commits.
pub const STOPPING: &str = "stopping the API's replicas";

/// The xtask binary, which `just e2e` builds before the tests run, into the profile's directory of
/// the target directory, target/<profile>/xtask: the one in the nearest directory above the
/// tests' own binary that holds one. Cargo keeps a test's binary under that directory, at a depth
/// that differs between its versions: target/<profile>/deps/e2e-<hash> up to 1.99, and
/// target/<profile>/build/app/<hash>/out/e2e-<hash> from 1.100 on. Where none is found, the path
/// beside the tests' binary, which names no file, so that starting it says so.
#[expect(
    clippy::disallowed_methods,
    reason = "tests may block, each on a runtime of its own (CODE-05)"
)]
fn xtask() -> PathBuf {
    let name = format!("xtask{}", std::env::consts::EXE_SUFFIX);
    let tests = std::env::current_exe().expect("the tests' binary");
    tests
        .ancestors()
        .skip(1)
        .map(|directory| directory.join(&name))
        .find(|candidate| candidate.is_file())
        .unwrap_or_else(|| tests.with_file_name(&name))
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
    /// Whether the settings that `set` last wrote make its server hold more than a share: they
    /// run the self-hosted stack, or the bundled database with telemetry that goes to a managed
    /// backend, whose stand-in is the collector's image on the server (memory.rs).
    heavy: AtomicBool,
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
        let authority = take_tls(&database, &host);
        server.hold_database(database);
        let deployment = Self::on(server, &[("DATABASE", "managed")], changes);
        deployment.server.write(
            &format!("{DEPLOYMENT}/certs/database-ca.crt"),
            &format!("{authority}\n"),
        );
        for (secret, setting) in [
            ("database_admin_url", "DATABASE_ADMIN_URL"),
            ("database_owner_url", "DATABASE_OWNER_URL"),
            ("database_url", "DATABASE_URL"),
        ] {
            let mut url = url::Url::parse(&std::env::var(setting).expect(setting)).expect(setting);
            url.set_host(Some(&host)).expect("a host");
            url.set_port(Some(5432)).expect("a port");
            // As deploy.md asks of a managed database's URLs (OPS-17), with the authority that
            // signed this one's certificate.
            url.set_query(Some(MANAGED_TLS));
            deployment
                .server
                .write(&format!("{DEPLOYMENT}/secrets/{secret}"), url.as_str());
        }
        deployment
    }

    /// A deployment, deployed, whose telemetry goes to a stand-in for a managed backend, which
    /// writes what it receives to /root/backend/out/telemetry.json.
    pub async fn with_a_managed_backend() -> Self {
        let deployment = Self::managed(&[]);
        let backend = deployment.server.start_otlp_backend();
        let mut changes = vec![("TELEMETRY", "managed")];
        changes.extend(
            backend
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
        );
        deployment.set(&changes);
        let deployed = deployment.apply(&deployment.api, &[]).await;
        assert!(deployed.status.success(), "{}", lossy(&deployed));
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
            heavy: AtomicBool::new(false),
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
            ("GRAFANA_ADMIN_USER", GRAFANA_ADMIN_USER),
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
        // The password of Grafana's administrator, which the operator chooses at the setup, for
        // the deploys that run the self-hosted stack.
        server.write(
            &format!("{DEPLOYMENT}/secrets/grafana_admin_password"),
            GRAFANA_ADMIN_PASSWORD,
        );
        let deployment = Self {
            server,
            api: Pushed::api().in_network(),
            postgres: Pushed::postgres().in_network(),
            base,
            heavy: AtomicBool::new(false),
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
        let is = |setting: &str, wanted: &str| {
            settings
                .iter()
                .any(|(name, value)| name == setting && value == wanted)
        };
        let heavy = is("TELEMETRY", "self-hosted")
            || (is("TELEMETRY", "managed") && is("DATABASE", "bundled"));
        self.heavy.store(heavy, Ordering::Relaxed);
        let text: String = settings
            .iter()
            .map(|(name, value)| format!("{name}={value}\n"))
            .collect();
        self.server
            .write(&format!("{DEPLOYMENT}/settings.env"), &text);
    }

    /// Deploys `api`, the API's image, with `options`. A deploy whose settings make its server
    /// hold more than a share first waits for the heavy turn (memory.rs).
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

    /// Waits for the heavy turn, once the settings make the server hold more than a share.
    async fn take_the_heavy_turn(&self) {
        if self.heavy.load(Ordering::Relaxed) {
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
        let (set, timeline) = self.restore_plan(moment, None);
        self.server.deploy(
            &[
                &["restore", moment.unwrap_or(""), &set, &timeline][..],
                options,
            ]
            .concat(),
        )
    }

    /// Starts a restore of the latest state, as `restore_with` does with `options`, and returns
    /// once it writes a line that contains [`STOPPING`].
    pub fn restore_until_stopping(&self, options: &[&str]) -> RunningDeploy {
        let (set, timeline) = self.restore_plan(None, None);
        self.server.deploy_until(
            &[&["restore", "", &set, &timeline][..], options].concat(),
            STOPPING,
        )
    }

    /// Restores from the backups that the bucket held at `held`, a time as pgBackRest takes one,
    /// to `moment`, or to the latest state that they hold, as `just restore` does with
    /// `--bucket-as-of` (OPS-22).
    pub fn restore_as_held(&self, moment: Option<&str>, held: &str) -> Output {
        let (set, timeline) = self.restore_plan(moment, Some(held));
        self.server.deploy(&[
            "restore",
            moment.unwrap_or(""),
            &set,
            &timeline,
            "--bucket-as-of",
            held,
        ])
    }

    /// The backup and the timeline of a restore to `moment`, or to the latest state, that xtask's
    /// `restore-plan` plans from what the bucket records, or, with `held`, recorded at that time.
    fn restore_plan(&self, moment: Option<&str>, held: Option<&str>) -> (String, String) {
        let inputs = self
            .server
            .deploy(&[&["restore-inputs"][..], held.as_slice()].concat());
        assert!(inputs.status.success(), "{}", lossy(&inputs));
        let as_held: Vec<&str> = held
            .iter()
            .flat_map(|held| ["--bucket-as-of", held])
            .collect();
        let mut planner = Command::new(xtask())
            .arg("restore-plan")
            .args(moment)
            .args(as_held)
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

/// Sends `GET` to `url`, through a client that trusts `root`, and says whether the server took
/// the connection and closed it without an answer.
pub fn unanswered(root: String, url: String) -> bool {
    thread::spawn(move || match client(&root).get(&url).send() {
        Ok(_) => false,
        Err(error) => !error.is_connect() && !error.is_timeout(),
    })
    .join()
    .expect("the request")
}

/// Sends `GET` to `url` with 101 headers beside the client's own, more than the 100 that a
/// replica's HTTP server takes (PLATFORM-099), through a client that trusts `root`, and returns
/// the response's status and headers.
pub fn get_with_too_many_headers(root: String, url: String) -> (u16, reqwest::header::HeaderMap) {
    thread::spawn(move || {
        let mut request = client(&root).get(&url);
        for index in 0..101 {
            request = request.header(format!("x-extra-{index}"), "1");
        }
        let response = request.send().expect("an answer");
        (response.status().as_u16(), response.headers().clone())
    })
    .join()
    .expect("the request")
}

/// Sends `GET` to `url` with `host` as its `Host`, through a client that trusts `root`, and says
/// whether the server took the connection and closed it without an answer. The client speaks
/// HTTP/1.1 alone, whose `Host` names the host that a request is for.
pub fn unanswered_as(root: String, url: String, host: &'static str) -> bool {
    thread::spawn(
        move || match client(&root).get(&url).header("host", host).send() {
            Ok(_) => false,
            Err(error) => !error.is_connect() && !error.is_timeout(),
        },
    )
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

/// Sends `GET` to `url` with `cookie` as its `Cookie` header, through a client that trusts
/// `root`, and returns the response's status.
pub fn get_with_cookie(root: String, url: String, cookie: String) -> u16 {
    thread::spawn(move || {
        let response = client(&root)
            .get(&url)
            .header("cookie", cookie)
            .send()
            .expect("an answer");
        response.status().as_u16()
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

/// Sends `POST` to `url` with the JSON body `body`, through a client that trusts `root`, and
/// returns the response's status and each cookie it sets, as its `Set-Cookie` writes it.
pub fn post_for_cookies(root: String, url: String, body: String) -> (u16, Vec<String>) {
    thread::spawn(move || {
        let response = client(&root)
            .post(&url)
            .header("content-type", "application/json")
            .body(body)
            .send()
            .expect("an answer");
        let cookies = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .map(|cookie| cookie.to_str().expect("a cookie in ASCII").to_owned())
            .collect();
        (response.status().as_u16(), cookies)
    })
    .join()
    .expect("the request")
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

// part: client_telemetry
/// Sends `PUT` to `url` with the JSON body `body`, with `token` as its Bearer access token,
/// through a client that trusts `root`, and returns the response's status.
pub fn put_as(root: String, url: String, token: String, body: String) -> u16 {
    thread::spawn(move || {
        let response = client(&root)
            .put(&url)
            .header("content-type", "application/json")
            .bearer_auth(token)
            .body(body)
            .send()
            .expect("an answer");
        response.status().as_u16()
    })
    .join()
    .expect("the request")
}
// end part: client_telemetry

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

/// The query of a managed database's URL in the tests: TLS that checks the server's certificate
/// and its address, against the authority that `Deployment::managed` gives the deployment.
pub const MANAGED_TLS: &str =
    "sslmode=verify-full&sslrootcert=/etc/deployment-certs/database-ca.crt";

/// Has the managed database's server `database` take TLS connections, as a provider's does, with
/// a certificate for `host`, the address that the deployment reaches it at, which an authority
/// of the test's own signs, and returns that authority's certificate, in PEM.
fn take_tls(database: &Container, host: &str) -> String {
    let script = format!(
        "set -e
        mkdir /tmp/tls && cd /tmp/tls
        openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.crt -days 1 -subj /CN=e2e-database-authority 2> /dev/null
        openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr -subj /CN={host} 2> /dev/null
        echo subjectAltName=IP:{host} > san
        openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out server.crt -days 1 -extfile san 2> /dev/null
        chmod 600 server.key
        psql --username=postgres --dbname=api -qAt -v ON_ERROR_STOP=1 \
            -c \"ALTER SYSTEM SET ssl_cert_file = '/tmp/tls/server.crt'\" \
            -c \"ALTER SYSTEM SET ssl_key_file = '/tmp/tls/server.key'\" \
            -c 'ALTER SYSTEM SET ssl = on' -c 'SELECT pg_reload_conf()' > /dev/null
        cat ca.crt"
    );
    let authority = docker(&[
        "exec",
        "--user",
        "postgres",
        database.id(),
        "sh",
        "-c",
        &script,
    ]);
    assert!(authority.contains("BEGIN CERTIFICATE"), "{authority}");
    authority
}

/// Starts the server of a managed database: another server of the development Postgres, for the reason
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
    /// Registers an owner on the deployed API, and arranges their workspace, as
    /// [`Deployment::owned_workspace`] does.
    pub fn workspace(&self) -> Workspace {
        let root = self.server.caddy_root();
        let token = self.registered("ada@example.com");
        let workspace = self.owned_workspace("Acme", "ada@example.com");
        let files = self.url("https", 443, &format!("/v1/workspaces/{workspace}/files"));
        Workspace { root, files, token }
    }
}
// end part: files

impl Deployment {
    /// Registers a user of `email` on the deployed API, and returns their access token.
    pub fn registered(&self, email: &str) -> String {
        let (registered, session) = post(
            self.server.caddy_root(),
            self.url("https", 443, "/v1/users"),
            users::registration(email).to_string(),
        );
        assert_eq!(registered, 201, "{session}");
        let session: Value = serde_json::from_str(&session).expect("a session");
        session["access_token"]
            .as_str()
            .expect("a token")
            .to_owned()
    }

    /// Arranges a workspace named `name` in the deployment's database, owned by the account that
    /// holds `email`, and returns its ID: through SQL, as the integration tests arrange theirs,
    /// rather than through one of the parts that create workspaces, so the tests hold whichever
    /// of them a fork keeps (TEST-09).
    pub fn owned_workspace(&self, name: &str, email: &str) -> String {
        let workspace = self.sql(&format!(
            "WITH created AS (INSERT INTO workspaces (name) VALUES ('{name}') RETURNING id), \
             joined AS (INSERT INTO workspace_memberships (workspace_id, user_id, role) \
             SELECT created.id, users.id, 'owner' FROM created, users \
             WHERE users.email = '{email}' RETURNING workspace_id) \
             SELECT workspace_id FROM joined"
        ));
        let workspace = workspace.trim().to_owned();
        assert_eq!(workspace.len(), 36, "no workspace for {email}: {workspace}");
        workspace
    }
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
