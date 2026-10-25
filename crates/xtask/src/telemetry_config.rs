//! `check-telemetry-config`: the collector's tail sampling keeps every trace OBS-08 says it
//! keeps, with a managed backend as with the self-hosted stack, and the latency objective is the
//! same in the sampling, the latency alerts' rules and the dashboard that lists the slow traces.

use serde_json::Value;

use crate::{
    Finding, Outcome,
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
/// for the traces pipeline, and defines no tail sampling of its own.
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
    if !overlay["service"]["pipelines"]["traces"]["processors"].is_null() {
        findings.push(Finding::new(
            stack::MANAGED,
            text::line_of(source, "traces:"),
            "the managed backend's configuration replaces the traces pipeline's processors, and \
             with them the tail sampling of the collector's own (OBS-08)",
        ));
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

/// Checks that every rule that selects the requests within the objective uses its bucket.
fn alerts_share(repo: &Repo, objective: f64, findings: &mut Vec<Finding>) {
    let mut measured = false;
    for rule in stack::rules(repo, findings) {
        for bound in bucket_bounds(&rule.expr) {
            measured = true;
            if bound.parse::<f64>().ok().map(|seconds| seconds * 1000.0) != Some(objective) {
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

/// The values of `le` that `expression` selects in the request metric's buckets.
fn bucket_bounds(expression: &str) -> Vec<String> {
    expression
        .match_indices(BUCKET)
        .filter_map(|(at, _)| {
            let rest = expression[at + BUCKET.len()..].strip_prefix('{')?;
            let matchers = &rest[..rest.find('}')?];
            let value = matchers.split("le=\"").nth(1)?;
            Some(value[..value.find('"')?].to_owned())
        })
        .collect()
}

/// Checks that the slow-trace list's queries keep the objective as their threshold, and that a
/// panel of the request durations draws its threshold at the objective.
fn dashboards_share(repo: &Repo, objective: f64, findings: &mut Vec<Finding>) {
    let dashboards = stack::dashboards(repo, findings);
    for query in &dashboards.queries {
        let durations = query.store == Store::Prometheus && query.query.contains(BUCKET);
        for threshold in query.thresholds.iter().filter(|_| durations) {
            if threshold * 1000.0 != objective {
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
    for (query, milliseconds) in lists {
        if milliseconds != Some(objective) {
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

/// The duration a TraceQL query keeps the traces above, in milliseconds: `Some(None)` when
/// the query compares the duration with a value it can't read.
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
    let milliseconds = if let Some(number) = value.strip_suffix("ms") {
        number.parse::<f64>().ok()
    } else if let Some(number) = value.strip_suffix('s') {
        number.parse::<f64>().ok().map(|seconds| seconds * 1000.0)
    } else {
        None
    };
    Some(milliseconds)
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
        assert!(check_with(replaces)[0].contains("replaces the traces pipeline's processors"));
        assert!(check_with(redefines)[0].contains("defines tail sampling of its own"));
    }

    #[test]
    fn obs_08_the_sampling_keeps_errors_slow_requests_and_a_share() {
        let kept = findings(
            &collector(POLICIES),
            RULES,
            &dashboard("{ duration > 500ms }"),
        );
        assert!(kept.is_empty(), "{kept:?}");

        let without_errors: String = POLICIES.lines().skip(1).collect::<Vec<_>>().join("\n");
        let missing = findings(
            &collector(&without_errors),
            RULES,
            &dashboard("{ duration > 0.5s }"),
        );
        assert_eq!(missing.len(), 1, "{missing:?}");
        assert!(missing[0].contains("error status"));
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
