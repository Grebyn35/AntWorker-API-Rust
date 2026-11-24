//! The reference deployment with its observability, the self-hosted stack or a managed backend
//! (D42), on a server of the tests' own.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader},
    path::Path,
    sync::mpsc,
    thread,
    time::Duration,
};

use crate::{
    deployment::{
        Deployment, GRAFANA_ADDRESS, GRAFANA_ADMIN_PASSWORD, GRAFANA_ADMIN_USER, get, get_traced,
        get_with_cookie, get_within, get_within_as, pairs, post, post_for_cookies,
        post_with_cookie, send, streaming_client, unanswered_as,
    },
    docker::{curl_image, lossy},
    server::DEPLOYMENT,
    support,
};

/// The names under which a client's personal data would reach a span or a log line of the proxy,
/// which none keeps (OBS-04): OpenTelemetry's, as Caddy names its spans' attributes, and Caddy's
/// own, as its log lines name their fields.
const PERSONAL: [&str; 9] = [
    "client.address",
    "network.peer.address",
    "user_agent.original",
    "url.path",
    "remote_ip",
    "client_ip",
    "User-Agent",
    "\"uri\"",
    "\"headers\"",
];

/// The `User-Agent` of the request that no replica answers, a value of the tests' own, which the
/// proxy's spans and log lines hold under no name (OBS-04).
const AGENT: &str = "the-e2e-tests-own-agent/7.1";

/// The lines of the stand-in backend's file that hold `trace`, once a line of spans and one of
/// log records do. The collector decides on a trace 15 seconds after its first span arrived
/// (OBS-08).
fn received(deployment: &Deployment, trace: &str) -> String {
    deployment.server.shell_ok(&format!(
        "timeout 120 sh -c 'until grep resourceSpans /root/backend/out/telemetry.json 2> /dev/null | grep -q {trace} \
         && grep resourceLogs /root/backend/out/telemetry.json | grep -q {trace}; do sleep 2; done' \
         && grep {trace} /root/backend/out/telemetry.json"
    ))
}

/// Stops the deployment's replicas, then asks the proxy for a resource, with `AGENT` as its
/// `User-Agent`, which no replica answers, and returns the trace ID of its 503 problem.
fn trace_of_a_request_no_replica_answers(deployment: &Deployment) -> String {
    deployment
        .server
        .shell_ok("docker stop deployment-api-blue-1 deployment-api-blue-2 > /dev/null");
    let (status, _, body) = get_within_as(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
        Duration::from_secs(2),
        Some(AGENT),
    );
    assert_eq!(status, 503, "{body}");
    let problem: serde_json::Value = serde_json::from_str(&body).expect("a problem");
    problem["trace_id"]
        .as_str()
        .expect("its trace ID")
        .to_owned()
}

/// Asserts that a deploy of `deployment` with the self-hosted stack takes only an administrator
/// of Grafana's that its operator chose: a name of 1 to 64 of its characters that isn't the one
/// every Grafana starts with, and a password of 15 characters or more, of which the deploy
/// generates none (SEC-14). It
/// leaves the settings with the stack, and the password that the setup wrote.
async fn assert_a_deploy_takes_only_the_operator_s_administrator(deployment: &Deployment) {
    let secret = format!("{DEPLOYMENT}/secrets/grafana_admin_password");
    let long = "a".repeat(65);
    for (name, refusal) in [
        (
            "Admin",
            "GRAFANA_ADMIN_USER is the name that every Grafana starts with",
        ),
        ("", "GRAFANA_ADMIN_USER is required"),
        ("ada lovelace", "GRAFANA_ADMIN_USER is 1 to 64 letters"),
        (long.as_str(), "GRAFANA_ADMIN_USER is 1 to 64 letters"),
    ] {
        deployment.set(&[("TELEMETRY", "self-hosted"), ("GRAFANA_ADMIN_USER", name)]);
        let named = deployment
            .apply(&deployment.api, &["--allow-restart"])
            .await;
        assert!(!named.status.success(), "{name}: {}", lossy(&named));
        assert!(lossy(&named).contains(refusal), "{name}: {}", lossy(&named));
    }
    deployment.set(&[("TELEMETRY", "self-hosted")]);
    deployment.server.write(&secret, "too short");
    let short = deployment
        .apply(&deployment.api, &["--allow-restart"])
        .await;
    deployment.server.shell_ok(&format!("rm {secret}"));
    let missing = deployment
        .apply(&deployment.api, &["--allow-restart"])
        .await;
    deployment.server.write(&secret, GRAFANA_ADMIN_PASSWORD);
    assert!(!short.status.success(), "{}", lossy(&short));
    assert!(
        lossy(&short).contains("grafana_admin_password holds fewer than 15 characters"),
        "{}",
        lossy(&short)
    );
    assert!(!missing.status.success(), "{}", lossy(&missing));
    assert!(
        lossy(&missing).contains("grafana_admin_password is missing"),
        "{}",
        lossy(&missing)
    );
}

/// What `deployment`'s Grafana answers a sign-in with the name and the password `as_whom`, and
/// the cookies it sets, through a client that trusts `root`.
fn grafana_sign_in(
    deployment: &Deployment,
    root: &str,
    as_whom: (&str, &str),
) -> (u16, Vec<String>) {
    post_for_cookies(
        root.to_owned(),
        deployment.url_at(GRAFANA_ADDRESS, "https", 443, "/login"),
        serde_json::json!({"user": as_whom.0, "password": as_whom.1}).to_string(),
    )
}

/// Asserts that a request that names neither of the deployment's addresses, such as one for the
/// server's own, gets no answer, over HTTPS and in plain HTTP (SEC-14).
fn assert_the_proxy_answers_no_other_host(deployment: &Deployment, root: &str) {
    for (scheme, port) in [("https", 443), ("http", 80)] {
        for host in ["other.example", "127.0.0.1"] {
            assert!(
                unanswered_as(root.to_owned(), deployment.url(scheme, port, "/"), host),
                "{scheme}, for {host}: a request that names neither address got an answer"
            );
        }
    }
}

/// Asserts that Prometheus, Loki, Tempo and Alertmanager, which ask no credential, join one
/// network, which the collector and Grafana join beside them and neither the proxy nor a replica
/// does, that each answers Go's profiling endpoints on that network, and that Prometheus answers
/// on that network and on none of the proxy's and the replicas' (SEC-14).
fn assert_the_stores_answer_the_collector_and_grafana_alone(deployment: &Deployment) {
    let networks = |service: &str| {
        let mut joined: Vec<String> = deployment
            .server
            .running(service)
            .iter()
            .flat_map(|container| {
                let names = deployment.server.shell_ok(&format!(
                    "docker inspect --format \
                     '{{{{ range $name, $network := .NetworkSettings.Networks }}}}{{{{ $name }}}} {{{{ end }}}}' \
                     {container}"
                ));
                names
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect::<Vec<_>>()
            })
            .collect();
        joined.sort();
        joined.dedup();
        joined
    };
    for store in ["prometheus", "loki", "tempo", "alertmanager"] {
        assert_eq!(networks(store), ["deployment_stack"], "{store}");
    }
    assert_eq!(
        networks("collector"),
        [
            "deployment_edge",
            "deployment_stack",
            "deployment_telemetry"
        ]
    );
    assert_eq!(networks("grafana"), ["deployment_edge", "deployment_stack"]);
    let mut senders = [
        networks("caddy"),
        networks("api-blue"),
        networks("api-green"),
    ]
    .concat();
    senders.sort();
    senders.dedup();
    assert!(
        senders
            .iter()
            .any(|network| network == "deployment_telemetry"),
        "{senders:?}"
    );
    let curl = curl_image();
    let healthy = |network: &str| {
        format!(
            "docker run --rm --quiet --network {network} {curl} --silent --fail --max-time 5 \
             http://prometheus:9090/-/healthy"
        )
    };
    deployment.server.shell_ok(&format!(
        "timeout 60 sh -c 'until {}; do sleep 1; done'",
        healthy("deployment_stack")
    ));
    assert_each_store_answers_go_s_profiling_endpoints_on_its_network(deployment, &curl);
    for network in &senders {
        let answered = deployment.server.shell(&healthy(network));
        assert!(
            !answered.status.success(),
            "{network}: {}",
            lossy(&answered)
        );
    }
}

/// Asserts that Prometheus, Loki, Tempo and Alertmanager each answer Go's profiling endpoints on
/// the network that they join, through `curl`, the image of the tools' curl (SEC-14).
fn assert_each_store_answers_go_s_profiling_endpoints_on_its_network(
    deployment: &Deployment,
    curl: &str,
) {
    for (store, port) in [
        ("prometheus", 9090),
        ("loki", 3100),
        ("tempo", 3200),
        ("alertmanager", 9093),
    ] {
        deployment.server.shell_ok(&format!(
            "timeout 120 sh -c 'until docker run --rm --quiet --network deployment_stack {curl} \
             --silent --fail --max-time 5 --output /dev/null http://{store}:{port}/debug/pprof/; \
             do sleep 1; done'"
        ));
    }
}

/// The server's ports that each container publishes, of those that publish one, from
/// `containers`, a line for each container with its name, a space and its ports, as `docker ps`
/// lists them: in each item of the ports that holds `->`, what stands between the last `:`
/// before it and it.
fn published(containers: &str) -> BTreeMap<&str, BTreeSet<&str>> {
    containers
        .lines()
        .filter_map(|line| {
            let (name, ports) = line.split_once(' ')?;
            let ports: BTreeSet<&str> = ports
                .split([' ', ','])
                .filter_map(|item| item.split_once("->"))
                .filter_map(|(host, _)| host.rsplit_once(':'))
                .map(|(_, port)| port)
                .collect();
            (!ports.is_empty()).then_some((name, ports))
        })
        .collect()
}

#[tokio::test]
async fn sec_14_a_server_publishes_only_the_proxy_and_grafana_signs_every_viewer_in() {
    // The bundled database, and a stack that a later deploy adds, after one that ran without it.
    let deployment = Deployment::bundled(&[]);
    let without = deployment.apply(&deployment.api, &[]).await;
    assert!(without.status.success(), "{}", lossy(&without));
    assert_a_deploy_takes_only_the_operator_s_administrator(&deployment).await;
    // TELEMETRY from off, which Caddy reads, so it restarts.
    let deployed = deployment
        .apply(&deployment.api, &["--allow-restart"])
        .await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    let root = deployment.server.caddy_root();

    let containers = deployment.server.shell_ok(
        "docker ps --filter label=com.docker.compose.project=deployment --format '{{.Names}} {{.Ports}}'",
    );
    let (home, home_headers) = get(
        root.clone(),
        deployment.url_at(GRAFANA_ADDRESS, "https", 443, "/"),
    );
    let (_, page_headers) = get(
        root.clone(),
        deployment.url_at(GRAFANA_ADDRESS, "https", 443, "/login"),
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
    let (metrics, _, exposition) = get_within(
        root.clone(),
        deployment.url_at(GRAFANA_ADDRESS, "https", 443, "/metrics"),
        Duration::from_secs(10),
    );
    let (sign_in, cookies) = grafana_sign_in(
        &deployment,
        &root,
        (GRAFANA_ADMIN_USER, GRAFANA_ADMIN_PASSWORD),
    );
    // Grafana's own `admin`, in whose place the operator's administrator stands, signs nobody
    // in.
    let (as_admin, _) = grafana_sign_in(&deployment, &root, ("admin", GRAFANA_ADMIN_PASSWORD));
    let (as_default, _) = grafana_sign_in(&deployment, &root, ("admin", "admin"));

    assert_eq!(
        published(&containers),
        BTreeMap::from([("deployment-caddy-1", BTreeSet::from(["443", "80"]))]),
        "{containers}"
    );
    assert!(containers.contains("deployment-grafana-1"), "{containers}");
    assert_eq!(home, 302);
    let login = home_headers["location"].to_str().unwrap();
    assert!(login.ends_with("/login"), "{login}");
    assert_eq!(
        home_headers["strict-transport-security"],
        "max-age=31536000; includeSubDomains"
    );
    assert_eq!(search, 401);
    // Grafana's own metrics, which nothing of the stack scrapes, answer no one.
    assert_ne!(metrics, 200, "{exposition}");
    assert!(!exposition.contains("# TYPE"), "{exposition}");
    assert_eq!(sign_up, 401, "{refusal}");
    assert!(refusal.contains("signup is disabled"), "{refusal}");
    assert_eq!(sign_in, 200, "{cookies:?}");
    assert_eq!(as_admin, 401);
    assert_eq!(as_default, 401);
    assert_the_session_s_cookie_is_this_host_s_alone(&cookies);
    assert_a_browser_guards_grafana_s_pages(&page_headers);
    assert_grafana_lists_no_directory_and_echoes_no_trace(&deployment, &root, &cookies);
    assert_grafana_shares_nothing_that_opens_without_a_sign_in(&deployment, &root, &cookies);
    assert_the_stores_answer_the_collector_and_grafana_alone(&deployment);
    assert_the_proxy_answers_no_other_host(&deployment, &root);
}

#[tokio::test]
async fn ops_17_the_proxy_redirects_plain_http_at_grafana_s_address_to_https() {
    let deployment = Deployment::managed(&[("TELEMETRY", "self-hosted")]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));

    // Grafana's pages are for a browser, so plain HTTP at its address leads to HTTPS.
    let (plain, headers) = get(
        deployment.server.caddy_root(),
        deployment.url_at(GRAFANA_ADDRESS, "http", 80, "/login"),
    );

    assert_eq!(plain, 308);
    let location = headers["location"].to_str().unwrap();
    assert!(
        location.starts_with(&format!("https://{GRAFANA_ADDRESS}")),
        "{location}"
    );
}

/// Of the cookies that Grafana's sign-in sets, the session's is `__Host-grafana_session`, for
/// this host alone and for 30 days, `HttpOnly`, `Secure` and `SameSite=Lax`, and the only other
/// is Grafana's own, of the time at which the session's token changes next, which is `Secure`
/// and `SameSite=Lax` too (SEC-14).
fn assert_the_session_s_cookie_is_this_host_s_alone(cookies: &[String]) {
    let session: Vec<&String> = cookies
        .iter()
        .filter(|cookie| cookie.starts_with("__Host-grafana_session="))
        .collect();
    assert_eq!(session.len(), 1, "{cookies:?}");
    let attributes: Vec<&str> = session[0].split("; ").skip(1).collect();
    for attribute in [
        "Path=/",
        "Max-Age=2592000",
        "HttpOnly",
        "Secure",
        "SameSite=Lax",
    ] {
        assert!(attributes.contains(&attribute), "{attribute}: {cookies:?}");
    }
    assert!(
        !attributes
            .iter()
            .any(|attribute| attribute.starts_with("Domain")),
        "{cookies:?}"
    );
    let others: Vec<&String> = cookies
        .iter()
        .filter(|cookie| !cookie.starts_with("__Host-grafana_session="))
        .collect();
    let names: Vec<&str> = others
        .iter()
        .filter_map(|cookie| cookie.split('=').next())
        .collect();
    assert_eq!(names, ["grafana_session_expiry"]);
    let expiry: Vec<&str> = others[0].split("; ").skip(1).collect();
    for attribute in ["Secure", "SameSite=Lax"] {
        assert!(expiry.contains(&attribute), "{attribute}: {cookies:?}");
    }
}

/// The cookies that a response set, `cookies`, as a browser sends them back: each one's name and
/// value.
fn sent_back(cookies: &[String]) -> String {
    cookies
        .iter()
        .filter_map(|cookie| cookie.split(';').next())
        .collect::<Vec<_>>()
        .join("; ")
}

/// Asserts that Grafana lists neither directory of its own assets: a request for one without a
/// session goes to the sign-in page, and one with the cookies of a sign-in, `cookies`, gets 404.
/// Asserts too that a `TRACE`, which Caddy passes on to Grafana, gets neither a success nor an
/// echo of the request (SEC-14).
fn assert_grafana_lists_no_directory_and_echoes_no_trace(
    deployment: &Deployment,
    root: &str,
    cookies: &[String],
) {
    let session = sent_back(cookies);
    let directories = ["/public/", "/public/build/"]
        .map(|directory| deployment.url_at(GRAFANA_ADDRESS, "https", 443, directory));
    let signed_out = directories.clone().map(|url| {
        let (status, headers) = get(root.to_owned(), url);
        let location = headers
            .get("location")
            .and_then(|location| location.to_str().ok())
            .unwrap_or_default();
        let page = location.split('?').next().unwrap_or_default().to_owned();
        (status, page.ends_with("/login"))
    });
    let signed_in = directories.map(|url| get_with_cookie(root.to_owned(), url, session.clone()));
    let (traced, echo) = send(
        root.to_owned(),
        reqwest::Method::TRACE,
        deployment.url_at(GRAFANA_ADDRESS, "https", 443, "/login"),
    );

    assert_eq!(signed_out, [(302, true), (302, true)]);
    assert_eq!(signed_in, [404, 404]);
    assert!(!(200..300).contains(&traced), "{traced}: {echo}");
    assert!(!echo.contains("TRACE"), "{echo}");
}

/// Asserts that Grafana's administrator, signed in with `cookies`, makes neither a snapshot of a
/// dashboard nor an externally shared dashboard, each of which opens without a sign-in (SEC-14).
fn assert_grafana_shares_nothing_that_opens_without_a_sign_in(
    deployment: &Deployment,
    root: &str,
    cookies: &[String],
) {
    let session = sent_back(cookies);
    let (snapshot, snapshot_refusal) = post_with_cookie(
        root.to_owned(),
        deployment.url_at(GRAFANA_ADDRESS, "https", 443, "/api/snapshots"),
        session.clone(),
        serde_json::json!({"dashboard": {"title": "A snapshot", "uid": "api-overview", "panels": []}, "expires": 3600})
            .to_string(),
    );
    let (shared, shared_refusal) = post_with_cookie(
        root.to_owned(),
        deployment.url_at(
            GRAFANA_ADDRESS,
            "https",
            443,
            "/api/dashboards/uid/api-overview/public-dashboards",
        ),
        session,
        serde_json::json!({"isEnabled": true, "share": "public"}).to_string(),
    );

    assert!(
        !(200..300).contains(&snapshot),
        "{snapshot}: {snapshot_refusal}"
    );
    assert!(
        snapshot_refusal.contains("Dashboard Snapshots are disabled"),
        "{snapshot_refusal}"
    );
    assert!(!(200..300).contains(&shared), "{shared}: {shared_refusal}");
    assert!(shared_refusal.contains("Not found"), "{shared_refusal}");
}

/// The headers of a page of Grafana's tell a browser to guess no type, to name the page to no
/// other site, to load no plugin object, to take a base address from Grafana alone, and to show
/// the page in no frame (SEC-12).
fn assert_a_browser_guards_grafana_s_pages(headers: &reqwest::header::HeaderMap) {
    assert_eq!(headers["x-content-type-options"], "nosniff");
    assert_eq!(headers["referrer-policy"], "same-origin");
    let policy = headers["content-security-policy"].to_str().unwrap();
    for directive in [
        "object-src 'none'",
        "base-uri 'self'",
        "frame-ancestors 'none'",
    ] {
        assert!(
            policy.split(';').any(|part| part.trim() == directive),
            "{directive}: {policy}"
        );
    }
}

/// What Grafana answers a request for the signed-in user with `session`, its session's cookie as
/// a sign-in set it, once Docker reports Grafana healthy, which a deploy that recreates it
/// doesn't wait for.
fn grafana_user(deployment: &Deployment, root: &str, session: &str) -> u16 {
    deployment.server.shell_ok(
        "timeout 60 sh -c 'until [ \"$(docker inspect --format \"{{ .State.Health.Status }}\" \
         deployment-grafana-1)\" = healthy ]; do sleep 1; done'",
    );
    get_with_cookie(
        root.to_owned(),
        deployment.url_at(GRAFANA_ADDRESS, "https", 443, "/api/user"),
        session.split(';').next().expect("a cookie").to_owned(),
    )
}

/// The keys of the rate limits' buckets that the deployment's database holds, each a keyed hash
/// of what it counts (PLATFORM-057), once a request from the tests' address has counted in its
/// own.
fn bucket_keys_after_a_request(deployment: &Deployment) -> Vec<String> {
    let (status, _) = get(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
    );
    assert_eq!(status, 200);
    deployment
        .sql("SELECT encode(key_hash, 'hex') FROM rate_limit_buckets ORDER BY 1")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Asserts that `replace-key` refused, changing nothing, with a line that holds `why`.
fn assert_no_key_was_replaced(refused: &std::process::Output, why: &str) {
    assert_eq!(refused.status.code(), Some(1), "{}", lossy(refused));
    assert!(lossy(refused).contains(why), "{}", lossy(refused));
}

#[tokio::test]
async fn ops_19_a_key_that_the_operator_replaced_reaches_its_services_at_the_next_deploy() {
    let deployment = Deployment::bundled(&[("TELEMETRY", "self-hosted")]);
    let secret = |name: &str| {
        deployment
            .server
            .shell_ok(&format!("cat {DEPLOYMENT}/secrets/{name}"))
    };
    let replace = |name: &str| deployment.server.deploy(&["replace-key", name]);
    let administrator = (GRAFANA_ADMIN_USER, GRAFANA_ADMIN_PASSWORD);
    // No deploy has generated a key yet, so none is there to replace.
    assert_no_key_was_replaced(&replace("hmac_key"), "holds no key to replace");
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let root = deployment.server.caddy_root();
    let (hmac, grafana_key) = (secret("hmac_key"), secret("grafana_secret_key"));
    let ring = secret("encryption_keys");
    let grafana = deployment.server.running("grafana");
    let (signed_in, cookies) = grafana_sign_in(&deployment, &root, administrator);
    assert_eq!(signed_in, 200, "{cookies:?}");
    let session = cookies
        .iter()
        .find(|cookie| cookie.starts_with("__Host-grafana_session="))
        .expect("the session's cookie");
    let before = bucket_keys_after_a_request(&deployment);
    assert_eq!(grafana_user(&deployment, &root, session), 200);

    // The ring is the operator's, which the setup writes: the task replaces only the two keys
    // that a deploy generates.
    let not_generated = replace("encryption_keys");
    let (replaced_hmac, replaced_grafana) = (replace("hmac_key"), replace("grafana_secret_key"));
    let (new_hmac, new_grafana_key) = (secret("hmac_key"), secret("grafana_secret_key"));
    // The services that run keep the keys they started with: the replicas count the tests'
    // address in the bucket they counted it in, and Grafana knows its viewer's session.
    let kept = bucket_keys_after_a_request(&deployment);
    let kept_session = grafana_user(&deployment, &root, session);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    let after = bucket_keys_after_a_request(&deployment);
    let ended_session = grafana_user(&deployment, &root, session);
    let (again, _) = grafana_sign_in(&deployment, &root, administrator);

    assert_no_key_was_replaced(
        &not_generated,
        "replace-key takes hmac_key or grafana_secret_key",
    );
    assert_eq!(secret("encryption_keys"), ring);
    assert!(replaced_hmac.status.success(), "{}", lossy(&replaced_hmac));
    assert!(
        replaced_grafana.status.success(),
        "{}",
        lossy(&replaced_grafana)
    );
    // A key of the keyed hashes is the base64 of 32 bytes, and Grafana's 24 bytes in hex.
    assert_ne!(new_hmac, hmac);
    assert_eq!(
        new_hmac.trim_end().len(),
        44,
        "{} characters",
        new_hmac.len()
    );
    assert_ne!(new_grafana_key, grafana_key);
    assert_eq!(new_grafana_key.trim_end().len(), 48);
    assert_eq!(kept, before);
    assert_eq!(kept_session, 200);
    // The deploy's replicas hash under the new key, so they count the same address in a bucket
    // that no request counted in before, and the Grafana that the deploy recreated knows no
    // session that the old key hashed, while its administrator signs in again.
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    assert!(
        after.iter().any(|key| !before.contains(key)),
        "{before:?} then {after:?}"
    );
    assert_ne!(deployment.server.running("grafana"), grafana);
    assert_eq!(ended_session, 401);
    assert_eq!(again, 200);
}

/// The names of the collector's own metrics that the alert rules and the dashboards read: in
/// deploy/observability/prometheus/rules.yaml and each file of
/// deploy/observability/grafana/dashboards/, each run of letters, digits and underscores that
/// starts with `otelcol_`.
#[expect(
    clippy::disallowed_methods,
    reason = "tests may block, each on a runtime of its own (CODE-05)"
)]
fn collector_metrics_read() -> BTreeSet<String> {
    let observability = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../deploy/observability");
    let mut files = vec![observability.join("prometheus/rules.yaml")];
    files.extend(
        fs::read_dir(observability.join("grafana/dashboards"))
            .expect("the dashboards")
            .map(|entry| entry.expect("a dashboard").path()),
    );
    let mut names = BTreeSet::new();
    for file in files {
        let text = fs::read_to_string(&file).expect("a file of the stack");
        for (at, _) in text.match_indices("otelcol_") {
            let name: String = text[at..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            names.insert(name);
        }
    }
    names
}

/// The collector's metrics that it creates at their first event, of which a deployment holds no
/// series until that event happens: a failed send.
// part: client_telemetry
/// And a point of clients' metrics that it can't add to a series.
// end part: client_telemetry
const AT_FIRST_EVENT: &[&str] = &[
    // part: client_telemetry
    "otelcol_deltatocumulative_datapoints_total",
    // end part: client_telemetry
    "otelcol_exporter_send_failed_log_records_total",
    "otelcol_exporter_send_failed_metric_points_total",
    "otelcol_exporter_send_failed_spans_total",
];

#[tokio::test]
async fn obs_14_the_backups_the_server_s_memory_and_the_collector_report_what_the_alerts_and_the_dashboards_read()
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
    // The collector's own metrics, under the names that the alert rules and the dashboards read,
    // once the API's telemetry has passed through it, as Prometheus holds them: the collector
    // creates some only at their first event, which a deployment that fails no send holds none
    // of: the alert counts a failed send's series from its first value.
    let read: Vec<String> = collector_metrics_read()
        .into_iter()
        .filter(|name| !AT_FIRST_EVENT.contains(&name.as_str()))
        .collect();
    let waits: Vec<String> = read
        .iter()
        .map(|name| format!("grep -q {name} /root/collector.json"))
        .collect();
    deployment.server.shell(&format!(
        "timeout 180 sh -c 'until docker exec deployment-prometheus-1 wget -qO- \
         \"http://localhost:9090/api/v1/query?query=count%20by%20(__name__)%20(%7B__name__%3D~%22otelcol_.%2B%22%7D)\" \
         > /root/collector.json && {}; do sleep 5; done'",
        waits.join(" && ")
    ));
    let held: serde_json::Value =
        serde_json::from_str(&deployment.server.shell_ok("cat /root/collector.json"))
            .expect("Prometheus's answer");
    let present: BTreeSet<&str> = held["data"]["result"]
        .as_array()
        .expect("its series")
        .iter()
        .filter_map(|series| series["metric"]["__name__"].as_str())
        .collect();
    let absent: Vec<&String> = read
        .iter()
        .filter(|name| !present.contains(name.as_str()))
        .collect();
    assert!(!read.is_empty());
    assert!(
        absent.is_empty(),
        "Prometheus holds no series of {absent:?}, of the collector's metrics that a rule or a \
         dashboard reads"
    );
}

#[tokio::test]
async fn ops_14_a_deploy_installs_the_release_s_alert_rules_and_prometheus_reads_them() {
    let deployment = Deployment::managed(&[("TELEMETRY", "self-hosted")]);
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
    let deployment = Deployment::with_a_managed_backend().await;

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
    let deployment = Deployment::managed(&[("TELEMETRY", "self-hosted")]);
    // A first release whose receivers name no integration, whatever the repository's
    // alertmanager.yaml names, as a fork's names its own; the test keeps that file for the second.
    deployment.server.shell_ok(
        "cd /root/release/observability/alertmanager \
         && cp alertmanager.yaml /root/alertmanager.release.yaml \
         && printf 'route:\n  receiver: page\nreceivers:\n  - name: page\n' > alertmanager.yaml",
    );
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
         && awk -f /root/page.awk /root/alertmanager.release.yaml > alertmanager/alertmanager.yaml \
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
    let deployment = Deployment::managed(&[]);
    let mut settings = vec![("TELEMETRY".to_owned(), "managed".to_owned())];
    settings.extend(deployment.server.start_otlp_backend());
    settings.extend(deployment.server.start_managed_grafana().await);
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

/// A deployment whose telemetry goes to a managed backend, whose Grafana answers through a front
/// that can make it answer as a Grafana that is still loading.
async fn behind_a_loading_grafana() -> Deployment {
    let deployment = Deployment::managed(&[]);
    let mut settings = vec![("TELEMETRY".to_owned(), "managed".to_owned())];
    settings.extend(deployment.server.start_otlp_backend());
    settings.extend(deployment.server.start_managed_grafana().await);
    settings.extend(deployment.server.front_managed_grafana());
    deployment.set(&pairs(&settings));
    deployment
}

#[tokio::test]
async fn obs_13_a_deploy_waits_for_a_managed_grafana_that_is_still_loading() {
    let deployment = behind_a_loading_grafana().await;
    let released = release_dashboards(&deployment);
    deployment.server.managed_grafana_loads();

    let deployed = deployment.apply(&deployment.api, &[]).await;

    assert!(deployed.status.success(), "{}", lossy(&deployed));
    assert!(
        lossy(&deployed).contains(&format!("imported {} dashboards", released.len())),
        "{}",
        lossy(&deployed)
    );
    // The import waited through a 503.
    assert_eq!(
        deployment.server.shell_ok("cat /root/front/loaded"),
        "answered 503"
    );
}

#[tokio::test]
async fn ops_19_the_check_waits_for_a_managed_grafana_that_is_still_loading() {
    let deployment = behind_a_loading_grafana().await;
    release_dashboards(&deployment);
    deployment.server.managed_grafana_loads();

    let checked =
        deployment
            .server
            .deploy(&["check-images", &deployment.api, &deployment.postgres]);

    assert!(
        lossy(&checked).contains("check: ok    the managed Grafana at "),
        "{}",
        lossy(&checked)
    );
    // The check waited through a 503.
    assert_eq!(
        deployment.server.shell_ok("cat /root/front/loaded"),
        "answered 503"
    );
}

#[tokio::test]
async fn obs_14_a_deploy_imports_the_rules_into_the_managed_grafana_and_warns_until_a_policy_reaches_a_contact_point()
 {
    let deployment = Deployment::managed(&[]);
    let mut settings = vec![("TELEMETRY".to_owned(), "managed".to_owned())];
    settings.extend(deployment.server.start_otlp_backend());
    deployment.set(&pairs(&settings));
    let released = release_rule_groups(&deployment);
    let without = deployment.apply(&deployment.api, &[]).await;
    settings.extend(deployment.server.start_managed_grafana().await);
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
    let deployment = Deployment::managed(&[("TELEMETRY", "self-hosted")]);
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
    // The API's server span is a child of the proxy's (OBS-06).
    assert!(both.contains("\"caddy\""), "{both}");
}

/// The addresses from which Caddy, on `deployment`'s server, may see a request of the tests: the
/// gateway of each network that its container joins, and the server's own gateway, through which
/// the port that the server publishes reaches it.
fn addresses_caddy_may_see(deployment: &Deployment) -> Vec<String> {
    deployment
        .server
        .shell_ok(
            "docker inspect --format '{{ range .NetworkSettings.Networks }}{{ .Gateway }} {{ end }}' \
             deployment-caddy-1 && ip route show default | awk '{ print $3 }'",
        )
        .split_whitespace()
        .map(str::to_owned)
        .collect()
}

/// Whether `text` holds the IPv4 address `address` whole: with neither a digit nor a dot before
/// it, and no digit after it.
fn holds_address(text: &str, address: &str) -> bool {
    text.match_indices(address).any(|(at, _)| {
        let before = text[..at].chars().next_back();
        let after = text[at + address.len()..].chars().next();
        !before.is_some_and(|c| c.is_ascii_digit() || c == '.')
            && !after.is_some_and(|c| c.is_ascii_digit())
    })
}

#[tokio::test]
async fn obs_04_the_proxy_s_span_and_logs_hold_no_personal_data() {
    let deployment = Deployment::with_a_managed_backend().await;
    let addresses = addresses_caddy_may_see(&deployment);

    let trace = trace_of_a_request_no_replica_answers(&deployment);
    let telemetry = received(&deployment, &trace);
    let written = deployment
        .server
        .shell_ok("docker exec deployment-caddy-1 cat /var/log/caddy/access.log");
    // Caddy's own log, which `just logs` shows.
    let own = deployment
        .server
        .shell_ok("docker logs deployment-caddy-1 2>&1");

    assert!(telemetry.contains("no replica answered"), "{telemetry}");
    assert!(written.contains(&trace), "{written}");
    assert!(!addresses.is_empty());
    for name in PERSONAL {
        assert!(!telemetry.contains(name), "{name}: {telemetry}");
        assert!(!written.contains(name), "{name}: {written}");
    }
    // The client's user agent and its address, under any name.
    for (what, text) in [
        ("the telemetry", &telemetry),
        ("the access log", &written),
        ("Caddy's own log", &own),
    ] {
        assert!(!text.contains(AGENT), "{what} holds the user agent: {text}");
        for address in &addresses {
            assert!(
                !holds_address(text, address),
                "{what} holds the address {address}: {text}"
            );
        }
    }
}

#[tokio::test]
async fn obs_04_the_proxy_s_own_line_of_a_response_that_a_replica_ends_short_holds_no_personal_data()
 {
    let deployment = Deployment::managed(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    let addresses = addresses_caddy_may_see(&deployment);
    let token = deployment.registered("ada@example.com");
    let (root, url) = (
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/users/me/events"),
    );
    // A user's stream, read through the proxy from a thread of the test, as a client reads one,
    // until it ends. A stream ends only when its replica ends it, so a replica that stops while
    // it is open ends the response short, whatever the sockets between them hold.
    let (opening, opened) = mpsc::channel();
    let reader = thread::spawn(move || {
        let response = streaming_client(&root, Duration::from_secs(60))
            .get(&url)
            .bearer_auth(token)
            .header("user-agent", AGENT)
            .send()
            .expect("the stream");
        let mut lines = BufReader::new(response).lines().map_while(Result::ok);
        let open = lines.by_ref().any(|line| line == "event: stream.opened");
        if let Err(unread) = opening.send(open) {
            platform::errors::ignore(&unread, "test_opening_unread");
        }
        lines.for_each(drop);
    });
    assert_eq!(opened.recv(), Ok(true), "the stream's opening");

    deployment.server.shell_ok(
        "for service in api-blue api-green; do \
         docker ps --quiet --filter label=com.docker.compose.service=$service; \
         done | xargs -r docker kill > /dev/null",
    );
    reader.join().expect("the stream's reader");
    let own = lossy(&deployment.server.shell(
        "timeout 60 sh -c 'until docker logs deployment-caddy-1 2>&1 \
         | grep -q \"aborting with incomplete response\"; do sleep 1; done'; \
         docker logs deployment-caddy-1 2>&1",
    ));

    let short: Vec<&str> = own
        .lines()
        .filter(|line| line.contains("aborting with incomplete response"))
        .collect();
    assert!(!short.is_empty(), "{own}");
    for line in &short {
        for name in PERSONAL {
            assert!(!line.contains(name), "{name}: {line}");
        }
    }
    assert!(
        !own.contains(AGENT),
        "Caddy's own log holds the user agent: {own}"
    );
    assert!(!addresses.is_empty());
    for address in &addresses {
        assert!(
            !holds_address(&own, address),
            "Caddy's own log holds the address {address}: {own}"
        );
    }
}

#[tokio::test]
async fn obs_15_the_proxy_traces_no_probe() {
    let deployment = Deployment::with_a_managed_backend().await;
    // A probe's trace context, which a traced probe would continue: while no replica runs, its
    // 503 would be a span that the collector keeps.
    let probe = "4bf92f3577b34da6a3ce929d0e0e4736";

    deployment
        .server
        .shell_ok("docker stop deployment-api-blue-1 deployment-api-blue-2 > /dev/null");
    let (status, _) = get_traced(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/health/ready"),
        format!("00-{probe}-00f067aa0ba902b7-01"),
    );
    let (_, _, body) = get_within(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
        Duration::from_secs(2),
    );
    let problem: serde_json::Value = serde_json::from_str(&body).expect("a problem");
    let trace = problem["trace_id"]
        .as_str()
        .expect("its trace ID")
        .to_owned();
    // Once the request's trace has arrived, the probe's would have too.
    received(&deployment, &trace);
    let everything = deployment
        .server
        .shell_ok("cat /root/backend/out/telemetry.json");

    assert_eq!(status, 503);
    assert!(!everything.contains(probe), "the probe's trace arrived");
}

/// The names of the services that sent the spans of the trace `trace` that `telemetry`, lines of
/// the stand-in backend's file, holds.
fn services_in_trace(telemetry: &str, trace: &str) -> BTreeSet<String> {
    let mut services = BTreeSet::new();
    for line in telemetry.lines() {
        let Ok(batch) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        for resource in batch["resourceSpans"].as_array().into_iter().flatten() {
            let holds = resource["scopeSpans"]
                .as_array()
                .into_iter()
                .flatten()
                .flat_map(|scope| scope["spans"].as_array().into_iter().flatten())
                .any(|span| span["traceId"] == trace);
            let name = resource["resource"]["attributes"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|attribute| attribute["key"] == "service.name")
                .and_then(|attribute| attribute["value"]["stringValue"].as_str());
            if let (true, Some(name)) = (holds, name) {
                services.insert(name.to_owned());
            }
        }
    }
    services
}

#[tokio::test]
async fn obs_06_the_trace_of_a_request_to_a_path_that_starts_as_a_probe_s_starts_at_the_proxy() {
    let deployment = Deployment::with_a_managed_backend().await;

    // A replica answers 404, a status the collector keeps the trace of.
    let (status, headers) = get(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/health/none"),
    );
    let timing = headers["server-timing"].to_str().unwrap().to_owned();
    let trace = timing.split('-').nth(1).expect("the trace ID").to_owned();
    // Until the proxy's span arrives, which a line of spans holds with its service's name.
    deployment.server.shell(&format!(
        "timeout 120 sh -c 'until grep resourceSpans /root/backend/out/telemetry.json 2> /dev/null | grep {trace} | grep -q caddy; do sleep 2; done'"
    ));
    let telemetry = received(&deployment, &trace);

    assert_eq!(status, 404);
    let services = services_in_trace(&telemetry, &trace);
    assert!(services.contains("caddy"), "{services:?}");
    assert!(services.contains(support::service_name()), "{services:?}");
}

#[tokio::test]
async fn obs_08_a_managed_backend_receives_the_proxy_s_span_and_log_line_of_its_503() {
    let deployment = Deployment::with_a_managed_backend().await;

    let trace = trace_of_a_request_no_replica_answers(&deployment);
    let telemetry = received(&deployment, &trace);

    assert!(telemetry.contains("no replica answered"), "{telemetry}");
}

/// The `service.version` of each resource of the service `caddy` in `line`, a line of the
/// stand-in backend's file, whether its spans' or its log records'; `None` for one that names no
/// version.
fn proxy_versions(line: &str) -> Vec<Option<String>> {
    let batch: serde_json::Value = serde_json::from_str(line).expect("a line of OTLP's JSON");
    let resources = ["resourceSpans", "resourceLogs"]
        .into_iter()
        .flat_map(|kind| batch[kind].as_array().into_iter().flatten());
    let mut versions = Vec::new();
    for resource in resources {
        let attribute = |key: &str| {
            resource["resource"]["attributes"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|attribute| attribute["key"] == key)
                .and_then(|attribute| attribute["value"]["stringValue"].as_str())
                .map(str::to_owned)
        };
        if attribute("service.name").as_deref() == Some("caddy") {
            versions.push(attribute("service.version"));
        }
    }
    versions
}

#[tokio::test]
async fn obs_02_each_span_and_log_line_of_the_proxy_names_its_service_s_version() {
    let deployment = Deployment::with_a_managed_backend().await;
    // The version of Caddy that its image names.
    let version = deployment
        .server
        .shell_ok("docker exec deployment-caddy-1 printenv CADDY_VERSION");

    let trace = trace_of_a_request_no_replica_answers(&deployment);
    let telemetry = received(&deployment, &trace);

    assert!(!version.is_empty());
    // `received` returns once a line of spans and one of log records hold the trace. Each
    // version is read up to its first space, since a version holds no space.
    for line in telemetry.lines() {
        let versions = proxy_versions(line);
        assert!(!versions.is_empty(), "{line}");
        for named in &versions {
            let first = named.as_deref().and_then(|named| named.split(' ').next());
            assert_eq!(first, Some(version.as_str()), "{line}");
        }
    }
}
