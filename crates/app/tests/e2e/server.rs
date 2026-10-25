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
}
