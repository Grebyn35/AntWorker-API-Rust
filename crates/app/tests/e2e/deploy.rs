//! Deploys of the reference deployment (OPS-14, OPS-17, OPS-18), and its status, its logs and the
//! operator's tasks on its accounts (OPS-19), on a server of the tests' own, with a managed
//! database on a Postgres server of its own, or the bundled one where a test says so.

use std::{process::Output, time::Duration};

use platform::errors::{ErrorCode, RequestError};

use crate::{
    deployment::{
        Deployment, MANAGED_TLS, Traffic, assert_all_succeeded, get, get_with_too_many_headers,
        get_within, post, unanswered,
    },
    docker::{curl_image, lossy},
    server::DEPLOYMENT,
};
// part: files
use crate::deployment::streaming_client;
// end part: files

#[tokio::test]
async fn ops_14_a_deploy_replaces_every_replica_while_every_request_succeeds() {
    let deployment = Deployment::managed(&[]);
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

// part: files
/// A body that sends `parts`, then waits for the rest, which arrives on `rest`, telling `started`
/// once its first part is read.
struct Coming {
    parts: std::vec::IntoIter<Vec<u8>>,
    current: std::io::Cursor<Vec<u8>>,
    rest: std::sync::mpsc::Receiver<Vec<u8>>,
    started: Option<std::sync::mpsc::Sender<()>>,
}

impl std::io::Read for Coming {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let read = self.current.read(buffer)?;
            if read > 0 {
                if let Some(started) = self.started.take() {
                    started
                        .send(())
                        .expect("the test waits for the upload's start");
                }
                return Ok(read);
            }
            let next = match self.parts.next() {
                Some(part) => part,
                // Waits for the rest, then ends.
                None => match self.rest.recv() {
                    Ok(part) => part,
                    Err(_) => return Ok(0),
                },
            };
            self.current = std::io::Cursor::new(next);
        }
    }
}

#[tokio::test]
async fn ops_06_an_upload_whose_body_still_comes_once_its_replica_stopped_accepting_is_stored() {
    let deployment = Deployment::managed(&[]);
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let binary = deployment.api_binary();
    let workspace = deployment.workspace();
    let (start, rest) = (b"stored ".to_vec(), b"across a deploy".to_vec());
    let length = u64::try_from(start.len() + rest.len()).unwrap();
    let (send_rest, coming) = std::sync::mpsc::channel();
    let (started, upload_started) = std::sync::mpsc::channel();
    let body = Coming {
        parts: vec![start].into_iter(),
        current: std::io::Cursor::new(Vec::new()),
        rest: coming,
        started: Some(started),
    };
    let upload = {
        let (root, files, token) = (
            workspace.root.clone(),
            workspace.files.clone(),
            workspace.token,
        );
        std::thread::spawn(move || {
            let response = streaming_client(&root, Duration::from_secs(120))
                .post(&files)
                .bearer_auth(token)
                .header("content-type", "application/octet-stream")
                .header("content-disposition", "attachment; filename=\"notes.txt\"")
                .body(reqwest::blocking::Body::sized(body, length))
                .send()
                .expect("an answer");
            (
                response.status().as_u16(),
                response.text().expect("the body"),
            )
        })
    };
    upload_started.recv().expect("the upload's start");

    let running = deployment
        .apply_until(&deployment.api, "stopping the blue replicas")
        .await;
    // Each blue replica has stopped accepting connections, so its drain has begun, or has stopped
    // altogether, as one with no request in flight does.
    deployment.server.shell_ok(&format!(
        "timeout 60 sh -c 'for replica in 1 2; do \
             until answer=$(docker exec deployment-api-blue-$replica {binary} health 2>&1); \
                 case $answer in *\"can'\\''t reach\"* | *\"is not running\"* | *\"No such container\"*) true ;; *) false ;; esac; do \
                 sleep 0.2; \
             done; \
         done'"
    ));
    send_rest
        .send(rest)
        .expect("the upload still reads its body");
    let (deployed, written) = running.finish();
    let (status, file) = upload.join().expect("the upload");

    assert!(deployed, "{written}");
    assert_eq!(status, 201, "{file}");
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&file).unwrap()["size_bytes"],
        length
    );
}
// end part: files

#[tokio::test]
async fn ops_14_a_release_whose_replicas_never_become_ready_is_removed_while_the_live_ones_serve() {
    let deployment = Deployment::managed(&[]);
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
async fn ops_14_a_deploy_restarts_the_proxy_only_when_the_operator_allows_it() {
    let deployment = Deployment::managed(&[]);
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

// part: files
#[tokio::test]
async fn ops_14_a_deploy_restarts_the_scanner_only_for_its_changed_files_and_with_consent() {
    let deployment = Deployment::managed(&[]);
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let scanner = deployment.server.running("scanner");

    let same = deployment.apply(&deployment.api, &[]).await;
    let kept = deployment.server.running("scanner");
    deployment
        .server
        .shell_ok("echo '# A changed configuration.' >> /root/release/scanner/clamd.conf");
    let refused = deployment.apply(&deployment.api, &[]).await;
    let refused_scanner = deployment.server.running("scanner");
    let allowed = deployment
        .apply(&deployment.api, &["--allow-restart"])
        .await;

    assert!(same.status.success(), "{}", lossy(&same));
    assert_eq!(kept, scanner);
    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("changes scanner"),
        "{}",
        lossy(&refused)
    );
    assert_eq!(refused_scanner, scanner);
    assert!(allowed.status.success(), "{}", lossy(&allowed));
    let restarted = deployment.server.running("scanner");
    assert_eq!(restarted.len(), 1);
    assert_ne!(restarted, scanner);
}
// end part: files

#[tokio::test]
async fn ops_18_a_deploy_refuses_replicas_whose_connections_would_not_fit() {
    let deployment = Deployment::managed(&[("DATABASE_MAX_CONNECTIONS", "40")]);

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
async fn ops_17_a_deploy_refuses_a_connection_that_would_leave_the_server_unencrypted() {
    let deployment = Deployment::managed(&[]);
    let verified = format!("?{MANAGED_TLS}");
    let mut refusals = Vec::new();
    // Each of a managed database's URLs, one at a time: without `sslmode`, with which the driver
    // falls back to plain text, with one that checks no certificate, with `verify-full` only in
    // its fragment, and with `verify-full` beside another, first, then last.
    for name in ["database_url", "database_owner_url", "database_admin_url"] {
        let secret = format!("{DEPLOYMENT}/secrets/{name}");
        let url = deployment.server.shell_ok(&format!("cat {secret}"));
        assert!(url.ends_with(&verified), "the tests' {name}");
        let plain = url.trim_end_matches(&verified);
        for query in [
            "",
            "?sslmode=require",
            "#?sslmode=verify-full",
            "?sslmode=verify-full&sslmode=disable",
            "?sslmode=disable&sslmode=verify-full",
        ] {
            deployment.server.write(&secret, &format!("{plain}{query}"));
            refusals.push((
                deployment.apply(&deployment.api, &[]).await,
                format!("secrets/{name} doesn't set sslmode=verify-full"),
            ));
        }
        deployment.server.write(&secret, &url);
    }
    // Email without encryption to a mail server that isn't on the server itself.
    deployment.set(&[
        ("EMAIL", "smtp"),
        ("SMTP_HOST", "192.0.2.25"),
        ("SMTP_SECURITY", "none"),
        ("EMAIL_FROM", "AntWorker <noreply@example.com>"),
    ]);
    refusals.push((
        deployment.apply(&deployment.api, &[]).await,
        "SMTP_SECURITY=none sends email unencrypted".to_owned(),
    ));
    // The same, in quotes, which Compose takes off a value, where a check of the line's text
    // would read another value than `none`.
    deployment.set(&[
        ("EMAIL", "smtp"),
        ("SMTP_HOST", "192.0.2.25"),
        ("SMTP_SECURITY", "\"none\""),
        ("EMAIL_FROM", "AntWorker <noreply@example.com>"),
    ]);
    refusals.push((
        deployment.apply(&deployment.api, &[]).await,
        "which sets SMTP_SECURITY, is none of the forms that a deploy reads".to_owned(),
    ));

    for (refused, why) in &refusals {
        assert!(!refused.status.success(), "{why}: {}", lossy(refused));
        assert!(lossy(refused).contains(why), "{why}: {}", lossy(refused));
        // A refusal names the file, and never what it holds.
        assert!(
            !lossy(refused).contains("postgres://"),
            "{}",
            lossy(refused)
        );
    }
    assert!(deployment.server.running("caddy").is_empty());
    assert!(deployment.server.running("api-blue").is_empty());
}

#[tokio::test]
async fn ops_05_while_no_replica_runs_the_proxy_answers_at_once_with_the_api_s_503_problem() {
    let deployment = Deployment::managed(&[]);
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
        assert_eq!(answer["cache-control"], "no-store");
        assert_eq!(
            answer["content-security-policy"],
            "default-src 'none'; base-uri 'none'; frame-ancestors 'none'"
        );
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
async fn ops_05_while_no_replica_runs_the_proxy_answers_a_request_beside_the_probes_with_the_problem()
 {
    let deployment = Deployment::managed(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    let root = deployment.server.caddy_root();
    deployment
        .server
        .shell_ok("docker stop deployment-api-blue-1 deployment-api-blue-2 > /dev/null");

    // No probe: a path that starts as a probe's, a probe's path in another case, and a probe's
    // path with another method.
    let (beside, _, beside_body) = get_within(
        root.clone(),
        deployment.url("https", 443, "/health/none"),
        Duration::from_secs(2),
    );
    let (cased, _, cased_body) = get_within(
        root.clone(),
        deployment.url("https", 443, "/HEALTH/live"),
        Duration::from_secs(2),
    );
    let (posted, posted_body) = post(
        root,
        deployment.url("https", 443, "/health/ready"),
        "{}".to_owned(),
    );

    for (answered, body) in [
        (beside, &beside_body),
        (cased, &cased_body),
        (posted, &posted_body),
    ] {
        assert_eq!(answered, 503, "{body}");
        let problem: serde_json::Value = serde_json::from_str(body).expect("a problem");
        assert_eq!(problem["code"], RequestError::Unavailable.code());
    }
}

#[tokio::test]
async fn ops_17_the_proxy_answers_no_plain_http_at_the_api_s_address_and_https_with_hsts() {
    let deployment = Deployment::managed(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    let root = deployment.server.caddy_root();

    let plain = unanswered(root.clone(), deployment.url("http", 80, "/v1/capabilities"));
    let (secure, secure_headers) = get(root, deployment.url("https", 443, "/v1/capabilities"));

    assert!(plain, "plain HTTP got an answer");
    assert_eq!(secure, 200);
    assert_eq!(
        secure_headers["strict-transport-security"],
        "max-age=31536000; includeSubDomains"
    );
    assert!(secure_headers.get("server").is_none());
}

#[tokio::test]
async fn sec_12_the_proxy_sets_the_headers_that_an_answer_of_a_replica_s_own_server_lacks() {
    let deployment = Deployment::managed(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    let root = deployment.server.caddy_root();

    // More headers than a replica's HTTP server takes: it refuses the request before the router,
    // with a 431 that carries none of the four headers, and Caddy passes both on.
    let (status, headers) =
        get_with_too_many_headers(root, deployment.url("https", 443, "/v1/capabilities"));

    assert_eq!(status, 431);
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["referrer-policy"], "no-referrer");
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(
        headers["content-security-policy"],
        "default-src 'none'; base-uri 'none'; frame-ancestors 'none'"
    );
    assert_eq!(
        headers["strict-transport-security"],
        "max-age=31536000; includeSubDomains"
    );
}

/// The requests that the `ip` bucket still takes of each of two clients, as the `RateLimit`
/// headers of two answers of `replica` give them: a container on `network` of the deployment's
/// server sends the replica itself two requests, each with an `X-Forwarded-For` that names a
/// client of its own.
fn left_for_two_forwarded_clients(
    deployment: &Deployment,
    network: &str,
    replica: &str,
) -> (u64, u64) {
    let curl = curl_image();
    let request = |client: &str| {
        format!(
            "--silent --max-time 10 --output /dev/null --dump-header - \
             --header 'X-Forwarded-For: {client}' http://{replica}:8080/v1/capabilities"
        )
    };
    let answers = deployment.server.shell_ok(&format!(
        "docker run --rm --quiet --network {network} {curl} {} --next {}",
        request("203.0.113.9"),
        request("203.0.113.10")
    ));
    let left: Vec<u64> = answers
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.eq_ignore_ascii_case("ratelimit"))
        .map(|(_, value)| {
            let (_, left) = value.split_once(";r=").expect("the requests left");
            left.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse()
                .expect("a number of requests")
        })
        .collect();
    assert_eq!(left.len(), 2, "{network}: {answers}");
    (left[0], left[1])
}

/// The subnet of the network that the proxy and the replicas of the deployment join.
fn proxy_subnet(deployment: &Deployment) -> String {
    deployment.server.shell_ok(
        "docker network inspect --format '{{ range .IPAM.Config }}{{ .Subnet }}{{ end }}' \
         deployment_proxy",
    )
}

#[tokio::test]
async fn sec_13_a_replica_takes_the_client_ip_from_a_peer_on_the_proxy_s_network_alone() {
    let deployment = Deployment::managed(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    let replica = "deployment-api-blue-1";
    let members = deployment.server.shell_ok(
        "docker network inspect --format '{{ range .Containers }}{{ .Name }} {{ end }}' \
         deployment_proxy",
    );
    let joined = deployment.server.shell_ok(&format!(
        "docker inspect --format \
         '{{{{ range $name, $network := .NetworkSettings.Networks }}}}{{{{ $name }}}} {{{{ end }}}}' \
         {replica}"
    ));
    let proxy_addresses = deployment.server.shell_ok(
        "docker inspect --format \
         '{{ range .NetworkSettings.Networks }}{{ .IPAddress }} {{ end }}' deployment-caddy-1",
    );

    let left: Vec<(&str, (u64, u64))> = joined
        .split_whitespace()
        .map(|network| {
            (
                network,
                left_for_two_forwarded_clients(&deployment, network, replica),
            )
        })
        .collect();
    deployment.registered("ada@example.com");
    let recorded = deployment.sql("SELECT DISTINCT host(client_ip) FROM audit_entry_clients");

    let mut members: Vec<&str> = members.split_whitespace().collect();
    members.sort_unstable();
    assert_eq!(
        members,
        [
            "deployment-api-blue-1",
            "deployment-api-blue-2",
            "deployment-caddy-1"
        ]
    );
    let trusted = left
        .iter()
        .filter(|(network, _)| *network == "deployment_proxy")
        .count();
    assert_eq!(trusted, 1, "{joined}");
    assert!(left.len() > 1, "{joined}");
    for (network, (first, second)) in left {
        if network == "deployment_proxy" {
            // From the proxy's network, each client that the header names has a bucket of its
            // own, which took its one request.
            assert_eq!(first, second, "{network}");
        } else {
            // From each other network that the replica joins, one bucket took both requests:
            // the sender's own.
            assert_eq!(second + 1, first, "{network}");
        }
    }
    // A request through the proxy is recorded under its client's address, none of the proxy's
    // own, which a replica that took the proxy for the client would record.
    assert!(!recorded.is_empty());
    for address in recorded.lines() {
        assert!(
            !proxy_addresses.split_whitespace().any(|own| own == address),
            "{address} of {proxy_addresses}"
        );
    }
}

#[tokio::test]
async fn sec_13_a_deploy_refuses_a_subnet_of_the_proxy_s_network_that_is_no_ipv4_range_within_its_bounds()
 {
    let deployment = Deployment::managed(&[]);

    for (subnet, refusal) in [
        (
            "fd00:221::/64",
            "PROXY_SUBNET is an IPv4 range in CIDR notation",
        ),
        (
            "172.016.221.0/24",
            "PROXY_SUBNET is an IPv4 range in CIDR notation",
        ),
        (
            "172.16.221.7/24",
            "PROXY_SUBNET names a range by its first address",
        ),
        (
            "172.16.221.0/27",
            "PROXY_SUBNET has a prefix of 16 to 26 bits",
        ),
        ("10.0.0.0/8", "PROXY_SUBNET has a prefix of 16 to 26 bits"),
    ] {
        deployment.set(&[("PROXY_SUBNET", subnet)]);
        let refused = deployment.apply(&deployment.api, &[]).await;
        assert!(!refused.status.success(), "{subnet}: {}", lossy(&refused));
        assert!(
            lossy(&refused).contains(refusal),
            "{subnet}: {}",
            lossy(&refused)
        );
    }
    assert!(deployment.server.running("caddy").is_empty());
}

#[tokio::test]
async fn ops_14_a_deploy_replaces_the_network_of_the_proxy_and_the_replicas_only_when_the_operator_allows_it()
 {
    let deployment = Deployment::managed(&[]);
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let caddy = deployment.server.running("caddy");
    let blue = deployment.server.running("api-blue");
    let subnet = proxy_subnet(&deployment);

    deployment.set(&[("PROXY_SUBNET", "172.16.222.0/24")]);
    let refused = deployment.apply(&deployment.api, &[]).await;
    let kept = (
        deployment.server.running("caddy"),
        deployment.server.running("api-blue"),
        proxy_subnet(&deployment),
    );
    let allowed = deployment
        .apply(&deployment.api, &["--allow-restart"])
        .await;

    assert_eq!(subnet, "172.16.221.0/24");
    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("this deploy replaces the network"),
        "{}",
        lossy(&refused)
    );
    assert_eq!(kept, (caddy, blue, subnet));
    assert!(allowed.status.success(), "{}", lossy(&allowed));
    assert_eq!(proxy_subnet(&deployment), "172.16.222.0/24");
    assert!(deployment.server.running("api-blue").is_empty());
    assert_eq!(deployment.server.running("api-green").len(), 2);
    let (status, _) = get(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
    );
    assert_eq!(status, 200);
    // The replicas take the client IP from a peer in the subnet that the network has since.
    let (first, second) =
        left_for_two_forwarded_clients(&deployment, "deployment_proxy", "deployment-api-green-1");
    assert_eq!(first, second);
}

#[tokio::test]
async fn ops_14_a_deploy_that_replaced_the_network_says_that_no_replica_serves_once_its_own_never_become_ready()
 {
    let deployment = Deployment::managed(&[]);
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    // The API's image, with a health check that always fails.
    let unready = deployment.image_from(
        &deployment.api,
        "unready",
        &format!(
            r#"HEALTHCHECK --interval=1s --retries=2 CMD ["{}", "openapi", "/proc/unwritable"]"#,
            deployment.api_binary()
        ),
    );
    deployment.set(&[("PROXY_SUBNET", "172.16.222.0/24")]);

    let refused = deployment.apply(&unready, &["--allow-restart"]).await;

    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains(
            "it stopped the replicas that served before it, if any, to replace the network, so none serves"
        ),
        "{}",
        lossy(&refused)
    );
    assert!(
        !lossy(&refused).contains("never stopped"),
        "{}",
        lossy(&refused)
    );
    assert!(deployment.server.running("api-blue").is_empty());
    assert!(deployment.server.running("api-green").is_empty());
}

#[tokio::test]
async fn ops_14_a_deploy_refuses_to_run_while_another_deploy_backup_or_restore_runs() {
    let deployment = Deployment::managed(&[]);
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

#[test]
fn ops_18_an_operator_s_subcommand_refuses_to_run_while_another_runs() {
    let deployment = Deployment::managed(&[]);
    // A subcommand that runs: a process that holds the lock that one holds.
    deployment.server.shell_ok(&format!(
        "mkdir -p {DEPLOYMENT}/state && cd {DEPLOYMENT}/state \
         && exec 7>> operating && flock -n 7 \
         && {{ sleep 600 < /dev/null > /dev/null 2>&1 & }}"
    ));

    let refused = deployment.server.deploy(&["end-sessions", "--all"]);

    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("another of the operator's subcommands is running"),
        "{}",
        lossy(&refused)
    );
}

#[tokio::test]
async fn ops_14_the_task_after_a_deploy_killed_before_its_replicas_serve_puts_back_its_files() {
    let deployment = Deployment::managed(&[]);
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
async fn ops_19_while_both_colors_run_and_no_deploy_recorded_which_serves_the_next_deploy_goes_on()
{
    let deployment = Deployment::managed(&[]);
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    // A deploy stops once both colors run, and no deploy recorded which serves, as after a first
    // deploy that stopped before it recorded its color.
    let stopping = deployment
        .apply_until(&waiting_image(&deployment), "starting 2 green replicas")
        .await;
    deployment.server.shell_ok(
        "timeout 120 sh -c 'until [ \"$(docker ps --quiet --filter label=com.docker.compose.service=api-green | wc -l)\" -eq 2 ]; do sleep 1; done'",
    );
    stopping.stop(&deployment.server, "KILL");
    deployment
        .server
        .shell_ok(&format!("rm {DEPLOYMENT}/state/live"));

    let status = deployment.server.deploy(&["status"]);
    let next = deployment.apply(&deployment.api, &[]).await;

    assert!(
        lossy(&status).contains(
            "serving: both colors, as while a deploy switches; blue served before the other started"
        ),
        "{}",
        lossy(&status)
    );
    assert!(next.status.success(), "{}", lossy(&next));
    assert!(
        lossy(&next).contains("2 green replicas serve"),
        "{}",
        lossy(&next)
    );
}

#[tokio::test]
async fn ops_14_a_put_back_that_fails_says_so_and_the_next_task_puts_the_files_back() {
    let deployment = Deployment::managed(&[]);
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
async fn ops_14_a_deploy_that_ends_once_its_replicas_serve_keeps_its_files() {
    let deployment = Deployment::managed(&[]);
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
async fn ops_14_a_deploy_removes_the_images_and_the_release_files_that_nothing_uses() {
    let deployment = Deployment::managed(&[]);
    // Three releases of one repository, as the API's are, and an image of another.
    let releases: Vec<String> = ["first", "second", "third"]
        .iter()
        .map(|tag| {
            deployment.image_tagged(
                &deployment.api,
                "release",
                tag,
                &format!("LABEL e2e.release={tag}"),
            )
        })
        .collect();
    let other = deployment.image_from(&deployment.api, "other", "LABEL e2e.other=true");
    deployment.server.write(
        &format!("{DEPLOYMENT}/releases/0.0.1/deploy.sh"),
        "# An earlier release's files.\n",
    );

    let mut outputs = Vec::new();
    for release in &releases {
        outputs.push(deployment.apply(release, &[]).await);
    }

    for output in &outputs {
        assert!(output.status.success(), "{}", lossy(output));
    }
    let third = lossy(&outputs[2]);
    assert!(third.contains("that nothing uses"), "{third}");
    let present = |image: &str| {
        deployment
            .server
            .shell(&format!("docker image inspect {image} > /dev/null 2>&1"))
            .status
            .success()
    };
    assert!(
        !present(&releases[0]),
        "the release before the previous one"
    );
    assert!(
        present(&releases[1]),
        "the previous release, which deploy.env names for the color that stopped"
    );
    assert!(present(&releases[2]), "the live release");
    assert!(present(&other), "an image of another repository");
    deployment
        .server
        .shell_ok(&format!("test ! -e {DEPLOYMENT}/releases/0.0.1"));
}

#[tokio::test]
async fn ops_14_every_container_of_a_deployment_keeps_a_capped_log() {
    // The bundled database and the self-hosted stack, which run the most services.
    let deployment = Deployment::bundled(&[("TELEMETRY", "self-hosted")]);

    let applied = deployment.apply(&deployment.api, &[]).await;

    assert!(applied.status.success(), "{}", lossy(&applied));
    let logs = deployment.server.shell_ok(
        "docker ps --all --quiet --filter label=com.docker.compose.project=deployment \
         | xargs docker inspect --format \
         '{{.Name}} {{.HostConfig.LogConfig.Type}} {{index .HostConfig.LogConfig.Config \"max-size\"}} {{index .HostConfig.LogConfig.Config \"max-file\"}}'",
    );
    let services = vec![
        "caddy",
        "api-blue",
        "postgres",
        "backup",
        "collector",
        "loki",
    ];
    // part: files
    let services = [services, vec!["scanner"]].concat();
    // end part: files
    for service in services {
        assert!(logs.contains(&format!("-{service}-")), "{service}: {logs}");
    }
    for line in logs.lines() {
        assert!(line.ends_with(" local 20m 5"), "a capped log: {logs}");
    }
}

#[tokio::test]
async fn ops_14_a_deploy_stops_the_services_that_the_settings_no_longer_choose() {
    let deployment = Deployment::with_a_managed_backend().await;
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
async fn ops_14_a_deploy_refuses_an_option_it_doesn_t_know() {
    let deployment = Deployment::managed(&[]);

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
async fn ops_14_a_deploy_restarts_the_bundled_database_only_for_other_files_and_with_consent() {
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
async fn obs_04_the_bundled_database_logs_no_value_that_a_constraint_refused() {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    // An insert that binds its value, as each statement of the API's does, twice, so a unique
    // constraint refuses the second.
    deployment.server.write(
        "/root/refused.sql",
        "CREATE TEMPORARY TABLE refused (address text UNIQUE);\n\
         INSERT INTO refused VALUES ($1) \\bind 'ada.refused@example.com' \\g\n\
         INSERT INTO refused VALUES ($1) \\bind 'ada.refused@example.com' \\g\n",
    );

    let refused = deployment.server.shell(
        "docker exec --interactive deployment-postgres-1 \
         psql --username=postgres --dbname=api < /root/refused.sql",
    );
    let log = lossy(&deployment.server.shell("docker logs deployment-postgres-1"));

    // The client is told which value the constraint refused, and the log isn't.
    let told = lossy(&refused);
    assert!(told.contains("ada.refused@example.com"), "{told}");
    let violation = "duplicate key value violates unique constraint";
    assert!(log.contains(violation), "{log}");
    assert!(!log.contains("ada.refused@example.com"), "{log}");
}

#[tokio::test]
async fn ops_14_a_secret_that_the_setup_replaced_recreates_the_services_that_read_it() {
    let deployment = Deployment::bundled(&[]);
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
    let (postgres, collector) = (
        deployment.server.running("postgres"),
        deployment.server.running("collector-managed"),
    );
    // As `just server-setup` writes a secret: a new file, renamed into place.
    let replace = |name: &str, value: &str| {
        deployment.server.shell_ok(&format!(
            "cd {DEPLOYMENT}/secrets && printf '%s' '{value}' > {name}.next && mv {name}.next {name}"
        ));
    };
    let key_secret = deployment
        .server
        .shell_ok(&format!("cat {DEPLOYMENT}/secrets/backup_s3_key_secret"));

    let unchanged = deployment.apply(&deployment.api, &[]).await;
    let kept_collector = deployment.server.running("collector-managed");
    replace("managed_otlp_token", "a token that the setup replaced");
    let new_token = deployment.apply(&deployment.api, &[]).await;
    let new_collector = deployment.server.running("collector-managed");
    replace("backup_s3_key_secret", "a secret that the setup replaced");
    let refused = deployment.apply(&deployment.api, &[]).await;
    let refused_postgres = deployment.server.running("postgres");
    // The same secret again, in a new file: its fingerprint is its content's.
    replace("backup_s3_key_secret", key_secret.trim_end());
    let same_again = deployment.apply(&deployment.api, &[]).await;
    let same_postgres = deployment.server.running("postgres");
    // A key pair that reaches the bucket, the operator's of the buckets' stand-in, in place of
    // the server's, then a deploy with consent.
    replace("backup_s3_key", "operator");
    replace("backup_s3_key_secret", "operator-secret");
    let consented = deployment
        .apply(&deployment.api, &["--allow-restart"])
        .await;
    let consented_postgres = deployment.server.running("postgres");
    let read = deployment
        .server
        .shell_ok("docker exec deployment-postgres-1 cat /run/secrets/backup_s3_key_secret");

    assert!(unchanged.status.success(), "{}", lossy(&unchanged));
    assert_eq!(kept_collector, collector);
    assert!(new_token.status.success(), "{}", lossy(&new_token));
    assert_eq!(new_collector.len(), 1);
    assert_ne!(new_collector, collector);
    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("this deploy changes postgres"),
        "{}",
        lossy(&refused)
    );
    assert_eq!(refused_postgres, postgres);
    assert!(same_again.status.success(), "{}", lossy(&same_again));
    assert_eq!(same_postgres, postgres);
    assert!(consented.status.success(), "{}", lossy(&consented));
    assert_eq!(consented_postgres.len(), 1);
    assert_ne!(consented_postgres, postgres);
    assert_eq!(read, "operator-secret");
}

#[tokio::test]
async fn ops_14_a_deploy_gives_the_bundled_database_its_own_secrets_once_the_settings_choose_it() {
    let deployment = Deployment::managed(&[]);
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
async fn ops_14_a_deploy_refuses_a_managed_database_while_its_urls_name_the_bundled_one() {
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
async fn ops_14_a_deploy_that_leaves_the_bundled_database_stops_it_and_its_backups_status() {
    let deployment = Deployment::managed(&[]);
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

/// What the subcommands that an operator runs against the accounts wrote, each run by
/// [`operator_subcommands_run`].
struct Operated {
    /// `remove-second-factor` with an address that no account holds.
    removal: Output,
    /// `end-sessions` with an address that no account holds.
    ending: Output,
    /// `end-sessions --all`.
    every_ending: Output,
    // part: workspace_setup
    /// `setup-workspace` with the account's address.
    set_up: Output,
    /// `setup-workspace` with an address that no account holds.
    no_owner: Output,
    // end part: workspace_setup
    // part: superadmins
    /// `superadmin add` with the account's address.
    added: Output,
    /// `superadmin remove` with an address that no account holds.
    nobody: Output,
    /// `superadmin list`.
    listed: Output,
    // end part: superadmins
}

/// Runs the subcommands that an operator runs against the accounts on `deployment`, which serves,
/// and whose one account, `ada@example.com`'s, has one session: each runs with the image and the
/// settings of the color that serves, against the deployment's database.
fn operator_subcommands_run(deployment: &Deployment) -> Operated {
    let run = |arguments: &[&str]| deployment.server.deploy(arguments);
    Operated {
        removal: run(&["remove-second-factor", "nobody@example.com"]),
        ending: run(&["end-sessions", "nobody@example.com"]),
        every_ending: run(&["end-sessions", "--all"]),
        // part: workspace_setup
        set_up: run(&["setup-workspace", "Acme Labs", "ada@example.com"]),
        no_owner: run(&["setup-workspace", "Globex", "nobody@example.com"]),
        // end part: workspace_setup
        // part: superadmins
        added: run(&["superadmin", "add", "ada@example.com"]),
        nobody: run(&["superadmin", "remove", "nobody@example.com"]),
        listed: run(&["superadmin", "list"]),
        // end part: superadmins
    }
}

/// Asserts that each subcommand of `operated` ended with the one line that says what it did, or
/// why it changed nothing, which names no address (OPS-03, OBS-04).
fn assert_the_subcommands_ended(operated: &Operated) {
    // The removal of a second factor refuses an address that no account holds (AUTH-072), with
    // the message of the code's own error.
    let written = lossy(&operated.removal);
    assert_eq!(operated.removal.status.code(), Some(1), "{written}");
    assert!(written.contains("startup failed"), "{written}");
    assert!(
        written.contains(&app::RemovalRefused::NoAccount.to_string()),
        "{written}"
    );
    assert!(!written.contains("nobody@example.com"), "{written}");
    // The end of sessions refuses an address that no account holds, and, for every account's,
    // ends the one session that the deployment holds (AUTH-094).
    let written = lossy(&operated.ending);
    assert_eq!(operated.ending.status.code(), Some(1), "{written}");
    assert!(written.contains("startup failed"), "{written}");
    assert!(
        written.contains(&app::EndingRefused::NoAccount.to_string()),
        "{written}"
    );
    assert!(!written.contains("nobody@example.com"), "{written}");
    let written = lossy(&operated.every_ending);
    assert_eq!(operated.every_ending.status.code(), Some(0), "{written}");
    assert!(written.contains("subcommand finished"), "{written}");
    assert!(
        written.contains("ended every session of every account: sessions 1, accounts 1"),
        "{written}"
    );
    assert!(!written.contains("ada@example.com"), "{written}");
    // part: workspace_setup
    // The setup of a workspace (WORKSPACES-023): with email off, it makes the account that holds
    // the address the workspace's owner, and sets nothing up for an address that no account
    // holds (WORKSPACES-026).
    let written = lossy(&operated.set_up);
    assert_eq!(operated.set_up.status.code(), Some(0), "{written}");
    assert!(written.contains("subcommand finished"), "{written}");
    assert!(written.contains("set up the workspace"), "{written}");
    assert!(written.contains("email is off"), "{written}");
    assert!(!written.contains("Acme"), "{written}");
    assert!(!written.contains("ada@example.com"), "{written}");
    let written = lossy(&operated.no_owner);
    assert_eq!(operated.no_owner.status.code(), Some(1), "{written}");
    assert!(
        written.contains(&app::SetupRefused::NoAccount.to_string()),
        "{written}"
    );
    assert!(!written.contains("nobody@example.com"), "{written}");
    // end part: workspace_setup
    // part: superadmins
    assert_the_superadmin_subcommand_ended(operated);
    // end part: superadmins
}

// part: superadmins
/// Asserts what the subcommand `superadmin` wrote, as [`assert_the_subcommands_ended`] asserts
/// what the others did: with email off, the account that holds the address became a superadmin
/// (SUPERADMINS-001), an address that no account holds ended none (SUPERADMINS-003), and the list
/// showed the operator the superadmin's address, which its log doesn't hold (SUPERADMINS-008).
fn assert_the_superadmin_subcommand_ended(operated: &Operated) {
    let written = lossy(&operated.added);
    assert_eq!(operated.added.status.code(), Some(0), "{written}");
    assert!(written.contains("subcommand finished"), "{written}");
    assert!(
        written.contains("made the account that holds the address a superadmin"),
        "{written}"
    );
    assert!(!written.contains("ada@example.com"), "{written}");
    let written = lossy(&operated.nobody);
    assert_eq!(operated.nobody.status.code(), Some(1), "{written}");
    assert!(
        written.contains(&app::SuperadminRefused::NoAccount.to_string()),
        "{written}"
    );
    assert!(!written.contains("nobody@example.com"), "{written}");
    let written = lossy(&operated.listed);
    assert_eq!(operated.listed.status.code(), Some(0), "{written}");
    assert!(
        written
            .lines()
            .any(|line| line.starts_with("  ada@example.com  ")),
        "{written}"
    );
    let logged: Vec<&str> = written
        .lines()
        .filter(|line| line.contains("subcommand finished"))
        .collect();
    assert_eq!(logged.len(), 1, "{written}");
    assert!(
        logged[0].contains("listed the deployment's superadmins: 1"),
        "{written}"
    );
    assert!(!logged[0].contains("ada@example.com"), "{written}");
}
// end part: superadmins

#[tokio::test]
async fn ops_19_the_operator_s_tasks_run_the_api_s_subcommands_against_the_deployment_s_database() {
    let deployment = Deployment::bundled(&[]);
    let applied = deployment.apply(&deployment.api, &[]).await;
    assert!(applied.status.success(), "{}", lossy(&applied));
    deployment.registered("ada@example.com");

    let operated = operator_subcommands_run(&deployment);

    assert_the_subcommands_ended(&operated);
}

#[tokio::test]
async fn ops_19_status_shows_what_serves_and_the_backups_and_logs_show_the_services_output() {
    // With the self-hosted stack, whose images, unlike the API's and Caddy's, carry no version
    // label.
    let deployment = Deployment::bundled(&[("TELEMETRY", "self-hosted")]);
    let applied = deployment.apply(&deployment.api, &[]).await;
    assert!(applied.status.success(), "{}", lossy(&applied));
    // The data that a restore moves aside, where the restore moves it.
    deployment.server.shell_ok(
        "docker exec deployment-postgres-1 mkdir /var/lib/postgresql/18/before-restore-20261029T120000Z",
    );

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
    let containers = section(&status, "containers:");
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
    assert_backups_recorded(&status);
    assert!(
        section(&status, "moved aside by a restore")
            .iter()
            .any(|line| line
                .trim_start()
                .starts_with("before-restore-20261029T120000Z ")),
        "{status}"
    );
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

#[tokio::test]
async fn ops_19_status_shows_the_data_a_restore_moved_aside_while_postgres_doesn_t_run() {
    // A managed database, as after DATABASE left `bundled`, with the volume the bundled one left,
    // which holds what a restore moved aside; no Postgres runs.
    let deployment = Deployment::managed(&[]);
    let applied = deployment.apply(&deployment.api, &[]).await;
    assert!(applied.status.success(), "{}", lossy(&applied));
    deployment.server.shell_ok(
        "docker volume create deployment_postgres > /dev/null \
         && docker run --rm --volume deployment_postgres:/data --entrypoint mkdir \
         \"$(docker inspect --format '{{ .Config.Image }}' deployment-caddy-1)\" \
         -p /data/18/before-restore-20261029T120000Z",
    );

    let status = deployment.server.deploy(&["status"]);

    assert!(status.status.success(), "{}", lossy(&status));
    let status = lossy(&status);
    assert!(deployment.server.running("postgres").is_empty());
    assert!(
        section(&status, "moved aside by a restore")
            .iter()
            .any(|line| line
                .trim_start()
                .starts_with("before-restore-20261029T120000Z ")),
        "{status}"
    );
}

/// The lines of the section of `status` under the line that starts with `heading`, each indented.
fn section<'a>(status: &'a str, heading: &str) -> Vec<&'a str> {
    status
        .lines()
        .skip_while(|line| !line.starts_with(heading))
        .skip(1)
        .take_while(|line| line.starts_with("  "))
        .collect()
}

/// Asserts that `status` shows when the backups last archived the log and completed a base
/// backup, and that no archive failed after the last one that succeeded.
fn assert_backups_recorded(status: &str) {
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
}
