//! The server the end-to-end tests deploy the reference deployment to: Docker in a container of
//! its own, joined to Compose's network, where it pulls the API's and the deployment's Postgres
//! from the registry that Compose runs (TEST-04), and Docker Hub's images through the mirror that
//! Compose runs (TEST-07). It runs the deployment as a server's Docker would, with the
//! deployment's scripts in its shell, and publishes Caddy's ports on 127.0.0.1.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::SocketAddr,
    path::Path,
    process::{Child, ChildStdout, Command, Output, Stdio},
};

use crate::docker::{Container, compose, docker, lossy, postgres_network};

/// Where the server holds the release's deployment files, as `just deploy` copies them.
const RELEASE: &str = "/root/release";
/// The deployment's directory on the server.
pub const DEPLOYMENT: &str = "/root/deployment";
/// Where the server holds the ID of the process group of the deploy that `deploy_until` started.
const DEPLOY_GROUP: &str = "/root/deploy-group";

pub struct Server {
    container: Container,
}

/// A deploy that `Server::deploy_until` started and that still runs.
pub struct RunningDeploy {
    child: Child,
    output: BufReader<ChildStdout>,
    /// What it wrote so far, stdout and stderr together.
    written: String,
}

impl RunningDeploy {
    /// Sends `signal` to the deploy and every process it started, as Ctrl-C does with `INT` and a
    /// crash with `KILL`, and returns what the deploy wrote once they have all ended.
    pub fn stop(mut self, server: &Server, signal: &str) -> String {
        server.shell_ok(&format!("kill -{signal} -$(cat {DEPLOY_GROUP})"));
        self.output
            .read_to_string(&mut self.written)
            .expect("read deploy.sh's output");
        self.child.wait().expect("wait for deploy.sh");
        // The processes it started may end a moment after it. One that has ended stays listed, as
        // a zombie (Z), until a parent waits for it.
        server.shell_ok(&format!(
            "group=$(cat {DEPLOY_GROUP}) && for attempt in $(seq 30); do \
                 cat /proc/[0-9]*/stat 2> /dev/null \
                     | awk -v group=\"$group\" '$5 == group && $3 != \"Z\" {{ found = 1 }} END {{ exit !found }}' \
                     || exit 0; \
                 sleep 1; \
             done; exit 1"
        ));
        self.written
    }
}

impl Drop for Server {
    /// Removes every container on the server through the server's own Docker, before the
    /// server's container goes. Killing the server with the deployment inside would make the
    /// kernel end every process of every container at once before the server's own process can
    /// exit, which on a busy machine takes longer than Docker waits for a container to exit after
    /// its kill, and the removal fails with "did not receive an exit event".
    fn drop(&mut self) {
        let _removed = Command::new("docker")
            .args(["exec", self.container.id(), "sh", "-c"])
            .arg("docker ps --all --quiet | xargs -r docker rm --force --volumes > /dev/null")
            .output();
    }
}

impl Server {
    /// Starts a server, waits until its Docker answers, and copies the repository's deployment
    /// files to it.
    pub fn start() -> Self {
        let image = compose(&["config", "--images", "server"]);
        let network = postgres_network();
        let container = Container::start(&[
            "--privileged",
            "--network",
            &network,
            // Docker listens only on its socket, with no certificates to make.
            "--env",
            "DOCKER_TLS_CERTDIR=",
            "--publish",
            "127.0.0.1::80",
            "--publish",
            "127.0.0.1::443",
            "--health-cmd",
            "docker info",
            "--health-interval",
            "5s",
            "--health-start-period",
            "120s",
            "--health-start-interval",
            "1s",
            "--health-retries",
            "3",
            // Docker Hub's images come through the mirror, which `just e2e` filled, and Docker
            // Hub itself is out of reach (TEST-07).
            "--add-host",
            "registry-1.docker.io:127.0.0.1",
            &image,
            "--insecure-registry=registry:5000",
            "--registry-mirror=http://mirror:5000",
        ]);
        assert_eq!(container.health(), "healthy", "{}", container.logs());
        let deploy = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/.");
        docker(&[
            "cp",
            &deploy.to_string_lossy(),
            &format!("{}:{RELEASE}", container.id()),
        ]);
        Self { container }
    }

    /// Writes `contents` to the file at `path` on the server.
    pub fn write(&self, path: &str, contents: &str) {
        let mut child = Command::new("docker")
            .args(["exec", "--interactive", self.container.id(), "sh", "-c"])
            .arg("mkdir -p \"$(dirname \"$1\")\" && cat > \"$1\"")
            .args(["sh", path])
            .stdin(Stdio::piped())
            .spawn()
            .expect("run docker exec");
        child
            .stdin
            .take()
            .expect("its input")
            .write_all(contents.as_bytes())
            .expect("write the file");
        assert!(
            child.wait().expect("wait for docker exec").success(),
            "write {path}"
        );
    }

    /// Runs `command` in the server's shell.
    pub fn shell(&self, command: &str) -> Output {
        Command::new("docker")
            .args(["exec", self.container.id(), "sh", "-c", command])
            .output()
            .expect("run docker exec")
    }

    /// Runs `command` in the server's shell, and returns what it wrote on stdout, trimmed. Panics
    /// with what it wrote if it fails.
    pub fn shell_ok(&self, command: &str) -> String {
        let output = self.shell(command);
        assert!(output.status.success(), "{command}: {}", lossy(&output));
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// Runs the release's deploy.sh with `args`, as `just deploy` does.
    pub fn deploy(&self, args: &[&str]) -> Output {
        Command::new("docker")
            .args(["exec", "--env", &format!("DEPLOYMENT={DEPLOYMENT}")])
            .args([self.container.id(), "sh", &format!("{RELEASE}/deploy.sh")])
            .args(args)
            .output()
            .expect("run deploy.sh")
    }

    /// Starts the release's deploy.sh with `args`, as `deploy` does, and returns once it writes a
    /// line that contains `line`, for the test to stop it there.
    pub fn deploy_until(&self, args: &[&str], line: &str) -> RunningDeploy {
        // The process that docker exec starts leads a process group of its own, which deploy.sh,
        // in its place, and every process it starts belong to.
        let mut child = Command::new("docker")
            .args(["exec", "--env", &format!("DEPLOYMENT={DEPLOYMENT}")])
            .args([self.container.id(), "sh", "-c"])
            .arg(format!(
                "echo $$ > {DEPLOY_GROUP} && exec sh {RELEASE}/deploy.sh \"$@\" 2>&1"
            ))
            .arg("sh")
            .args(args)
            .stdout(Stdio::piped())
            .spawn()
            .expect("run deploy.sh");
        let mut output = BufReader::new(child.stdout.take().expect("its output"));
        let mut written = String::new();
        loop {
            let start = written.len();
            let read = output
                .read_line(&mut written)
                .expect("read deploy.sh's output");
            assert!(
                read > 0,
                "deploy.sh ended before it wrote {line:?}: {written}"
            );
            if written[start..].contains(line) {
                break;
            }
        }
        RunningDeploy {
            child,
            output,
            written,
        }
    }

    /// What a service of the deployment's telemetry answers to `url`, such as Tempo's or Loki's
    /// API, with the request headers `headers`, once its answer holds `text`, asked from the
    /// deployment's telemetry network every two seconds for at most three minutes: the collector
    /// decides on a trace 15 seconds after its last span. Panics with the last answer if none
    /// holds `text`.
    pub fn telemetry_once_it_holds(&self, url: &str, headers: &[&str], text: &str) -> String {
        let headers: String = headers
            .iter()
            .map(|header| format!(" --header '{header}'"))
            .collect();
        let curl = compose(&[
            "--file",
            "../../deploy/tools/compose.yaml",
            "config",
            "--images",
            "curl",
        ]);
        self.shell_ok(&format!(
            "for attempt in $(seq 90); do \
                 answer=$(docker run --rm --quiet --network deployment_telemetry {curl} --silent{headers} '{url}') || true; \
                 case $answer in *\"{text}\"*) printf '%s' \"$answer\"; exit 0 ;; esac; \
                 sleep 2; \
             done; printf '%s' \"$answer\"; exit 1"
        ))
    }

    /// The IDs of the running containers of the deployment's service `service`.
    pub fn running(&self, service: &str) -> Vec<String> {
        self.shell_ok(&format!(
            "docker ps --quiet --filter label=com.docker.compose.project=deployment \
             --filter label=com.docker.compose.service={service} --filter status=running"
        ))
        .lines()
        .map(str::to_owned)
        .collect()
    }

    /// The certificate of the authority that signs Caddy's certificates for names under
    /// `localhost`, which the tests trust.
    pub fn caddy_root(&self) -> String {
        self.shell_ok(
            "docker exec deployment-caddy-1 cat /data/caddy/pki/authorities/local/root.crt",
        )
    }

    /// The address on this machine at which the server's port `port` is published.
    pub fn address(&self, port: u16) -> SocketAddr {
        self.container.address(port)
    }

    /// Copies each of `paths`, a file or a directory of the deployment's directory, to the same
    /// place on `other`.
    pub fn copy_to(&self, other: &Server, paths: &[&str]) {
        for path in paths {
            let mut from = Command::new("docker")
                .args([
                    "cp",
                    &format!("{}:{DEPLOYMENT}/{path}", self.container.id()),
                    "-",
                ])
                .stdout(Stdio::piped())
                .spawn()
                .expect("run docker cp");
            let to = Command::new("docker")
                .args(["cp", "-", &format!("{}:{DEPLOYMENT}", other.container.id())])
                .stdin(from.stdout.take().expect("its output"))
                .status()
                .expect("run docker cp");
            assert!(
                from.wait().expect("wait for docker cp").success() && to.success(),
                "copy {path}"
            );
        }
    }

    /// Starts the stand-in for a managed observability backend on the server: a collector that
    /// receives OTLP over HTTPS on the server's own address, port 4319, with a certificate from
    /// an authority of the test's own, which the deployment trusts as
    /// certs/managed-otlp-ca.crt, admits only the user `e2e` with the token the deployment holds,
    /// and writes what it receives to /root/backend/out/telemetry.json. Returns the deployment's
    /// settings for it.
    pub fn start_otlp_backend(&self) -> Vec<(String, String)> {
        // From the file development and the deployment share, where the collector depends on no
        // other service whose image the listing would add.
        let image = compose(&[
            "--file",
            "../../deploy/observability/compose.yaml",
            "config",
            "--images",
            "collector",
        ]);
        let address = self.issue_certificate("/root/backend", "e2e-backend-authority");
        self.write(
            "/root/backend/config.yaml",
            "extensions:\n  basicauth/server:\n    htpasswd:\n      inline: e2e:the-backend-token\n\
             receivers:\n  otlp:\n    protocols:\n      http:\n        endpoint: 0.0.0.0:4318\n        \
             tls:\n          cert_file: /backend/server.crt\n          key_file: /backend/server.key\n        \
             auth:\n          authenticator: basicauth/server\n\
             exporters:\n  file:\n    path: /backend/out/telemetry.json\n\
             service:\n  extensions: [basicauth/server]\n  pipelines:\n    \
             traces:\n      receivers: [otlp]\n      exporters: [file]\n    \
             logs:\n      receivers: [otlp]\n      exporters: [file]\n",
        );
        self.write(
            &format!("{DEPLOYMENT}/secrets/managed_otlp_token"),
            "the-backend-token",
        );
        self.shell_ok(&format!(
            "set -e
            cd /root/backend
            mkdir -p out && chmod 777 out
            mkdir -p {DEPLOYMENT}/certs
            cp ca.crt {DEPLOYMENT}/certs/managed-otlp-ca.crt
            docker run --detach --quiet --name backend --publish 4319:4318 --volume /root/backend:/backend {image} \
                --config=/backend/config.yaml > /dev/null"
        ));
        [
            ("MANAGED_OTLP_ENDPOINT", format!("https://{address}:4319")),
            ("MANAGED_OTLP_USER", "e2e".to_owned()),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_owned(), value))
        .collect()
    }

    /// Starts the stand-in for a managed backend's Grafana on the server: the Grafana that the
    /// self-hosted stack runs, serving HTTPS on the server's own address, port 3443, with a
    /// certificate from an authority of the test's own, which the deployment trusts as
    /// certs/managed-grafana-ca.crt, and with data sources under Grafana Cloud's uids. Writes the
    /// token of a service account with the Admin role, which the import signs in with, and
    /// returns the deployment's setting for it.
    pub fn start_managed_grafana(&self) -> Vec<(String, String)> {
        let image = compose(&[
            "--file",
            "../../deploy/observability/compose.yaml",
            "config",
            "--images",
            "grafana",
        ]);
        let address = self.issue_certificate("/root/grafana", "e2e-grafana-authority");
        self.write(
            "/root/grafana/datasources/datasources.yaml",
            "apiVersion: 1\ndatasources:\n\
             \x20 - {name: prom, uid: grafanacloud-prom, type: prometheus, url: http://127.0.0.1:9090, isDefault: true}\n\
             \x20 - {name: logs, uid: grafanacloud-logs, type: loki, url: http://127.0.0.1:3100}\n\
             \x20 - {name: traces, uid: grafanacloud-traces, type: tempo, url: http://127.0.0.1:3200}\n",
        );
        self.shell_ok(&format!(
            "set -e
            mkdir -p {DEPLOYMENT}/certs
            cp /root/grafana/ca.crt {DEPLOYMENT}/certs/managed-grafana-ca.crt
            docker run --detach --quiet --name grafana --publish 3443:3000 --volume /root/grafana:/grafana:ro \
                --volume /root/grafana/datasources:/etc/grafana/provisioning/datasources:ro \
                --env GF_SERVER_PROTOCOL=https --env GF_SERVER_CERT_FILE=/grafana/server.crt \
                --env GF_SERVER_CERT_KEY=/grafana/server.key {image} > /dev/null"
        ));
        // Grafana's first start migrates its database, which takes more than a minute on a
        // machine that runs the suite's other servers too.
        self.managed_grafana_until_ready();
        let account = self.managed_grafana(
            "POST",
            "/api/serviceaccounts",
            r#"{"name": "import", "role": "Admin"}"#,
        );
        let account: serde_json::Value = serde_json::from_str(&account).expect("the account");
        let token = self.managed_grafana(
            "POST",
            &format!("/api/serviceaccounts/{}/tokens", account["id"]),
            r#"{"name": "import"}"#,
        );
        let token: serde_json::Value = serde_json::from_str(&token).expect("the token");
        self.write(
            &format!("{DEPLOYMENT}/secrets/managed_grafana_token"),
            token["key"].as_str().expect("the token's key"),
        );
        vec![(
            "MANAGED_GRAFANA_URL".to_owned(),
            format!("https://{address}:3443"),
        )]
    }

    /// Waits until the stand-in managed Grafana answers its health check, for at most four
    /// minutes.
    fn managed_grafana_until_ready(&self) {
        let curl = compose(&[
            "--file",
            "../../deploy/tools/compose.yaml",
            "config",
            "--images",
            "curl",
        ]);
        let address = self
            .container
            .inspect("{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}");
        self.shell_ok(&format!(
            "docker run --rm --quiet --volume /root/grafana/ca.crt:/ca.crt:ro {curl} --silent \
                --show-error --fail --retry 120 --retry-all-errors --retry-delay 2 --cacert /ca.crt \
                --output /dev/null https://{address}:3443/api/health"
        ));
    }

    /// Sends `method` for `path` to the stand-in managed Grafana as its administrator, with the
    /// JSON `body` unless it is empty, once Grafana answers, and returns the answer's body.
    /// Panics if Grafana refuses.
    pub fn managed_grafana(&self, method: &str, path: &str, body: &str) -> String {
        let curl = compose(&[
            "--file",
            "../../deploy/tools/compose.yaml",
            "config",
            "--images",
            "curl",
        ]);
        let address = self
            .container
            .inspect("{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}");
        self.write("/root/grafana/request.json", body);
        let data = if body.is_empty() {
            ""
        } else {
            "--data-binary @/request.json"
        };
        self.shell_ok(&format!(
            "docker run --rm --quiet --volume /root/grafana/ca.crt:/ca.crt:ro \
                --volume /root/grafana/request.json:/request.json:ro {curl} --silent --show-error \
                --fail-with-body --retry 30 --retry-all-errors --retry-delay 2 --cacert /ca.crt \
                --user admin:admin --header 'Content-Type: application/json' --request {method} {data} \
                https://{address}:3443{path}"
        ))
    }

    /// Makes an authority of the test's own named `authority`, as ca.crt in `directory` on the
    /// server, and a certificate it signed for the server's own address, as server.crt and
    /// server.key, each readable by every user, since the stand-ins run as users of their own.
    /// The deployment's containers reach the server's address, while no name on Compose's
    /// network resolves inside the server. Returns the address.
    fn issue_certificate(&self, directory: &str, authority: &str) -> String {
        let address = self
            .container
            .inspect("{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}");
        self.shell_ok(&format!(
            "set -e
            mkdir -p {directory}
            cd {directory}
            openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.crt -days 1 -subj /CN={authority} 2> /dev/null
            openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr -subj /CN={address} 2> /dev/null
            echo subjectAltName=IP:{address} > san
            openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out server.crt -days 1 -extfile san 2> /dev/null
            chmod 644 ./*.crt ./*.key"
        ));
        address
    }

    /// Starts the stand-in for an alert integration on the server: a collector that receives
    /// Alertmanager's webhooks on the server's own address, port 9099, and writes each to
    /// /root/receiver/out/received.json. Returns the address to send them to.
    pub fn start_alert_receiver(&self) -> String {
        let image = compose(&[
            "--file",
            "../../deploy/observability/compose.yaml",
            "config",
            "--images",
            "collector",
        ]);
        let address = self
            .container
            .inspect("{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}");
        self.write(
            "/root/receiver/config.yaml",
            "receivers:\n  webhook_event:\n    endpoint: 0.0.0.0:9099\n    path: /alerts\n\
             exporters:\n  file:\n    path: /receiver/out/received.json\n\
             service:\n  pipelines:\n    logs:\n      receivers: [webhook_event]\n      exporters: [file]\n",
        );
        self.shell_ok(&format!(
            "set -e
            mkdir -p /root/receiver/out && chmod 777 /root/receiver/out
            docker run --detach --quiet --name receiver --publish 9099:9099 --volume /root/receiver:/receiver {image} \
                --config=/receiver/config.yaml > /dev/null"
        ));
        format!("http://{address}:9099/alerts")
    }

    /// Starts the stand-in for the bundled database's backup bucket on the server: SeaweedFS,
    /// serving S3 over HTTPS on the server's own address, with a certificate for that address
    /// from an authority of the test's own, which the deployment trusts as certs/backup-ca.crt.
    /// Writes the bucket's secrets, and returns its settings.
    pub fn start_bucket(&self) -> Vec<(String, String)> {
        let image = compose(&["config", "--images", "bucket"]);
        let address = self.issue_certificate("/root/bucket", "e2e-bucket-authority");
        self.write(
            "/root/bucket/s3.json",
            r#"{"identities":[{"name":"backup","credentials":[{"accessKey":"e2e","secretKey":"e2e-secret"}],"actions":["Admin","Read","Write","List","Tagging"]}]}"#,
        );
        self.shell_ok(&format!(
            "set -e
            cd /root/bucket
            chmod 644 ./*
            mkdir -p {DEPLOYMENT}/certs
            cp ca.crt {DEPLOYMENT}/certs/backup-ca.crt
            docker run --detach --quiet --name bucket --publish 8334:8334 --volume /root/bucket:/bucket:ro {image} \
                server -dir=/data -s3 -s3.port=8333 -s3.port.https=8334 \
                -s3.cert.file=/bucket/server.crt -s3.key.file=/bucket/server.key -s3.config=/bucket/s3.json > /dev/null
            # SeaweedFS's master takes a few seconds to start, and the shell can't create the bucket
            # before it answers.
            timeout 120 sh -c 'until echo s3.bucket.create -name backups | docker exec --interactive bucket weed shell -master=localhost:9333 2> /dev/null | grep -q created; do sleep 1; done'"
        ));
        self.write(&format!("{DEPLOYMENT}/secrets/backup_s3_key"), "e2e");
        self.write(
            &format!("{DEPLOYMENT}/secrets/backup_s3_key_secret"),
            "e2e-secret",
        );
        self.write(
            &format!("{DEPLOYMENT}/secrets/backup_cipher_pass"),
            "the passphrase of the e2e tests' backups",
        );
        [
            ("BACKUP_S3_ENDPOINT", address.as_str()),
            ("BACKUP_S3_PORT", "8334"),
            ("BACKUP_S3_REGION", "us-east-1"),
            ("BACKUP_S3_BUCKET", "backups"),
            ("BACKUP_S3_URI_STYLE", "path"),
        ]
        .iter()
        .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
        .collect()
    }
}
