//! The server the end-to-end tests deploy the reference deployment to: Docker in a container of
//! its own, joined to Compose's network, where it pulls the images from the registry that Compose
//! runs (TEST-04). It runs the deployment as a server's Docker would, with the deployment's
//! scripts in its shell, and publishes Caddy's ports on 127.0.0.1.

use std::{
    io::Write,
    net::SocketAddr,
    path::Path,
    process::{Command, Output, Stdio},
};

use crate::docker::{Container, compose, docker, lossy, postgres_network};

/// Where the server holds the release's deployment files, as `just deploy` copies them.
const RELEASE: &str = "/root/release";
/// The deployment's directory on the server.
pub const DEPLOYMENT: &str = "/root/deployment";

pub struct Server {
    container: Container,
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
            &image,
            "--insecure-registry=registry:5000",
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
        let address = self
            .container
            .inspect("{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}");
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
            openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.crt -days 1 -subj /CN=e2e-backend-authority 2> /dev/null
            openssl req -newkey rsa:2048 -nodes -keyout server.key -out server.csr -subj /CN={address} 2> /dev/null
            echo subjectAltName=IP:{address} > san
            openssl x509 -req -in server.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out server.crt -days 1 -extfile san 2> /dev/null
            chmod 644 ./*.crt ./*.key
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

    /// Starts the stand-in for the bundled database's backup bucket on the server: SeaweedFS,
    /// serving S3 over HTTPS on the server's own address, with a certificate for that address
    /// from an authority of the test's own, which the deployment trusts as certs/backup-ca.crt.
    /// The deployment's containers reach the server's address, while no name on Compose's
    /// network resolves inside the server. Writes the bucket's secrets, and returns its
    /// settings.
    pub fn start_bucket(&self) -> Vec<(String, String)> {
        let image = compose(&["config", "--images", "bucket"]);
        let address = self
            .container
            .inspect("{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}");
        self.write(
            "/root/bucket/s3.json",
            r#"{"identities":[{"name":"backup","credentials":[{"accessKey":"e2e","secretKey":"e2e-secret"}],"actions":["Admin","Read","Write","List","Tagging"]}]}"#,
        );
        self.shell_ok(&format!(
            "set -e
            cd /root/bucket
            openssl req -x509 -newkey rsa:2048 -nodes -keyout ca.key -out ca.crt -days 1 -subj /CN=e2e-bucket-authority 2> /dev/null
            openssl req -newkey rsa:2048 -nodes -keyout s3.key -out s3.csr -subj /CN={address} 2> /dev/null
            echo subjectAltName=IP:{address} > san
            openssl x509 -req -in s3.csr -CA ca.crt -CAkey ca.key -CAcreateserial -out s3.crt -days 1 -extfile san 2> /dev/null
            chmod 644 ./*
            mkdir -p {DEPLOYMENT}/certs
            cp ca.crt {DEPLOYMENT}/certs/backup-ca.crt
            docker run --detach --quiet --name bucket --publish 8334:8334 --volume /root/bucket:/bucket:ro {image} \
                server -dir=/data -s3 -s3.port=8333 -s3.port.https=8334 \
                -s3.cert.file=/bucket/s3.crt -s3.key.file=/bucket/s3.key -s3.config=/bucket/s3.json > /dev/null
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
