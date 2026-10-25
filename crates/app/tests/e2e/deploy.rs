//! Deploys of the reference deployment (OPS-16, OPS-20, OPS-21), on a server of the tests' own,
//! with Compose's Postgres as a managed database, or the bundled one where a test says so.

use crate::{
    deployment::{API_ADDRESS, Deployment, Traffic, assert_all_succeeded, get},
    docker::lossy,
    server::DEPLOYMENT,
};

#[tokio::test]
async fn ops_16_a_deploy_replaces_every_replica_while_every_request_succeeds() {
    let deployment = Deployment::managed(&[]).await;
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let blue = deployment.server.running("api-blue");

    let traffic = Traffic::start(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
    );
    let second = deployment.apply(&deployment.api, &[]).await;
    let outcomes = traffic.stop();

    assert!(second.status.success(), "{}", lossy(&second));
    assert_eq!(blue.len(), 2, "{}", lossy(&first));
    assert_eq!(deployment.server.running("api-green").len(), 2);
    assert!(deployment.server.running("api-blue").is_empty());
    assert_all_succeeded(&outcomes);
}

#[tokio::test]
async fn ops_16_a_release_whose_replicas_never_become_ready_is_removed_while_the_live_ones_serve() {
    let deployment = Deployment::managed(&[]).await;
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let blue = deployment.server.running("api-blue");
    // The API's image, with a health check that always fails, in a release whose deployment
    // files differ from the live one's.
    let unready = deployment.image_from(
        &deployment.api,
        "unready",
        r#"HEALTHCHECK --interval=1s --retries=2 CMD ["/usr/local/bin/antworker-api", "openapi", "/proc/unwritable"]"#,
    );
    deployment
        .server
        .shell_ok("echo '# The unready release.' >> /root/release/compose.yaml");

    let traffic = Traffic::start(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
    );
    let refused = deployment.apply(&unready, &[]).await;
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
    let installed = deployment
        .server
        .shell_ok(&format!("cat {DEPLOYMENT}/compose.yaml"));
    assert!(!installed.contains("The unready release"), "{installed}");
}

#[tokio::test]
async fn ops_16_a_deploy_restarts_the_proxy_only_when_the_operator_allows_it() {
    let deployment = Deployment::managed(&[]).await;
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let caddy = deployment.server.running("caddy");
    let blue = deployment.server.running("api-blue");

    deployment.set(&[("TRUSTED_PROXIES", "203.0.113.0/24")]);
    let refused = deployment.apply(&deployment.api, &[]).await;
    let refused_caddy = deployment.server.running("caddy");
    let allowed = deployment
        .apply(&deployment.api, &["--allow-restart"])
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
    let deployment = Deployment::managed(&[("DATABASE_MAX_CONNECTIONS", "40")]).await;

    let refused = deployment.apply(&deployment.api, &[]).await;

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
    let deployment = Deployment::managed(&[]).await;
    let deployed = deployment.apply(&deployment.api, &[]).await;
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

#[tokio::test]
async fn ops_16_a_deploy_refuses_to_run_while_another_deploy_backup_or_restore_runs() {
    let deployment = Deployment::managed(&[]).await;
    // The lock of a task that runs.
    deployment
        .server
        .shell_ok(&format!("mkdir -p {DEPLOYMENT}/state/lock"));

    let refused = deployment.apply(&deployment.api, &[]).await;

    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("another deploy, backup or restore is running"),
        "{}",
        lossy(&refused)
    );
    assert!(deployment.server.running("caddy").is_empty());
    deployment
        .server
        .shell_ok(&format!("test -d {DEPLOYMENT}/state/lock"));
}

#[tokio::test]
async fn ops_16_a_deploy_stops_the_services_that_the_settings_no_longer_choose() {
    let deployment = Deployment::managed(&[]).await;
    let backend = deployment.server.start_otlp_backend();
    let mut managed = vec![("TELEMETRY", "managed")];
    managed.extend(
        backend
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    deployment.set(&managed);
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let collector = deployment.server.running("collector-managed");
    // TELEMETRY back to off.
    deployment.set(&[]);

    let second = deployment.apply(&deployment.api, &[]).await;

    assert_eq!(collector.len(), 1);
    assert!(second.status.success(), "{}", lossy(&second));
    assert!(
        lossy(&second).contains("stopping collector-managed"),
        "{}",
        lossy(&second)
    );
    assert!(deployment.server.running("collector-managed").is_empty());
    assert_eq!(deployment.server.running("api-green").len(), 2);
}

#[tokio::test]
async fn ops_16_a_deploy_refuses_an_option_it_doesn_t_know() {
    let deployment = Deployment::managed(&[]).await;

    let refused = deployment
        .apply(&deployment.api, &["--allow-restarts"])
        .await;

    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("unknown option --allow-restarts"),
        "{}",
        lossy(&refused)
    );
    assert!(deployment.server.running("caddy").is_empty());
}

#[tokio::test]
async fn ops_16_a_deploy_restarts_the_bundled_database_only_for_other_files_and_with_consent() {
    let deployment = Deployment::bundled(&[]);
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let postgres = deployment.server.running("postgres");
    // The same files under another version, as each release's image is, and other files.
    let relabeled = deployment.image_from(
        &deployment.postgres,
        "relabeled",
        "LABEL org.opencontainers.image.version=e2e-other",
    );
    let rebuilt = deployment.image_from(
        &deployment.postgres,
        "rebuilt",
        "LABEL build.inputs=other-files",
    );

    let same = deployment
        .apply_images(&deployment.api, &relabeled, &[])
        .await;
    let kept = deployment.server.running("postgres");
    let refused = deployment
        .apply_images(&deployment.api, &rebuilt, &[])
        .await;
    let refused_postgres = deployment.server.running("postgres");
    let allowed = deployment
        .apply_images(&deployment.api, &rebuilt, &["--allow-restart"])
        .await;

    assert!(same.status.success(), "{}", lossy(&same));
    assert_eq!(kept, postgres);
    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("changes postgres"),
        "{}",
        lossy(&refused)
    );
    assert_eq!(refused_postgres, postgres);
    assert!(allowed.status.success(), "{}", lossy(&allowed));
    let restarted = deployment.server.running("postgres");
    assert_eq!(restarted.len(), 1);
    assert_ne!(restarted, postgres);
}

#[tokio::test]
async fn ops_16_a_deploy_gives_the_bundled_database_its_own_secrets_once_the_settings_choose_it() {
    let deployment = Deployment::managed(&[]).await;
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let mut bundled = vec![("DATABASE", "bundled")];
    let bucket = deployment.server.start_bucket();
    bundled.extend(
        bucket
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
    );
    deployment.set(&bundled);

    let second = deployment.apply(&deployment.api, &[]).await;

    assert!(second.status.success(), "{}", lossy(&second));
    let url = deployment
        .server
        .shell_ok(&format!("cat {DEPLOYMENT}/secrets/database_url"));
    assert!(url.ends_with("@postgres:5432/api"), "{url}");
    deployment.server.shell_ok(&format!(
        "test -s {DEPLOYMENT}/secrets/database_url.before-bundled"
    ));
    assert_eq!(deployment.server.running("api-green").len(), 2);
    let (status, _) = get(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
    );
    assert_eq!(status, 200);
}
