//! The reference deployment with its self-hosted observability stack (D42), on a server of the
//! tests' own.

use crate::{
    deployment::{Deployment, GRAFANA_ADDRESS, get},
    docker::lossy,
};

#[tokio::test]
async fn sec_18_a_server_publishes_only_the_proxy_and_grafana_signs_every_viewer_in() {
    let deployment = Deployment::managed(&[("TELEMETRY", "self-hosted")]).await;
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    let root = deployment.server.caddy_root();

    let containers = deployment.server.shell_ok(
        "docker ps --filter label=com.docker.compose.project=deployment --format '{{.Names}} {{.Ports}}'",
    );
    let (home, home_headers) = get(
        root.clone(),
        deployment.url_at(GRAFANA_ADDRESS, "https", 443, "/"),
    );
    let (search, _) = get(
        root,
        deployment.url_at(GRAFANA_ADDRESS, "https", 443, "/api/search"),
    );

    let publishing: Vec<&str> = containers
        .lines()
        .filter(|line| line.contains("->"))
        .collect();
    assert_eq!(publishing.len(), 1, "{containers}");
    assert!(
        publishing[0].starts_with("deployment-caddy-1 "),
        "{containers}"
    );
    assert!(containers.contains("deployment-grafana-1"), "{containers}");
    assert_eq!(home, 302);
    assert!(
        home_headers["location"]
            .to_str()
            .unwrap()
            .ends_with("/login"),
        "{home_headers:?}"
    );
    assert_eq!(
        home_headers["strict-transport-security"],
        "max-age=31536000; includeSubDomains"
    );
    assert_eq!(search, 401);
}

#[tokio::test]
async fn obs_14_the_backups_report_when_an_archive_and_a_base_backup_last_succeeded() {
    let deployment = Deployment::bundled(&[("TELEMETRY", "self-hosted")]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql("SELECT pg_switch_wal()");

    // The collector reads the status files every minute, and exports to Prometheus within
    // seconds.
    let found = deployment.server.shell(
        "timeout 300 sh -c 'until docker exec deployment-prometheus-1 wget -qO- \
         \"http://localhost:9090/api/v1/query?query=file_mtime_seconds\" > /root/series.json \
         && grep -q backup-completed /root/series.json && grep -q \"\\\"archived\\\"\" /root/series.json; \
         do sleep 5; done'; cat /root/series.json",
    );

    let series = String::from_utf8_lossy(&found.stdout);
    assert!(found.status.success(), "{}", lossy(&found));
    assert!(
        series.contains(r#""file_name":"backup-completed""#),
        "{series}"
    );
    assert!(series.contains(r#""file_name":"archived""#), "{series}");
}
