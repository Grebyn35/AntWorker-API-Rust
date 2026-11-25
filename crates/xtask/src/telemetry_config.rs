//! `check-telemetry-config`: the collector's tail sampling keeps every trace OBS-08 says it
//! keeps, with a managed backend as with the self-hosted stack, and the latency objective is the
//! same in the sampling, in each bound of the requests' buckets that the rules select, as
//! [`bucket_bounds`] reads one, and in the dashboards, as [`dashboards_share`] reads them.

use serde_json::Value;

use crate::{
    Finding, Outcome, promql,
    repo::Repo,
    stack::{self, Store},
    text,
};

/// The latency rules select the requests within the objective by this bucket of the request
/// metric.
const BUCKET: &str = "http_server_request_duration_seconds_bucket";

/// Runs `check-telemetry-config`.
pub fn check(repo: &Repo) -> Outcome {
    let mut findings = Vec::new();
    overlay_keeps_sampling(repo, &mut findings);
    proxy_keeps_processors(repo, &mut findings);
    if let Some(objective) = sampling(repo, &mut findings) {
        alerts_share(repo, objective, &mut findings);
        dashboards_share(repo, objective, &mut findings);
    }
    Outcome {
        report: Vec::new(),
        findings,
    }
}

/// Checks that the managed backend's configuration, which the collector merges over its own,
/// keeps its tail sampling: the merge replaces a list it sets, so the overlay sets no processors
/// and no receivers for a traces pipeline, `traces` or one whose name starts with `traces/`, as
/// a pipeline of its own takes, which could carry the API's spans past the sampling; and it
/// defines no tail sampling of its own.
fn overlay_keeps_sampling(repo: &Repo, findings: &mut Vec<Finding>) {
    if repo.file(stack::MANAGED).is_none() {
        return;
    }
    let Some(overlay) = stack::yaml(repo, stack::MANAGED, findings) else {
        return;
    };
    let source = repo
        .file(stack::MANAGED)
        .map_or("", |file| file.text.as_str());
    for (name, set) in traces_pipelines(&overlay) {
        if set.contains(&true) {
            findings.push(Finding::new(
                stack::MANAGED,
                text::line_of(source, &format!("{name}:")),
                format!(
                    "the managed backend's configuration sets the {} of the pipeline {name}, \
                     which replaces the processors of the collector's own, the tail sampling \
                     among them, or takes spans that the sampling doesn't read (OBS-08)",
                    if set[0] { "processors" } else { "receivers" }
                ),
            ));
        }
    }
    let redefines = overlay["processors"]
        .as_object()
        .into_iter()
        .flat_map(|processors| processors.keys())
        .any(|name| name == "tail_sampling" || name.starts_with("tail_sampling/"));
    if redefines {
        findings.push(Finding::new(
            stack::MANAGED,
            text::line_of(source, "tail_sampling"),
            "the managed backend's configuration defines tail sampling of its own; the \
             collector's own applies to every backend (OBS-08)",
        ));
    }
}

/// Each traces pipeline that `overlay` names, `traces` or one whose name starts with `traces/`,
/// with whether it sets the pipeline's processors and its receivers.
fn traces_pipelines(overlay: &Value) -> Vec<(&str, [bool; 2])> {
    overlay["service"]["pipelines"]
        .as_object()
        .into_iter()
        .flatten()
        .filter(|(name, _)| name.split('/').next() == Some("traces"))
        .map(|(name, pipeline)| {
            let set = ["processors", "receivers"].map(|key| !pipeline[key].is_null());
            (name.as_str(), set)
        })
        .collect()
}

/// Checks that the proxy's configuration, which every collector of a deployment merges over its
/// own, keeps the collector's processors: the merge replaces a list it sets, so the list of
/// processors that it sets for the pipeline `traces`, `logs` or `metrics` holds the collector's
/// own, in their order, tail sampling among them (OBS-08). It sets no receivers for a traces
/// pipeline whose name starts with `traces/`, as a pipeline of its own takes, which could carry
/// the API's spans past the sampling, and it defines no tail sampling of its own.
fn proxy_keeps_processors(repo: &Repo, findings: &mut Vec<Finding>) {
    if repo.file(stack::PROXY).is_none() {
        return;
    }
    let (Some(config), Some(proxy)) = (
        stack::yaml(repo, stack::COLLECTOR, findings),
        stack::yaml(repo, stack::PROXY, findings),
    ) else {
        return;
    };
    let source = repo
        .file(stack::PROXY)
        .map_or("", |file| file.text.as_str());
    let names = |list: &Value| -> Vec<String> {
        list.as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    };
    for pipeline in ["traces", "logs", "metrics"] {
        let set = &proxy["service"]["pipelines"][pipeline]["processors"];
        if set.is_null() {
            continue;
        }
        let own = names(&config["service"]["pipelines"][pipeline]["processors"]);
        let kept: Vec<String> = names(set)
            .into_iter()
            .filter(|name| own.contains(name))
            .collect();
        if kept != own {
            findings.push(Finding::new(
                stack::PROXY,
                text::line_of(source, &format!("{pipeline}:")),
                format!(
                    "the proxy's configuration replaces the {pipeline} pipeline's processors \
                     without the collector's own, {}, in their order (OBS-08)",
                    own.join(", ")
                ),
            ));
        }
    }
    for (name, [_, receivers]) in traces_pipelines(&proxy) {
        if name != "traces" && receivers {
            findings.push(Finding::new(
                stack::PROXY,
                text::line_of(source, &format!("{name}:")),
                format!(
                    "the proxy's configuration sets the receivers of the pipeline {name}, which \
                     takes spans that the collector's tail sampling doesn't read (OBS-08)"
                ),
            ));
        }
    }
    let redefines = proxy["processors"]
        .as_object()
        .into_iter()
        .flat_map(|processors| processors.keys())
        .any(|name| name == "tail_sampling" || name.starts_with("tail_sampling/"));
    if redefines {
        findings.push(Finding::new(
            stack::PROXY,
            text::line_of(source, "tail_sampling"),
            "the proxy's configuration defines tail sampling of its own; the collector's own \
             applies to every trace (OBS-08)",
        ));
    }
}

/// The latency objective, in milliseconds, which the collector's tail sampling keeps every trace
/// above, as `just load-test` measures against it; or why it can't be read.
pub fn latency_objective(repo: &Repo) -> Result<f64, String> {
    let mut findings = Vec::new();
    sampling(repo, &mut findings).ok_or_else(|| {
        findings
            .into_iter()
            .map(|finding| finding.message)
            .collect::<Vec<_>>()
            .join("; ")
    })
}

/// Checks the tail sampling of the traces pipeline, and returns the latency objective it keeps
/// traces above, in milliseconds.
fn sampling(repo: &Repo, findings: &mut Vec<Finding>) -> Option<f64> {
    let config = stack::yaml(repo, stack::COLLECTOR, findings)?;
    let source = repo
        .file(stack::COLLECTOR)
        .map_or("", |file| file.text.as_str());
    let mut fail = |needle: &str, message: &str| {
        findings.push(Finding::new(
            stack::COLLECTOR,
            text::line_of(source, needle),
            format!("{message} (OBS-08)"),
        ));
    };
    let Some(name) = config["service"]["pipelines"]["traces"]["processors"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find(|name| *name == "tail_sampling" || name.starts_with("tail_sampling/"))
    else {
        fail(
            "traces:",
            "the traces pipeline has no tail_sampling processor",
        );
        return None;
    };
    let policies: Vec<&Value> = config["processors"][name]["policies"]
        .as_array()
        .into_iter()
        .flatten()
        .collect();
    for policy in &policies {
        if policy["type"] == "drop" || policy_inverts(policy) {
            fail(
                policy["name"].as_str().unwrap_or("policies:"),
                "a policy that drops or inverts could drop a trace the other policies keep",
            );
        }
    }
    for message in missing(&policies) {
        fail("policies:", message);
    }
    let latency = of_type(&policies, "latency");
    match latency.as_slice() {
        [policy] if policy["latency"]["upper_threshold_ms"].is_null() => {
            let objective = policy["latency"]["threshold_ms"].as_f64();
            if objective.is_none() {
                fail("latency:", "the latency policy has no threshold_ms");
            }
            objective
        }
        _ => {
            fail(
                "policies:",
                "one latency policy, with no upper threshold, keeps every trace slower than the latency objective",
            );
            None
        }
    }
}

/// What the policies that keep errors, responses of 400 and above, and a share of the rest
/// lack, if any of them is missing.
fn missing(policies: &[&Value]) -> Vec<&'static str> {
    let mut missing = Vec::new();
    if !of_type(policies, "status_code").iter().any(|policy| {
        policy["status_code"]["status_codes"]
            .as_array()
            .is_some_and(|codes| codes.iter().any(|code| code == "ERROR"))
    }) {
        missing.push("no policy keeps every trace with an error status");
    }
    if !of_type(policies, "numeric_attribute").iter().any(|policy| {
        let rule = &policy["numeric_attribute"];
        rule["key"] == "http.response.status_code"
            && rule["min_value"].as_f64() == Some(400.0)
            && rule["max_value"].as_f64().is_none_or(|max| max >= 599.0)
    }) {
        missing.push("no policy keeps every trace with http.response.status_code >= 400");
    }
    if !of_type(policies, "probabilistic").iter().any(|policy| {
        policy["probabilistic"]["sampling_percentage"]
            .as_f64()
            .is_some_and(|p| p > 0.0)
    }) {
        missing.push("no policy keeps a share of the other traces");
    }
    missing
}

/// The policies among `policies` of the type `kind`.
fn of_type<'a>(policies: &[&'a Value], kind: &'static str) -> Vec<&'a Value> {
    policies
        .iter()
        .copied()
        .filter(|policy| policy["type"] == kind)
        .collect()
}

fn policy_inverts(policy: &Value) -> bool {
    policy
        .as_object()
        .into_iter()
        .flat_map(|fields| fields.values())
        .any(|settings| settings["invert_match"] == true)
}

/// Checks that each bound that [`bucket_bounds`] reads in a rule's expression is the objective,
/// and that one rule's expression holds one; fails on each part of a rule's expression that
/// [`bucket_bounds`] can't read.
fn alerts_share(repo: &Repo, objective: f64, findings: &mut Vec<Finding>) {
    let mut measured = false;
    for rule in stack::rules(repo, findings) {
        for bound in bucket_bounds(&rule.expr) {
            let bound = match bound {
                Ok(bound) => bound,
                Err(unread) => {
                    findings.push(Finding::new(stack::RULES, rule.line, unread));
                    continue;
                }
            };
            measured = true;
            if !bound
                .parse::<f64>()
                .is_ok_and(|seconds| is_objective(seconds, objective))
            {
                findings.push(Finding::new(
                    stack::RULES,
                    rule.line,
                    format!(
                        "the rule selects the bucket le=\"{bound}\", but the latency objective is {objective} ms (OBS-08)"
                    ),
                ));
            }
        }
    }
    if !measured {
        findings.push(Finding::new(
            stack::RULES,
            1,
            format!("no rule selects the requests within the latency objective through {BUCKET} (OBS-08)"),
        ));
    }
}

/// The bounds of the request metric's buckets that `expression`, a rule's, selects, with an
/// `Err` for each part that the check can't read. It reads each selector, as
/// [`promql::selectors`] reads one, by the name before its braces, and can't read one without a
/// name. Of a selector named [`BUCKET`], it reads each matcher as a label's name, an identifier
/// or a string, then `=`, `!=`, `=~` or `!~`, then a string, spaces aside, each string in double
/// quotes, single quotes or backticks and without a backslash: a matcher of the label `le` gives
/// its string as a bound when its operator is `=`, and the check can't read another matcher of
/// `le`, nor a matcher that it doesn't read so.
fn bucket_bounds(expression: &str) -> Vec<Result<String, String>> {
    let mut bounds = Vec::new();
    for selector in promql::selectors(expression) {
        let Some(name) = selector.name else {
            bounds.push(Err(format!(
                "the rule's selector {{{}}} has no metric's name before its braces, by which \
                 check-telemetry-config reads what it selects (OBS-08)",
                selector.matchers.join(", ")
            )));
            continue;
        };
        if name != BUCKET {
            continue;
        }
        for text in &selector.matchers {
            match matcher(text) {
                Some((label, _, _)) if label != "le" => {}
                Some((_, "=", bound)) => bounds.push(Ok(bound)),
                _ => bounds.push(Err(format!(
                    "the rule selects {BUCKET} with the matcher {text}, which \
                     check-telemetry-config reads only as a label's name, `=`, `!=`, `=~` or \
                     `!~` and a string without a backslash, and of the label `le` only with `=` \
                     (OBS-08)"
                ))),
            }
        }
    }
    bounds
}

/// The label's name, the operator and the string of `text`, a matcher, as [`bucket_bounds`]
/// reads one.
fn matcher(text: &str) -> Option<(String, &'static str, String)> {
    let (label, rest) = match string(text) {
        Some(quoted) => quoted,
        None => {
            let end = text
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(text.len());
            let label = &text[..end];
            if !label.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
                return None;
            }
            (label.to_owned(), &text[end..])
        }
    };
    let rest = rest.trim_start();
    let operator = ["=~", "!~", "!=", "="]
        .into_iter()
        .find(|operator| rest.starts_with(operator))?;
    let (value, rest) = string(rest[operator.len()..].trim_start())?;
    rest.trim().is_empty().then_some((label, operator, value))
}

/// The text of the string that `text` starts with, in double quotes, single quotes or
/// backticks, if it holds no backslash, and what follows it.
fn string(text: &str) -> Option<(String, &str)> {
    let quote = text
        .chars()
        .next()
        .filter(|c| matches!(c, '"' | '\'' | '`'))?;
    let inner = &text[quote.len_utf8()..];
    let end = inner.find(quote)?;
    let value = &inner[..end];
    (!value.contains('\\')).then(|| (value.to_owned(), &inner[end + quote.len_utf8()..]))
}

/// Checks that the slow-trace list's queries keep the objective as their threshold, and that a
/// panel of the request durations draws its threshold at the objective.
fn dashboards_share(repo: &Repo, objective: f64, findings: &mut Vec<Finding>) {
    let dashboards = stack::dashboards(repo, findings);
    for query in &dashboards.queries {
        let durations = query.store == Store::Prometheus && query.query.contains(BUCKET);
        for threshold in query.thresholds.iter().filter(|_| durations) {
            if !is_objective(*threshold, objective) {
                findings.push(Finding::new(
                    &query.path,
                    query.line,
                    format!(
                        "the panel `{}` draws a threshold of {threshold} s, but the latency objective is {objective} ms (OBS-08)",
                        query.title
                    ),
                ));
            }
        }
    }
    let lists: Vec<_> = dashboards
        .queries
        .iter()
        .filter(|query| query.store == Store::Tempo)
        .filter_map(|query| Some((query, threshold(&query.query)?)))
        .collect();
    if lists.is_empty() {
        findings.push(Finding::new(
            stack::DASHBOARDS,
            1,
            "no dashboard lists the traces slower than the latency objective (OBS-08)",
        ));
    }
    for (query, seconds) in lists {
        if !seconds.is_some_and(|seconds| is_objective(seconds, objective)) {
            findings.push(Finding::new(
                &query.path,
                query.line,
                format!(
                    "the panel `{}` lists traces by a duration other than the latency objective of {objective} ms (OBS-08)",
                    query.title
                ),
            ));
        }
    }
}

/// Whether `seconds` is the latency objective of `milliseconds`, each a 64-bit float of seconds:
/// the product by 1,000 of a bound such as 1.001 s isn't 1,001.
fn is_objective(seconds: f64, milliseconds: f64) -> bool {
    seconds == milliseconds / 1000.0
}

/// The duration a TraceQL query keeps the traces above, in seconds: `Some(None)` when the query
/// compares the duration with a value it can't read.
fn threshold(query: &str) -> Option<Option<f64>> {
    let rest = query
        .split("duration")
        .nth(1)?
        .trim_start()
        .strip_prefix('>')?;
    let value: String = rest
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '.')
        .collect();
    let seconds = if let Some(number) = value.strip_suffix("ms") {
        number
            .parse::<f64>()
            .ok()
            .map(|milliseconds| milliseconds / 1000.0)
    } else if let Some(number) = value.strip_suffix('s') {
        number.parse::<f64>().ok()
    } else {
        None
    };
    Some(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: &str = "groups:\n  - name: slo\n    rules:\n      - record: job:slow:ratio\n        expr: 1 - sum(rate(http_server_request_duration_seconds_bucket{le=\"0.5\"}[5m])) / sum(rate(x[5m]))\n";

    fn collector(policies: &str) -> String {
        format!(
            "processors:\n  tail_sampling:\n    policies:\n{policies}\nservice:\n  pipelines:\n    traces:\n      processors: [tail_sampling]\n"
        )
    }

    const POLICIES: &str = "      - {name: errors, type: status_code, status_code: {status_codes: [ERROR]}}
      - {name: four, type: numeric_attribute, numeric_attribute: {key: http.response.status_code, min_value: 400}}
      - {name: slow, type: latency, latency: {threshold_ms: 500}}
      - {name: share, type: probabilistic, probabilistic: {sampling_percentage: 10}}";

    fn dashboard(query: &str) -> String {
        serde_json::json!({"panels": [{"type": "table", "title": "Which?",
            "datasource": {"type": "tempo"}, "targets": [{"query": query}]}]})
        .to_string()
    }

    fn findings(collector: &str, rules: &str, dashboard: &str) -> Vec<String> {
        let repo = Repo::of(&[
            (stack::COLLECTOR, collector),
            (stack::RULES, rules),
            ("deploy/observability/grafana/dashboards/a.json", dashboard),
        ]);
        check(&repo)
            .findings
            .into_iter()
            .map(|f| f.message)
            .collect()
    }

    #[test]
    fn obs_08_the_managed_backend_s_configuration_keeps_the_collector_s_sampling() {
        let keeps = "service:\n  pipelines:\n    traces:\n      exporters: [otlp_http/managed]\n";
        let replaces = "service:\n  pipelines:\n    traces:\n      processors: [batch]\n";
        let adds = "service:\n  pipelines:\n    traces:\n      exporters: [otlp_http/managed]\n    \
                    traces/x:\n      receivers: [otlp]\n      exporters: [otlp_http/managed]\n";
        let redefines = "processors:\n  tail_sampling:\n    policies: []\n";
        let check_with = |overlay: &str| {
            let repo = Repo::of(&[
                (stack::COLLECTOR, collector(POLICIES).as_str()),
                (stack::MANAGED, overlay),
                (stack::RULES, RULES),
                (
                    "deploy/observability/grafana/dashboards/a.json",
                    dashboard("{ duration > 500ms }").as_str(),
                ),
            ]);
            check(&repo)
                .findings
                .into_iter()
                .map(|f| f.message)
                .collect::<Vec<_>>()
        };

        assert!(check_with(keeps).is_empty());
        assert!(check_with(replaces)[0].contains("sets the processors of the pipeline traces,"));
        assert!(check_with(adds)[0].contains("sets the receivers of the pipeline traces/x,"));
        assert!(check_with(redefines)[0].contains("defines tail sampling of its own"));
    }

    #[test]
    fn obs_08_the_proxy_s_configuration_keeps_the_collector_s_processors() {
        let keeps = "service:\n  pipelines:\n    traces:\n      processors: [transform/proxy, tail_sampling]\n";
        let drops =
            "service:\n  pipelines:\n    traces:\n      processors: [transform/proxy, batch]\n";
        let redefines = "processors:\n  tail_sampling/proxy:\n    policies: []\n";
        let check_with = |proxy: &str| {
            let repo = Repo::of(&[
                (stack::COLLECTOR, collector(POLICIES).as_str()),
                (stack::PROXY, proxy),
                (stack::RULES, RULES),
                (
                    "deploy/observability/grafana/dashboards/a.json",
                    dashboard("{ duration > 500ms }").as_str(),
                ),
            ]);
            check(&repo)
                .findings
                .into_iter()
                .map(|f| f.message)
                .collect::<Vec<_>>()
        };

        assert!(check_with(keeps).is_empty(), "{:?}", check_with(keeps));
        assert!(check_with(drops)[0].contains("without the collector's own, tail_sampling"));
        assert!(check_with(redefines)[0].contains("defines tail sampling of its own"));
    }

    #[test]
    fn obs_08_the_proxy_s_configuration_adds_no_traces_pipeline_that_takes_spans() {
        // The proxy's own receivers of a pipeline of logs, as deploy/observability/collector/
        // proxy.yaml sets them, beside a traces pipeline that keeps the sampling.
        let keeps = "service:\n  pipelines:\n    traces:\n      processors: [tail_sampling]\n    \
                     logs:\n      receivers: [otlp, file_log/proxy]\n";
        let adds = "service:\n  pipelines:\n    traces:\n      processors: [tail_sampling]\n    \
                    traces/x:\n      receivers: [otlp]\n      exporters: [otlp_http/tempo]\n";
        let check_with = |proxy: &str| {
            let repo = Repo::of(&[
                (stack::COLLECTOR, collector(POLICIES).as_str()),
                (stack::PROXY, proxy),
                (stack::RULES, RULES),
                (
                    "deploy/observability/grafana/dashboards/a.json",
                    dashboard("{ duration > 500ms }").as_str(),
                ),
            ]);
            check(&repo)
                .findings
                .into_iter()
                .map(|f| f.message)
                .collect::<Vec<_>>()
        };

        assert!(check_with(keeps).is_empty(), "{:?}", check_with(keeps));
        let added = check_with(adds);
        assert!(
            added.len() == 1 && added[0].contains("sets the receivers of the pipeline traces/x,"),
            "{added:?}"
        );
    }

    #[test]
    fn obs_08_the_sampling_keeps_errors_slow_requests_and_a_share() {
        let kept = findings(
            &collector(POLICIES),
            RULES,
            &dashboard("{ duration > 500ms }"),
        );
        assert!(kept.is_empty(), "{kept:?}");

        // Each policy but the latency one, without which a finding names what it kept.
        for (line, kept) in [
            (0, "error status"),
            (1, "http.response.status_code >= 400"),
            (3, "a share of the other traces"),
        ] {
            let without: Vec<&str> = POLICIES
                .lines()
                .enumerate()
                .filter(|(index, _)| *index != line)
                .map(|(_, policy)| policy)
                .collect();
            let missing = findings(
                &collector(&without.join("\n")),
                RULES,
                &dashboard("{ duration > 0.5s }"),
            );
            assert_eq!(missing.len(), 1, "{missing:?}");
            assert!(missing[0].contains(kept), "{kept} in {missing:?}");
        }
    }

    #[test]
    fn obs_08_a_latency_policy_with_an_upper_threshold_fails() {
        let bounded = POLICIES.replace(
            "{threshold_ms: 500}",
            "{threshold_ms: 500, upper_threshold_ms: 10000}",
        );

        let found = findings(
            &collector(&bounded),
            RULES,
            &dashboard("{ duration > 500ms }"),
        );

        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("with no upper threshold"), "{found:?}");
    }

    #[test]
    fn obs_08_a_dashboard_lists_the_traces_slower_than_the_objective() {
        let found = findings(
            &collector(POLICIES),
            RULES,
            &dashboard("{ status = error }"),
        );

        assert_eq!(
            found,
            ["no dashboard lists the traces slower than the latency objective (OBS-08)"]
        );
    }

    #[test]
    fn obs_08_a_policy_that_drops_fails() {
        let dropping = format!("{POLICIES}\n      - {{name: no, type: drop, drop: {{}}}}");
        let found = findings(
            &collector(&dropping),
            RULES,
            &dashboard("{ duration > 500ms }"),
        );
        assert_eq!(found.len(), 1, "{found:?}");
    }

    #[test]
    fn obs_08_a_duration_panel_draws_the_objective() {
        let panel = |threshold: f64| {
            serde_json::json!({"panels": [
                {"type": "table", "title": "Which?", "datasource": {"type": "tempo"},
                 "targets": [{"query": "{ duration > 500ms }"}]},
                {"type": "timeseries", "title": "How long?", "datasource": {"type": "prometheus"},
                 "fieldConfig": {"defaults": {"thresholds": {"steps": [
                     {"color": "green", "value": null}, {"color": "red", "value": threshold}]}}},
                 "targets": [{"expr": "histogram_quantile(0.99, sum by (le) (rate(http_server_request_duration_seconds_bucket[5m])))"}]}
            ]})
            .to_string()
        };
        assert!(findings(&collector(POLICIES), RULES, &panel(0.5)).is_empty());
        assert_eq!(findings(&collector(POLICIES), RULES, &panel(1.0)).len(), 1);
    }

    #[test]
    fn obs_08_a_bound_or_a_threshold_in_seconds_matches_the_objective_of_its_milliseconds() {
        // 1.001 s, whose product by 1,000 as 64-bit floats isn't 1,001, in each of the four
        // places: the policy, a rule's bucket, the list of slow traces and a panel's line.
        let policies = POLICIES.replace("{threshold_ms: 500}", "{threshold_ms: 1001}");
        let rules = RULES.replace("le=\"0.5\"", "le=\"1.001\"");
        let dashboard = serde_json::json!({"panels": [
            {"type": "table", "title": "Which?", "datasource": {"type": "tempo"},
             "targets": [{"query": "{ duration > 1.001s }"}]},
            {"type": "timeseries", "title": "How long?", "datasource": {"type": "prometheus"},
             "fieldConfig": {"defaults": {"thresholds": {"steps": [
                 {"color": "green", "value": null}, {"color": "red", "value": 1.001}]}}},
             "targets": [{"expr": "histogram_quantile(0.99, sum by (le) (rate(http_server_request_duration_seconds_bucket[5m])))"}]}
        ]})
        .to_string();

        let found = findings(&collector(&policies), &rules, &dashboard);

        assert!(found.is_empty(), "{found:?}");
    }

    #[test]
    fn ops_16_the_load_test_measures_against_the_latency_policy_s_threshold() {
        let repo = Repo::of(&[(stack::COLLECTOR, &collector(POLICIES))]);
        let without = Repo::of(&[(stack::COLLECTOR, &collector(""))]);

        assert_eq!(latency_objective(&repo), Ok(500.0));
        assert!(latency_objective(&without).is_err());
    }

    /// The findings of the check where the rules hold [`RULES`]'s, and a second, whose expression
    /// is `expression`.
    fn beside_a_rule(expression: &str) -> Vec<String> {
        let rules = format!(
            "{RULES}      - record: job:other:ratio\n        expr: |\n          {expression}\n"
        );
        findings(
            &collector(POLICIES),
            &rules,
            &dashboard("{ duration > 500ms }"),
        )
    }

    #[test]
    fn obs_08_a_rule_s_bucket_is_read_by_its_le_matcher() {
        // Another label whose name ends in `le`, spaces, the other two quotes, a quoted label name,
        // a `}` and a `,` in a string, and spaces before the braces.
        let expressions = [
            r#"sum(rate(http_server_request_duration_seconds_bucket{handle="0.5", le="1"}[5m]))"#,
            r#"sum(rate(http_server_request_duration_seconds_bucket{le = "1"}[5m]))"#,
            r#"sum(rate(http_server_request_duration_seconds_bucket{le='1'}[5m]))"#,
            r#"sum(rate(http_server_request_duration_seconds_bucket{le=`1`}[5m]))"#,
            r#"sum(rate(http_server_request_duration_seconds_bucket{"le"="1"}[5m]))"#,
            r#"sum(rate(http_server_request_duration_seconds_bucket{route="},", le="1"}[5m]))"#,
            r#"sum(rate(http_server_request_duration_seconds_bucket {le="1"}[5m]))"#,
        ];

        let found = expressions.map(beside_a_rule);

        let wrong =
            "the rule selects the bucket le=\"1\", but the latency objective is 500 ms (OBS-08)";
        let missed: Vec<(&str, &Vec<String>)> = expressions
            .iter()
            .copied()
            .zip(&found)
            .filter(|(_, found)| *found != &[wrong])
            .collect();
        assert!(missed.is_empty(), "{missed:#?}");
    }

    #[test]
    fn obs_08_a_bucket_s_matcher_that_the_check_can_t_read_as_le_equal_to_a_bound_fails() {
        // Another operator, a value the check would read otherwise than Prometheus, a matcher
        // without a value, and the bucket's name inside the braces.
        let expressions = [
            r#"sum(rate(http_server_request_duration_seconds_bucket{le=~"0.5"}[5m]))"#,
            r#"sum(rate(http_server_request_duration_seconds_bucket{le!="0.5"}[5m]))"#,
            r#"sum(rate(http_server_request_duration_seconds_bucket{le="0\x2e5"}[5m]))"#,
            r#"sum(rate(http_server_request_duration_seconds_bucket{le}[5m]))"#,
            r#"sum(rate({__name__="http_server_request_duration_seconds_bucket", le="1"}[5m]))"#,
            r#"sum(rate({"http_server_request_duration_seconds_bucket", le="1"}[5m]))"#,
        ];

        let found = expressions.map(beside_a_rule);

        let missed: Vec<(&str, &Vec<String>)> = expressions
            .iter()
            .copied()
            .zip(&found)
            .filter(|(_, found)| {
                !(found.len() == 1 && found[0].contains("check-telemetry-config reads"))
            })
            .collect();
        assert!(missed.is_empty(), "{missed:#?}");
    }

    #[test]
    fn obs_08_the_objective_is_the_same_everywhere() {
        let rules = RULES.replace("le=\"0.5\"", "le=\"1\"");
        let found = findings(
            &collector(POLICIES),
            &rules,
            &dashboard("{ duration > 750ms }"),
        );
        assert_eq!(found.len(), 2, "{found:?}");
    }
}
