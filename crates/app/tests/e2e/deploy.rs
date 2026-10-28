//! Deploys of the reference deployment (OPS-16, OPS-20, OPS-21), and its status and logs (OPS-22),
//! on a server of the tests' own, with Compose's Postgres as a managed database, or the bundled
//! one where a test says so.

use std::time::Duration;

use platform::errors::{ErrorCode, RequestError};

use crate::{
    deployment::{API_ADDRESS, Deployment, Traffic, assert_all_succeeded, get, get_within},
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
        &format!(
            r#"HEALTHCHECK --interval=1s --retries=2 CMD ["{}", "openapi", "/proc/unwritable"]"#,
            deployment.api_binary()
        ),
    );
    deployment
        .server
        .shell_ok("echo '# The unready release.' >> /root/release/compose.yaml");
    // A live deployment that lacks a file and a directory of the release, as an earlier
    // release's would.
    deployment.server.shell_ok(&format!(
        "rm {DEPLOYMENT}/import-grafana.sh && rm -r {DEPLOYMENT}/tools"
    ));

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
    let lacking = deployment.server.shell(&format!(
        "test ! -e {DEPLOYMENT}/import-grafana.sh && test ! -e {DEPLOYMENT}/tools"
    ));
    assert!(lacking.status.success(), "{}", lossy(&refused));
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
async fn ops_05_while_no_replica_runs_the_proxy_answers_at_once_with_the_api_s_503_problem() {
    let deployment = Deployment::managed(&[]).await;
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    let root = deployment.server.caddy_root();
    deployment
        .server
        .shell_ok("docker stop deployment-api-blue-1 deployment-api-blue-2 > /dev/null");

    // Within two seconds, or the client gives up.
    let (status, headers, body) = get_within(
        root.clone(),
        deployment.url("https", 443, "/v1/capabilities"),
        Duration::from_secs(2),
    );
    let (probe, probe_headers, probe_body) = get_within(
        root,
        deployment.url("https", 443, "/health/ready"),
        Duration::from_secs(2),
    );

    assert_eq!(status, 503, "{body}");
    assert_eq!(headers["content-type"], "application/problem+json");
    assert!(headers.contains_key("retry-after"), "{headers:?}");
    assert_eq!(
        headers["strict-transport-security"],
        "max-age=31536000; includeSubDomains"
    );
    assert!(headers.get("server").is_none(), "{headers:?}");
    for answer in [&headers, &probe_headers] {
        assert_eq!(answer["x-content-type-options"], "nosniff");
        assert_eq!(answer["referrer-policy"], "no-referrer");
    }
    let problem: serde_json::Value = serde_json::from_str(&body).expect("a problem");
    let unavailable = RequestError::Unavailable;
    assert_eq!(problem["code"], unavailable.code());
    assert_eq!(problem["title"], unavailable.title());
    assert_eq!(problem["type"], format!("/problems/{}", unavailable.code()));
    assert_eq!(problem["status"], 503);
    let trace = problem["trace_id"].as_str().expect("a trace ID");
    assert!(
        trace.len() == 32
            && trace
                .chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)),
        "{trace}"
    );
    let timing = headers["server-timing"].to_str().expect("text");
    assert!(
        timing.starts_with(&format!("trace;desc=00-{trace}-")),
        "{timing}"
    );
    // Without telemetry, the trace ID leads to the proxy's line through `just logs`.
    let logs = deployment.server.deploy(&["logs", "caddy"]);
    assert!(logs.status.success(), "{}", lossy(&logs));
    assert!(
        lossy(&logs)
            .lines()
            .any(|line| line.starts_with("caddy requests | ")
                && line.contains(&format!("\"traceID\":\"{trace}\""))),
        "{}",
        lossy(&logs)
    );
    assert_eq!(probe, 503);
    assert!(probe_body.is_empty(), "{probe_body}");
    for absent in ["content-type", "server-timing"] {
        assert!(probe_headers.get(absent).is_none(), "{probe_headers:?}");
    }
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
    // A task that runs: a process that holds the locks a task holds.
    deployment.server.shell_ok(&format!(
        "mkdir -p {DEPLOYMENT}/state && cd {DEPLOYMENT}/state \
         && exec 9>> lock 8>> running && flock -n 9 && flock 8 \
         && {{ sleep 600 < /dev/null > /dev/null 2>&1 & }}"
    ));

    let refused = deployment.apply(&deployment.api, &[]).await;
    let status = deployment.server.deploy(&["status"]);

    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("another deploy, backup or restore is running"),
        "{}",
        lossy(&refused)
    );
    assert!(deployment.server.running("caddy").is_empty());
    assert!(
        lossy(&status).contains("a deploy, backup or restore is running"),
        "{}",
        lossy(&status)
    );
    deployment
        .server
        .shell_ok(&format!("! flock -n {DEPLOYMENT}/state/lock true"));
}

#[tokio::test]
async fn ops_16_the_task_after_a_deploy_killed_before_its_replicas_serve_puts_back_its_files() {
    let deployment = Deployment::managed(&[]).await;
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let waiting = waiting_image(&deployment);
    deployment
        .server
        .shell_ok("echo '# The killed release.' >> /root/release/tools/compose.yaml");

    deployment
        .apply_until(&waiting, "starting 2 green replicas")
        .await
        .stop(&deployment.server, "KILL");
    let status = deployment.server.deploy(&["status"]);
    let backup = deployment.server.deploy(&["backup"]);

    assert!(
        lossy(&status).contains("a deploy stopped before its replicas served"),
        "{}",
        lossy(&status)
    );
    // The killed deploy's lock went with its processes, so the backup took it, put the files
    // back, and only then refused, since the database is managed.
    assert!(
        lossy(&backup)
            .contains("didn't put back the deployment files it replaced; putting them back"),
        "{}",
        lossy(&backup)
    );
    assert!(
        lossy(&backup).contains("the database is managed"),
        "{}",
        lossy(&backup)
    );
    let installed = deployment
        .server
        .shell_ok(&format!("cat {DEPLOYMENT}/tools/compose.yaml"));
    assert!(!installed.contains("The killed release"), "{installed}");
    deployment
        .server
        .shell_ok(&format!("test ! -e {DEPLOYMENT}/state/previous"));
}

#[tokio::test]
async fn ops_16_a_put_back_that_fails_says_so_and_the_next_task_puts_the_files_back() {
    let deployment = Deployment::managed(&[]).await;
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let waiting = waiting_image(&deployment);
    deployment
        .server
        .shell_ok("echo '# The interrupted release.' >> /root/release/tools/compose.yaml");
    let running = deployment
        .apply_until(&waiting, "starting 2 green replicas")
        .await;
    // Caddy's directory can't be written, so the put-back fails there, before it reaches tools/.
    deployment
        .server
        .shell_ok(&format!("mount -t tmpfs -o ro tmpfs {DEPLOYMENT}/caddy"));

    let interrupted = running.stop(&deployment.server, "INT");
    let left = deployment
        .server
        .shell_ok(&format!("cat {DEPLOYMENT}/tools/compose.yaml"));
    deployment
        .server
        .shell_ok(&format!("umount {DEPLOYMENT}/caddy"));
    let backup = deployment.server.deploy(&["backup"]);

    assert!(
        interrupted.contains("putting back the deployment files that this deploy replaced"),
        "{interrupted}"
    );
    assert!(
        interrupted.contains("couldn't put back the deployment files"),
        "{interrupted}"
    );
    assert!(left.contains("The interrupted release"), "{left}");
    assert!(
        lossy(&backup).contains("putting them back"),
        "{}",
        lossy(&backup)
    );
    let installed = deployment
        .server
        .shell_ok(&format!("cat {DEPLOYMENT}/tools/compose.yaml"));
    assert!(
        !installed.contains("The interrupted release"),
        "{installed}"
    );
}

#[tokio::test]
async fn ops_16_a_deploy_that_ends_once_its_replicas_serve_keeps_its_files() {
    let deployment = Deployment::managed(&[]).await;
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    // A container of a service that the settings don't choose, so the deploy stops it once its
    // replicas serve. Its process ignores the signal to stop, so it takes a minute to stop.
    deployment.server.shell_ok(
        "docker run --detach --quiet --name deployment-collector-managed-1 \
         --label com.docker.compose.project=deployment \
         --label com.docker.compose.service=collector-managed \
         --label com.docker.compose.container-number=1 --label com.docker.compose.oneoff=False \
         --entrypoint sleep \"$(docker inspect --format '{{ .Config.Image }}' deployment-caddy-1)\" 600 \
         > /dev/null",
    );
    deployment
        .server
        .shell_ok("echo '# The serving release.' >> /root/release/tools/compose.yaml");

    let interrupted = deployment
        .apply_until(&deployment.api, "stopping collector-managed")
        .await
        .stop(&deployment.server, "INT");

    assert!(!interrupted.contains("putting back"), "{interrupted}");
    assert_eq!(deployment.server.running("api-green").len(), 2);
    assert!(deployment.server.running("api-blue").is_empty());
    let installed = deployment
        .server
        .shell_ok(&format!("cat {DEPLOYMENT}/tools/compose.yaml"));
    assert!(installed.contains("The serving release"), "{installed}");
    deployment
        .server
        .shell_ok(&format!("test ! -e {DEPLOYMENT}/state/previous"));
}

/// The API's image, with a health check that always fails but counts no failure for ten minutes,
/// so a deploy of it waits for its replicas meanwhile.
fn waiting_image(deployment: &Deployment) -> String {
    deployment.image_from(
        &deployment.api,
        "waiting",
        &format!(
            r#"HEALTHCHECK --interval=1s --start-period=10m CMD ["{}", "openapi", "/proc/unwritable"]"#,
            deployment.api_binary()
        ),
    )
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
    // TELEMETRY back to off, which Caddy reads, so it restarts.
    deployment.set(&[]);

    let second = deployment
        .apply(&deployment.api, &["--allow-restart"])
        .await;

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

#[tokio::test]
async fn ops_16_a_deploy_refuses_a_managed_database_while_its_urls_name_the_bundled_one() {
    let deployment = Deployment::bundled(&[]);
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    deployment.set(&[("DATABASE", "managed")]);

    let refused = deployment.apply(&deployment.api, &[]).await;

    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("names the bundled database"),
        "{}",
        lossy(&refused)
    );
    assert_eq!(deployment.server.running("postgres").len(), 1);
}

#[tokio::test]
async fn ops_16_a_deploy_that_leaves_the_bundled_database_stops_it_and_its_backups_status() {
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
    // Back to the managed database, whose URLs the move to the bundled one kept aside.
    deployment.server.shell_ok(&format!(
        "cd {DEPLOYMENT}/secrets && for name in database_url database_owner_url database_admin_url; \
         do mv $name.before-bundled $name; done"
    ));
    deployment.set(&[]);

    let third = deployment.apply(&deployment.api, &[]).await;

    assert!(third.status.success(), "{}", lossy(&third));
    assert!(deployment.server.running("postgres").is_empty());
    assert!(deployment.server.running("backup").is_empty());
    let status = deployment
        .server
        .shell_ok("ls -A /var/lib/docker/volumes/deployment_backup-status/_data");
    assert_eq!(status, "", "the backups' status files that stayed");
    let (served, _) = get(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
    );
    assert_eq!(served, 200);
}

#[tokio::test]
async fn ops_22_status_shows_what_serves_and_the_backups_and_logs_show_the_services_output() {
    // With the self-hosted stack, whose images, unlike the API's and Caddy's, carry no version
    // label.
    let deployment = Deployment::bundled(&[("TELEMETRY", "self-hosted")]);
    let applied = deployment.apply(&deployment.api, &[]).await;
    assert!(applied.status.success(), "{}", lossy(&applied));

    let status = deployment.server.deploy(&["status"]);
    let logs = deployment.server.deploy(&["logs", "api", "caddy"]);
    let refused = deployment.server.deploy(&["logs", "--since=1h"]);

    assert!(status.status.success(), "{}", lossy(&status));
    let status = lossy(&status);
    assert!(status.contains("serving: blue"), "{status}");
    let version = deployment.server.shell_ok(&format!(
        "docker image inspect --format '{{{{ index .Config.Labels \"org.opencontainers.image.version\" }}}}' {}",
        deployment.api
    ));
    assert!(!version.is_empty(), "the image's version");
    for replica in ["api-blue-1", "api-blue-2"] {
        assert!(
            status.lines().any(|line| line.contains(replica)
                && line.contains("(healthy)")
                && line.trim_end().ends_with(&version)),
            "{replica} healthy, of {version}: {status}"
        );
    }
    let containers: Vec<&str> = status
        .lines()
        .skip_while(|line| *line != "containers:")
        .skip(1)
        .take_while(|line| line.starts_with("  "))
        .collect();
    assert!(
        containers.iter().any(|line| line.contains("prometheus-1")),
        "{status}"
    );
    for line in &containers {
        let shown = line.split_whitespace().last().unwrap_or_default();
        assert!(
            shown.starts_with(|c: char| c == 'v' || c.is_ascii_digit()),
            "a version for {line}: {status}"
        );
    }
    let recorded = |what: &str| {
        status
            .lines()
            .find_map(|line| line.trim().strip_prefix(what))
            .unwrap_or_default()
            .trim()
            .to_owned()
    };
    let archived = recorded("log last archived");
    let failed = recorded("archive last failed");
    assert!(
        recorded("base backup last completed").ends_with(" UTC"),
        "{status}"
    );
    assert!(archived.ends_with(" UTC"), "{status}");
    // Postgres archives before the first deploy creates the stanza, so an archive may have
    // failed then; the times sort as they read.
    assert!(failed == "never" || archived > failed, "{status}");
    assert!(status.contains("memory"), "{status}");
    assert!(
        status
            .lines()
            .any(|line| line.trim_start().starts_with("disk") && line.contains("free")),
        "{status}"
    );
    assert!(logs.status.success(), "{}", lossy(&logs));
    let logs = lossy(&logs);
    for service in ["api-blue-1", "api-blue-2", "caddy-1"] {
        assert!(
            logs.lines().any(|line| line.starts_with(service)),
            "{service}'s lines: {logs}"
        );
    }
    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("unknown option --since=1h"),
        "{}",
        lossy(&refused)
    );
}
