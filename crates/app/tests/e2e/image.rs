//! The image (OPS-14, OPS-15).

use std::{
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;

use crate::{
    docker::{Container, compose, lossy, postgres_network},
    support::{TestDatabase, http_request},
};

/// The port `serve` binds in the container.
const PORT: u16 = 8080;

/// The image under test: the image for the platform `E2E_PLATFORM` names, of the index that
/// `just image` wrote to target/image, as the registry that Compose runs holds it. A container
/// runs it by its own digest, not the index's: Docker's classic image store holds one platform
/// under a digest, and refuses to pull a second one under the same.
struct Image {
    reference: String,
    platform: String,
}

impl Image {
    fn pushed() -> Self {
        let platform = std::env::var("E2E_PLATFORM").expect("E2E_PLATFORM; run `just e2e`");
        let name = std::env::var("E2E_IMAGE_NAME").expect("E2E_IMAGE_NAME; run `just e2e`");
        let index = index_digest();
        let digest = compose(&[
            "run",
            "--rm",
            "--no-TTY",
            "crane",
            "digest",
            "--insecure",
            "--platform",
            &platform,
            &format!("registry:5000/{name}@{index}"),
        ]);
        let registry = compose(&["port", "registry", "5000"]);
        Self {
            reference: format!("{registry}/{name}@{digest}"),
            platform,
        }
    }

    /// The arguments of `docker run` that every container of the image takes: its platform, the
    /// network it reaches Postgres on, a root filesystem it can't write to, and `env`.
    fn arguments<'a>(&'a self, network: &'a str, env: &'a [String]) -> Vec<&'a str> {
        let mut arguments = vec![
            "--platform",
            &self.platform,
            "--network",
            network,
            "--read-only",
        ];
        for setting in env {
            arguments.extend(["--env", setting]);
        }
        arguments
    }

    /// Runs the subcommand `command` in a container of the image until it exits.
    fn run(&self, network: &str, env: &[String], command: &str) -> Output {
        Command::new("docker")
            .args(["run", "--rm"])
            .args(self.arguments(network, env))
            .args([&self.reference, command])
            .output()
            .expect("run the image")
    }

    /// Starts `serve` in a container of the image, with its port published on 127.0.0.1.
    fn serve(&self, network: &str, env: &[String]) -> Container {
        let published = format!("127.0.0.1::{PORT}");
        let mut arguments = self.arguments(network, env);
        arguments.extend(["--publish", &published, &self.reference, "serve"]);
        Container::start(&arguments)
    }
}

/// The digest of the index in target/image.
#[expect(
    clippy::disallowed_methods,
    reason = "tests may block, each on a runtime of its own (CODE-05)"
)]
fn index_digest() -> String {
    let layout = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/image/index.json");
    let index: Value = serde_json::from_str(
        &std::fs::read_to_string(&layout).expect("target/image/index.json; run `just image`"),
    )
    .expect("an OCI index");
    index["manifests"][0]["digest"]
        .as_str()
        .expect("the image's digest")
        .to_owned()
}

/// The settings of a container of the image, which reaches `database` as `postgres` on
/// Compose's network.
fn settings(database: &TestDatabase) -> Vec<String> {
    let mut settings = vec![
        "ENVIRONMENT=production".to_owned(),
        format!("LISTEN_ADDR=0.0.0.0:{PORT}"),
    ];
    for setting in ["DATABASE_ADMIN_URL", "DATABASE_OWNER_URL", "DATABASE_URL"] {
        let mut url = url::Url::parse(&std::env::var(setting).expect(setting)).expect(setting);
        url.set_host(Some("postgres")).expect("a host");
        url.set_port(Some(5432)).expect("a port");
        url.set_path(database.name());
        settings.push(format!("{setting}={url}"));
    }
    settings
}

#[tokio::test]
async fn ops_15_bootstrap_migrate_and_serve_run_from_one_image_as_a_non_root_user_on_a_read_only_root()
 {
    let image = Image::pushed();
    let database = TestDatabase::empty().await;
    let network = postgres_network();
    let settings = settings(&database);

    let bootstrap = database
        .with_roles_locked(async { image.run(&network, &settings, "bootstrap") })
        .await;
    let migrate = image.run(&network, &settings, "migrate");
    let server = image.serve(&network, &settings);
    let health = server.health();
    let capabilities = http_request(server.address(PORT), "GET", "/v1/capabilities", &[])
        .await
        .expect("an answer");

    assert!(bootstrap.status.success(), "{}", lossy(&bootstrap));
    assert!(migrate.status.success(), "{}", lossy(&migrate));
    assert_eq!(health, "healthy", "{}", server.logs());
    assert!(capabilities.starts_with("HTTP/1.1 200 "), "{capabilities}");
    assert_eq!(server.inspect("{{.Config.User}}"), "65532:65532");
    assert_eq!(server.inspect("{{.HostConfig.ReadonlyRootfs}}"), "true");
}
