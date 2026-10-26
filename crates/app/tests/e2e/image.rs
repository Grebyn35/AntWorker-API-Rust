//! The images (OPS-14, OPS-15, OPS-20).

use std::process::{Command, Output};

use crate::{
    docker::{Container, lossy, postgres_network},
    images::Pushed,
    support::{TestDatabase, http_request},
};

/// The port `serve` binds in the container.
pub const PORT: u16 = 8080;

/// The API's image under test, which a container on this machine runs.
pub struct Image {
    reference: String,
    platform: String,
}

impl Image {
    pub fn pushed() -> Self {
        let image = Pushed::api();
        Self {
            reference: image.on_host(),
            platform: image.platform().to_owned(),
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
    pub fn run(&self, network: &str, env: &[String], command: &str) -> Output {
        Command::new("docker")
            .args(["run", "--rm"])
            .args(self.arguments(network, env))
            .args([&self.reference, command])
            .output()
            .expect("run the image")
    }

    /// Starts `serve` in a container of the image, with its port published on 127.0.0.1.
    pub fn serve(&self, network: &str, env: &[String]) -> Container {
        let published = format!("127.0.0.1::{PORT}");
        let mut arguments = self.arguments(network, env);
        arguments.extend(["--publish", &published, &self.reference, "serve"]);
        Container::start(&arguments)
    }
}

/// The settings of a container of the image, which reaches `database` as `postgres` on
/// Compose's network.
pub fn settings(database: &TestDatabase) -> Vec<String> {
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

#[test]
fn ops_20_the_postgres_image_holds_the_authorities_that_a_bucket_s_certificate_is_checked_against()
{
    let image = Pushed::postgres();
    let compose = include_str!("../../../../deploy/compose.yaml");
    let authorities = compose
        .split("${BACKUP_CA_FILE:-")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("the authorities BACKUP_CA_FILE names by default");

    let output = Command::new("docker")
        .args(["run", "--rm", "--platform", image.platform()])
        .args(["--entrypoint", "grep", &image.on_host()])
        .args(["--count", "BEGIN CERTIFICATE", authorities])
        .output()
        .expect("run docker");

    let certificates: usize = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or(0);
    assert!(certificates > 100, "{authorities}: {}", lossy(&output));
}
