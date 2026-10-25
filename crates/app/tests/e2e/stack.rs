//! The reference deployment with its self-hosted observability stack (D42), on a server of the
//! tests' own.

use crate::{
    deployment::{Deployment, GRAFANA_ADDRESS, get, post},
    docker::lossy,
    server::DEPLOYMENT,
};

#[tokio::test]
async fn sec_18_a_server_publishes_only_the_proxy_and_grafana_signs_every_viewer_in() {
    // The bundled database, and a stack that a later deploy adds, after one that ran without it.
    let deployment = Deployment::bundled(&[]);
    let without = deployment.apply(&deployment.api, &[]).await;
    assert!(without.status.success(), "{}", lossy(&without));
    deployment.set(&[("TELEMETRY", "self-hosted")]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    let root = deployment.server.caddy_root();
    let password = deployment
        .server
        .shell_ok(&format!("cat {DEPLOYMENT}/secrets/grafana_admin_password"));

    let containers = deployment.server.shell_ok(
        "docker ps --filter label=com.docker.compose.project=deployment --format '{{.Names}} {{.Ports}}'",
    );
    let (home, home_headers) = get(
        root.clone(),
        deployment.url_at(GRAFANA_ADDRESS, "https", 443, "/"),
    );
    let (search, _) = get(
        root.clone(),
        deployment.url_at(GRAFANA_ADDRESS, "https", 443, "/api/search"),
    );
    let (sign_up, refusal) = post(
        root.clone(),
        deployment.url_at(GRAFANA_ADDRESS, "https", 443, "/api/user/signup/step2"),
        r#"{"email":"viewer@example.invalid","username":"viewer","password":"a password of the e2e tests"}"#
            .to_owned(),
    );
    let (sign_in, signed_in) = post(
        root,
        deployment.url_at(GRAFANA_ADDRESS, "https", 443, "/login"),
        serde_json::json!({"user": "admin", "password": password}).to_string(),
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
    assert_eq!(sign_up, 401, "{refusal}");
    assert!(refusal.contains("signup is disabled"), "{refusal}");
    assert!(
        lossy(&deployed).contains("wrote Grafana's administrator password"),
        "{}",
        lossy(&deployed)
    );
    assert!(password.len() >= 32, "{password}");
    assert_eq!(sign_in, 200, "{signed_in}");
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

#[tokio::test]
async fn ops_16_a_deploy_installs_the_release_s_alert_rules_and_prometheus_reads_them() {
    let deployment = Deployment::managed(&[("TELEMETRY", "self-hosted")]).await;
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    // The services that the first deploy starts read its files as they start.
    assert!(
        !lossy(&first).contains("configuration changed"),
        "{}",
        lossy(&first)
    );
    // A release whose rules hold one rule more, and that restarts Caddy, so the deploy that
    // first installs its files refuses before the services read them.
    deployment.server.shell_ok(
        "printf '  - name: e2e\n    rules:\n      - alert: ReleaseMarker\n        expr: vector(0) > 1\n' \
         >> /root/release/observability/prometheus/rules.yaml",
    );
    deployment.set(&[
        ("TELEMETRY", "self-hosted"),
        ("TRUSTED_PROXIES", "203.0.113.0/24"),
    ]);

    let refused = deployment.apply(&deployment.api, &[]).await;
    let second = deployment
        .apply(&deployment.api, &["--allow-restart"])
        .await;
    let rules = deployment.server.shell(
        "timeout 60 sh -c 'until docker exec deployment-prometheus-1 wget -qO- http://localhost:9090/api/v1/rules \
         | grep -q ReleaseMarker; do sleep 1; done'",
    );

    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(second.status.success(), "{}", lossy(&second));
    assert!(
        lossy(&second).contains("prometheus's configuration changed"),
        "{}",
        lossy(&second)
    );
    assert!(
        rules.status.success(),
        "{}\nPrometheus's rules file ends with:\n{}\nIts log ends with:\n{}",
        lossy(&rules),
        deployment
            .server
            .shell_ok("docker exec deployment-prometheus-1 tail -n 6 /etc/prometheus/rules.yaml"),
        deployment
            .server
            .shell_ok("docker logs --tail 8 deployment-prometheus-1 2>&1")
    );
}

#[tokio::test]
async fn obs_08_a_managed_backend_receives_the_trace_and_the_logs_of_a_4xx_response() {
    let deployment = Deployment::managed(&[]).await;
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

    let (status, headers) = get(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/no-such-resource"),
    );
    let timing = headers["server-timing"].to_str().unwrap().to_owned();
    let trace = timing.split('-').nth(1).expect("the trace ID").to_owned();
    // The collector keeps the trace for its status, and decides after 15 seconds.
    let received = deployment.server.shell(&format!(
        "timeout 90 sh -c 'until grep resourceSpans /root/backend/out/telemetry.json 2> /dev/null | grep -q {trace} \
         && grep resourceLogs /root/backend/out/telemetry.json | grep -q {trace}; do sleep 2; done'"
    ));

    assert_eq!(status, 404);
    assert!(
        received.status.success(),
        "{}\nThe backend's log:\n{}",
        lossy(&received),
        deployment
            .server
            .shell_ok("docker logs --tail 10 backend 2>&1")
    );
}

#[tokio::test]
async fn obs_14_an_alert_reaches_the_integration_that_the_release_names_through_the_deployment_s_credential()
 {
    let deployment = Deployment::managed(&[("TELEMETRY", "self-hosted")]).await;
    let receiver = deployment.server.start_alert_receiver();
    deployment.server.write(
        &format!("{DEPLOYMENT}/secrets/alertmanager/page_url"),
        &receiver,
    );
    // A release whose `page` receiver posts to a webhook, through the deployment's credential,
    // and whose rules hold an alert that fires at once.
    deployment.server.write(
        "/root/page.awk",
        "{ print }\n/^  - name: page$/ { print \"    webhook_configs:\"; \
         print \"      - url_file: /etc/alertmanager-secrets/page_url\" }\n",
    );
    deployment.server.shell_ok(
        "cd /root/release/observability \
         && awk -f /root/page.awk alertmanager/alertmanager.yaml > /root/alertmanager.yaml \
         && cp /root/alertmanager.yaml alertmanager/alertmanager.yaml \
         && printf '  - name: e2e\n    rules:\n      - alert: E2EPage\n        expr: vector(1)\n        labels:\n          severity: page\n' \
         >> prometheus/rules.yaml",
    );

    let deployed = deployment.apply(&deployment.api, &[]).await;
    // Prometheus evaluates the rules every 15 seconds, and Alertmanager waits 30 seconds for a
    // group's first alerts.
    let received = deployment.server.shell(
        "timeout 240 sh -c 'until grep -q E2EPage /root/receiver/out/received.json 2> /dev/null; do sleep 5; done'",
    );

    assert!(deployed.status.success(), "{}", lossy(&deployed));
    assert!(
        received.status.success(),
        "{}\nAlertmanager's log ends with:\n{}",
        lossy(&received),
        deployment
            .server
            .shell_ok("docker logs --tail 10 deployment-alertmanager-1 2>&1")
    );
}
