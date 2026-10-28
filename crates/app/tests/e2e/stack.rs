//! The reference deployment with its self-hosted observability stack (D42), on a server of the
//! tests' own.

use std::time::Duration;

use crate::{
    deployment::{Deployment, GRAFANA_ADDRESS, get, get_within, post},
    docker::lossy,
    server::DEPLOYMENT,
    support,
};

/// The attributes that would hold a client's personal data, which no span or log line of the
/// proxy keeps (OBS-04).
const PERSONAL: [&str; 4] = [
    "client.address",
    "network.peer.address",
    "user_agent.original",
    "url.path",
];

/// Stops the deployment's replicas, then asks the proxy for a resource, which no replica
/// answers, and returns the trace ID of its 503 problem.
fn trace_of_a_request_no_replica_answers(deployment: &Deployment) -> String {
    deployment
        .server
        .shell_ok("docker stop deployment-api-blue-1 deployment-api-blue-2 > /dev/null");
    let (status, _, body) = get_within(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
        Duration::from_secs(2),
    );
    assert_eq!(status, 503, "{body}");
    let problem: serde_json::Value = serde_json::from_str(&body).expect("a problem");
    problem["trace_id"]
        .as_str()
        .expect("its trace ID")
        .to_owned()
}

#[tokio::test]
async fn sec_18_a_server_publishes_only_the_proxy_and_grafana_signs_every_viewer_in() {
    // The bundled database, and a stack that a later deploy adds, after one that ran without it.
    let deployment = Deployment::bundled(&[]);
    let without = deployment.apply(&deployment.api, &[]).await;
    assert!(without.status.success(), "{}", lossy(&without));
    // TELEMETRY from off, which Caddy reads, so it restarts.
    deployment.set(&[("TELEMETRY", "self-hosted")]);
    let deployed = deployment
        .apply(&deployment.api, &["--allow-restart"])
        .await;
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
    assert_eq!(sign_in, 200, "{signed_in}");
}

#[tokio::test]
async fn obs_14_the_backups_the_server_s_memory_and_the_collector_report_what_the_cause_alerts_read()
 {
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
    // The server's memory, which the collector reads every 30 seconds. A test server's disks are
    // bind mounts of the machine's, which the scraper leaves out as views of filesystems it
    // reports where they are mounted whole, as on a server; so the test reads memory only.
    let memory = deployment.server.shell(
        "timeout 120 sh -c 'until docker exec deployment-prometheus-1 wget -qO-          \"http://localhost:9090/api/v1/query?query=system_linux_memory_available_bytes\"          | grep -q system_linux_memory_available_bytes; do sleep 5; done'",
    );
    assert!(memory.status.success(), "{}", lossy(&memory));
    // The collector's own counters, under the names the alert on lost telemetry reads, once the
    // API's metrics have passed through it.
    let collector = deployment.server.shell(
        "timeout 120 sh -c 'until docker exec deployment-prometheus-1 wget -qO-          \"http://localhost:9090/api/v1/query?query=otelcol_receiver_refused_metric_points_total\"          | grep -q otelcol_receiver_refused_metric_points_total; do sleep 5; done'",
    );
    assert!(collector.status.success(), "{}", lossy(&collector));
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
async fn obs_14_a_deploy_warns_until_the_release_names_an_integration_that_alerts_then_reach() {
    let deployment = Deployment::managed(&[("TELEMETRY", "self-hosted")]).await;
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
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

    assert!(
        lossy(&first).contains("the alerts notify no one"),
        "{}",
        lossy(&first)
    );
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    assert!(
        !lossy(&deployed).contains("the alerts notify no one"),
        "{}",
        lossy(&deployed)
    );
    assert!(
        received.status.success(),
        "{}\nAlertmanager's log ends with:\n{}",
        lossy(&received),
        deployment
            .server
            .shell_ok("docker logs --tail 10 deployment-alertmanager-1 2>&1")
    );
}

#[tokio::test]
async fn obs_13_a_deploy_imports_the_dashboards_into_a_folder_of_the_managed_grafana_that_editors_only_view()
 {
    let deployment = Deployment::managed(&[]).await;
    let mut settings = vec![("TELEMETRY".to_owned(), "managed".to_owned())];
    settings.extend(deployment.server.start_otlp_backend());
    settings.extend(deployment.server.start_managed_grafana());
    deployment.set(&pairs(&settings));
    let released = release_dashboards(&deployment);
    let token = format!("{DEPLOYMENT}/secrets/managed_grafana_token");
    // A token that the Grafana refuses, then the right one, then a release without a dashboard.
    deployment
        .server
        .shell_ok(&format!("cp {token} /root/token && echo refused > {token}"));
    let refused = deployment.apply(&deployment.api, &[]).await;
    deployment
        .server
        .shell_ok(&format!("cp /root/token {token}"));
    let first = deployment.apply(&deployment.api, &[]).await;
    deployment
        .server
        .shell_ok("rm /root/release/observability/grafana/dashboards/server.json");
    let second = deployment.apply(&deployment.api, &[]).await;

    let (imported, sources) = imported_dashboards(&deployment);
    let roles = folder_roles(&deployment);
    let kept: Vec<(String, String)> = released
        .iter()
        .filter(|uid| *uid != "server")
        .map(|uid| (uid.clone(), "api".to_owned()))
        .collect();

    assert!(refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("the import into the managed Grafana failed"),
        "{}",
        lossy(&refused)
    );
    assert!(first.status.success(), "{}", lossy(&first));
    assert!(
        lossy(&first).contains(&format!("imported {} dashboards", released.len())),
        "{}",
        lossy(&first)
    );
    assert!(second.status.success(), "{}", lossy(&second));
    assert!(
        lossy(&second).contains("deleting the dashboard server,"),
        "{}",
        lossy(&second)
    );
    assert_eq!(imported, kept);
    assert!(
        sources.iter().any(|uid| uid == "grafanacloud-prom")
            && sources.iter().any(|uid| uid == "grafanacloud-traces"),
        "{sources:?}"
    );
    assert!(
        !sources
            .iter()
            .any(|uid| ["prometheus", "loki", "tempo"].contains(&uid.as_str())),
        "{sources:?}"
    );
    assert!(
        roles.contains(&("Editor".to_owned(), "View".to_owned()))
            && roles.contains(&("Viewer".to_owned(), "View".to_owned())),
        "{roles:?}"
    );
}

#[tokio::test]
async fn obs_14_a_deploy_imports_the_rules_into_the_managed_grafana_and_warns_until_a_policy_reaches_a_contact_point()
 {
    let deployment = Deployment::managed(&[]).await;
    let mut settings = vec![("TELEMETRY".to_owned(), "managed".to_owned())];
    settings.extend(deployment.server.start_otlp_backend());
    deployment.set(&pairs(&settings));
    let released = release_rule_groups(&deployment);
    let without = deployment.apply(&deployment.api, &[]).await;
    settings.extend(deployment.server.start_managed_grafana());
    deployment.set(&pairs(&settings));
    let first = deployment.apply(&deployment.api, &[]).await;
    let loaded = rule_groups(&deployment);
    // A contact point and a policy that reaches it, then a release without a rule group.
    deployment.server.managed_grafana(
        "POST",
        "/api/v1/provisioning/contact-points",
        r#"{"name": "page", "type": "webhook", "settings": {"url": "http://127.0.0.1:9/alerts"}}"#,
    );
    deployment.server.managed_grafana(
        "PUT",
        "/api/v1/provisioning/policies",
        r#"{"receiver": "empty", "routes": [{"receiver": "page", "object_matchers": [["severity", "=", "page"]]}]}"#,
    );
    deployment.server.shell_ok(
        "cd /root/release/observability/prometheus \
         && awk '/^  - name: slo-latency$/ { skip = 1; next } /^  - name: / { skip = 0 } !skip' rules.yaml > /root/rules.yaml \
         && cp /root/rules.yaml rules.yaml",
    );
    let second = deployment.apply(&deployment.api, &[]).await;
    let reloaded = rule_groups(&deployment);
    let kept: Vec<String> = released
        .iter()
        .filter(|group| *group != "slo-latency")
        .cloned()
        .collect();

    assert!(without.status.success(), "{}", lossy(&without));
    assert!(
        lossy(&without).contains("MANAGED_GRAFANA_URL is empty, so no dashboard or alert rule"),
        "{}",
        lossy(&without)
    );
    assert!(first.status.success(), "{}", lossy(&first));
    assert!(
        lossy(&first).contains("send the alerts to no contact point"),
        "{}",
        lossy(&first)
    );
    assert_eq!(loaded, released);
    assert!(second.status.success(), "{}", lossy(&second));
    assert!(
        !lossy(&second).contains("send the alerts to no contact point"),
        "{}",
        lossy(&second)
    );
    assert!(
        lossy(&second).contains("deleting the rule group slo-latency,"),
        "{}",
        lossy(&second)
    );
    assert_eq!(reloaded, kept);
}

/// `settings` as `Deployment::set` takes them.
fn pairs(settings: &[(String, String)]) -> Vec<(&str, &str)> {
    settings
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// The stand-in managed Grafana's answer to `GET` for `path`, as JSON.
fn managed_grafana_json(deployment: &Deployment, path: &str) -> serde_json::Value {
    let answer = deployment.server.managed_grafana("GET", path, "");
    let parsed = serde_json::from_str(&answer);
    assert!(parsed.is_ok(), "{path}: {answer}");
    parsed.unwrap_or_default()
}

/// Each dashboard in the stand-in managed Grafana, by its uid and its folder's, sorted, and the
/// uid of every data source they name.
fn imported_dashboards(deployment: &Deployment) -> (Vec<(String, String)>, Vec<String>) {
    let listed = managed_grafana_json(
        deployment,
        "/apis/dashboard.grafana.app/v1/namespaces/default/dashboards",
    );
    let mut imported: Vec<(String, String)> = listed["items"]
        .as_array()
        .expect("the dashboards")
        .iter()
        .map(|item| {
            let metadata = &item["metadata"];
            let folder = &metadata["annotations"]["grafana.app/folder"];
            (
                metadata["name"].as_str().unwrap_or_default().to_owned(),
                folder.as_str().unwrap_or_default().to_owned(),
            )
        })
        .collect();
    imported.sort();
    let mut sources = Vec::new();
    data_sources(&listed, &mut sources);
    (imported, sources)
}

/// Adds to `uids` the uid of every data source that `value` names, at any depth.
fn data_sources(value: &serde_json::Value, uids: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(object) => {
            if let Some(uid) = object
                .get("datasource")
                .and_then(|source| source["uid"].as_str())
            {
                uids.push(uid.to_owned());
            }
            object.values().for_each(|child| data_sources(child, uids));
        }
        serde_json::Value::Array(items) => items.iter().for_each(|item| data_sources(item, uids)),
        _ => {}
    }
}

/// Each role's permission on the stand-in managed Grafana's folder `api`.
fn folder_roles(deployment: &Deployment) -> Vec<(String, String)> {
    managed_grafana_json(deployment, "/api/access-control/folders/api")
        .as_array()
        .expect("the folder's permissions")
        .iter()
        .filter_map(|permission| {
            Some((
                permission["builtInRole"].as_str()?.to_owned(),
                permission["permission"].as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// The names of the rule groups that the stand-in managed Grafana holds, sorted. Panics on a
/// rule that no import provisioned.
fn rule_groups(deployment: &Deployment) -> Vec<String> {
    let rules = managed_grafana_json(deployment, "/api/v1/provisioning/alert-rules");
    let mut groups: Vec<String> = rules
        .as_array()
        .expect("the rules")
        .iter()
        .map(|rule| {
            assert_eq!(rule["provenance"], "converted_prometheus", "{rule}");
            rule["ruleGroup"].as_str().unwrap_or_default().to_owned()
        })
        .collect();
    groups.sort();
    groups.dedup();
    groups
}

/// The uids of the dashboards of the release on the deployment's server, sorted.
fn release_dashboards(deployment: &Deployment) -> Vec<String> {
    let files = deployment
        .server
        .shell_ok("cat /root/release/observability/grafana/dashboards/*.json");
    let mut uids: Vec<String> = serde_json::Deserializer::from_str(&files)
        .into_iter::<serde_json::Value>()
        .map(|board| {
            board.expect("a dashboard")["uid"]
                .as_str()
                .expect("its uid")
                .to_owned()
        })
        .collect();
    uids.sort();
    uids
}

/// The names of the rule groups of the release on the deployment's server, sorted.
fn release_rule_groups(deployment: &Deployment) -> Vec<String> {
    let rules = deployment
        .server
        .shell_ok("cat /root/release/observability/prometheus/rules.yaml");
    let mut groups: Vec<String> = rules
        .lines()
        .filter_map(|line| line.strip_prefix("  - name: "))
        .map(str::to_owned)
        .collect();
    groups.sort();
    groups
}

#[tokio::test]
async fn obs_08_the_trace_id_of_the_proxy_s_503_leads_to_its_span_and_its_log_line() {
    let deployment = Deployment::managed(&[("TELEMETRY", "self-hosted")]).await;
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    // A replica answers 404, a status the collector keeps the trace of.
    let (_, headers) = get(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/no-such-resource"),
    );
    let timing = headers["server-timing"].to_str().unwrap().to_owned();
    let answered = timing.split('-').nth(1).expect("the trace ID").to_owned();

    let trace = trace_of_a_request_no_replica_answers(&deployment);
    let spans = deployment.server.telemetry_once_it_holds(
        &format!("http://tempo:3200/api/traces/{trace}"),
        "no replica answered",
    );
    let lines = deployment.server.telemetry_once_it_holds(
        &format!(
            "http://loki:3100/loki/api/v1/query_range?query=%7Bservice_name%3D%22caddy%22%7D%20%7C%20trace_id%3D%22{trace}%22"
        ),
        "no replica answered",
    );
    let both = deployment.server.telemetry_once_it_holds(
        &format!("http://tempo:3200/api/traces/{answered}"),
        support::service_name(),
    );

    assert!(spans.contains("STATUS_CODE_ERROR"), "{spans}");
    assert!(lines.contains(&trace), "{lines}");
    for name in PERSONAL {
        assert!(!spans.contains(name), "{name}: {spans}");
        assert!(!lines.contains(name), "{name}: {lines}");
    }
    // The API's server span is a child of the proxy's (OBS-06).
    assert!(both.contains("\"caddy\""), "{both}");
}

#[tokio::test]
async fn obs_08_a_managed_backend_receives_the_proxy_s_span_and_log_line_of_its_503() {
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

    let trace = trace_of_a_request_no_replica_answers(&deployment);
    // The collector keeps the trace for its error, and decides after 15 seconds.
    let received = deployment.server.shell(&format!(
        "timeout 120 sh -c 'until grep resourceSpans /root/backend/out/telemetry.json 2> /dev/null | grep -q {trace} \
         && grep resourceLogs /root/backend/out/telemetry.json | grep {trace} | grep -q \"no replica answered\"; do sleep 2; done'"
    ));

    assert!(
        received.status.success(),
        "{}\nThe backend's log:\n{}",
        lossy(&received),
        deployment
            .server
            .shell_ok("docker logs --tail 10 backend 2>&1")
    );
}
