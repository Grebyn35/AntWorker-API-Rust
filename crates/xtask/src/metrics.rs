//! `check-metrics`: every metric is declared and named as OBS-11 says, the catalog is the one
//! the declarations give (OBS-12), every dashboard and alert reads only metrics that are
//! declared or listed, every metric is read by one, and every panel's title is a question
//! (OBS-13).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::{
    Finding, Outcome,
    declarations::{self, Metric},
    instrument, promql,
    repo::Repo,
    stack::{self, Store},
};

/// The catalog that `just metrics` writes.
pub const CATALOG: &str = "docs/operations/metrics.md";

/// The metrics other parts of the stack produce, which dashboards and alerts read.
const EXTERNAL: &str = "deploy/observability/metrics.yaml";

/// A metric that another part of the stack produces, by its name in Prometheus.
#[derive(Debug)]
struct External {
    name: String,
    kind: String,
    producer: String,
    description: String,
}

/// Runs `check-metrics`.
pub fn check(repo: &Repo) -> Outcome {
    let mut findings = Vec::new();
    let Some(catalog) = catalog(repo, &mut findings) else {
        return Outcome {
            report: Vec::new(),
            findings,
        };
    };
    match repo.file(CATALOG) {
        Some(file) if file.text == catalog.text => {}
        _ => findings.push(Finding::new(
            CATALOG,
            1,
            "the catalog differs from the declarations; run `just metrics` (OBS-12)",
        )),
    }
    check_use(repo, &catalog, &mut findings);
    Outcome {
        report: Vec::new(),
        findings,
    }
}

/// The catalog's text, for `just metrics` to write, or the findings that keep it from being
/// written.
pub fn write(repo: &Repo) -> Result<String, Vec<Finding>> {
    let mut findings = Vec::new();
    match catalog(repo, &mut findings) {
        Some(catalog) if findings.is_empty() => Ok(catalog.text),
        _ => Err(findings),
    }
}

/// The metrics and the catalog's text.
struct Catalog {
    declared: Vec<Metric>,
    external: Vec<External>,
    text: String,
}

fn catalog(repo: &Repo, findings: &mut Vec<Finding>) -> Option<Catalog> {
    let names = match instrument::names(repo) {
        Ok(names) => names,
        Err(finding) => {
            findings.push(finding);
            return None;
        }
    };
    let declared = declarations::read(repo, &names, findings);
    let external = external(repo, findings);
    let text = render(&declared, &external, findings);
    Some(Catalog {
        declared,
        external,
        text,
    })
}

/// The metrics [`EXTERNAL`] lists.
fn external(repo: &Repo, findings: &mut Vec<Finding>) -> Vec<External> {
    let Some(document) = stack::yaml(repo, EXTERNAL, findings) else {
        return Vec::new();
    };
    let field = |metric: &Value, key: &str| metric[key].as_str().unwrap_or_default().to_owned();
    let mut listed = Vec::new();
    for metric in document["metrics"].as_array().into_iter().flatten() {
        let external = External {
            name: field(metric, "name"),
            kind: field(metric, "kind"),
            producer: field(metric, "producer"),
            description: field(metric, "description"),
        };
        if [
            &external.name,
            &external.kind,
            &external.producer,
            &external.description,
        ]
        .iter()
        .any(|value| value.is_empty())
        {
            findings.push(Finding::new(
                EXTERNAL,
                crate::text::line_of(
                    repo.file(EXTERNAL).map_or("", |f| f.text.as_str()),
                    &external.name,
                ),
                "each metric has a name, a kind, a producer and a description",
            ));
        }
        listed.push(external);
    }
    listed
}

/// The catalog: every declared metric, then every listed one.
fn render(declared: &[Metric], external: &[External], findings: &mut Vec<Finding>) -> String {
    let mut text = String::from(
        "# Metric catalog\n\n\
         Every metric the dashboards and alerts read: the API's own, from their declarations in \
         code (OBS-11), and those that other parts of the stack produce, from \
         `deploy/observability/metrics.yaml`. `just metrics` writes this file, and \
         `check-metrics` fails while it differs from what the declarations give (OBS-12).\n\n\
         Prometheus stores each metric under the name in its \"In Prometheus\" column: its \
         receiver of the OpenTelemetry Protocol (OTLP) turns the dots into underscores, and adds \
         the unit and the kind as suffixes. A label's dots become underscores too.\n\n\
         ## The API's metrics\n\n\
         | Metric | Kind | Unit | Labels | In Prometheus | Declared in | Description |\n\
         |---|---|---|---|---|---|---|\n",
    );
    let mut sorted: Vec<&Metric> = declared.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    for metric in sorted {
        let series = metric.series().unwrap_or_else(|message| {
            findings.push(Finding::new(&metric.path, metric.line, message));
            Vec::new()
        });
        let labels = if metric.labels.is_empty() {
            "none".to_owned()
        } else {
            code_spans(&metric.labels)
        };
        let unit = if metric.unit.is_empty() {
            "none".to_owned()
        } else {
            format!("`{}`", metric.unit)
        };
        text.push_str(&format!(
            "| `{}` | {} | {} | {} | {} | `{}` | {} |\n",
            metric.name,
            metric.kind.name(),
            unit,
            labels,
            code_spans(&series),
            metric.path,
            metric.description,
        ));
    }
    text.push_str(
        "\n## Metrics of the rest of the stack\n\n\
         | In Prometheus | Kind | Produced by | Description |\n\
         |---|---|---|---|\n",
    );
    for metric in external {
        text.push_str(&format!(
            "| `{}` | {} | {} | {} |\n",
            metric.name, metric.kind, metric.producer, metric.description
        ));
    }
    text
}

fn code_spans(values: &[String]) -> String {
    values
        .iter()
        .map(|value| format!("`{value}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Fails a query of an unknown metric, a metric or recording rule that nothing reads, and a
/// panel title that isn't a question.
fn check_use(repo: &Repo, catalog: &Catalog, findings: &mut Vec<Finding>) {
    let dashboards = stack::dashboards(repo, findings);
    let rules = stack::rules(repo, findings);
    for title in &dashboards.titles {
        if !title.title.trim_end().ends_with('?') {
            findings.push(Finding::new(
                &title.path,
                title.line,
                format!(
                    "the panel title `{}` isn't the question it answers (OBS-13)",
                    title.title
                ),
            ));
        }
    }
    // Each name Prometheus holds, with the metric it belongs to.
    let mut owners: BTreeMap<String, String> = BTreeMap::new();
    for metric in &catalog.declared {
        for series in metric.series().unwrap_or_default() {
            owners.insert(series, metric.name.clone());
        }
    }
    for metric in &catalog.external {
        owners.insert(metric.name.clone(), metric.name.clone());
    }
    let recorded: BTreeMap<&str, &stack::Rule> = rules
        .iter()
        .filter_map(|rule| Some((rule.record.as_deref()?, rule)))
        .collect();
    // The queries that something reads directly: the dashboards' and the alerts'.
    let mut reads: Vec<(String, usize, String)> = dashboards
        .queries
        .iter()
        .filter(|query| query.store == Store::Prometheus)
        .map(|query| (query.path.clone(), query.line, query.query.clone()))
        .collect();
    reads.extend(
        rules
            .iter()
            .filter(|rule| rule.alert.is_some())
            .map(|rule| (stack::RULES.to_owned(), rule.line, rule.expr.clone())),
    );
    let mut used_metrics: BTreeSet<String> = BTreeSet::new();
    let mut used_records: BTreeSet<String> = BTreeSet::new();
    while let Some((path, line, query)) = reads.pop() {
        for name in promql::metric_names(&query) {
            if let Some(owner) = owners.get(&name) {
                used_metrics.insert(owner.clone());
            } else if let Some(rule) = recorded.get(name.as_str()) {
                if used_records.insert(name.clone()) {
                    reads.push((stack::RULES.to_owned(), rule.line, rule.expr.clone()));
                }
            } else {
                findings.push(Finding::new(
                    &path,
                    line,
                    format!(
                        "`{name}` is neither declared, listed in {EXTERNAL}, nor recorded (OBS-12)"
                    ),
                ));
            }
        }
    }
    unread(catalog, &recorded, &used_metrics, &used_records, findings);
}

/// Fails each metric and recording rule that no dashboard or alert reads.
fn unread(
    catalog: &Catalog,
    recorded: &BTreeMap<&str, &stack::Rule>,
    used_metrics: &BTreeSet<String>,
    used_records: &BTreeSet<String>,
    findings: &mut Vec<Finding>,
) {
    let unread = |name: &str| format!("no dashboard or alert reads `{name}` (OBS-12)");
    for metric in &catalog.declared {
        if !used_metrics.contains(&metric.name) {
            findings.push(Finding::new(
                &metric.path,
                metric.line,
                unread(&metric.name),
            ));
        }
    }
    for metric in &catalog.external {
        if !used_metrics.contains(&metric.name) {
            findings.push(Finding::new(EXTERNAL, 1, unread(&metric.name)));
        }
    }
    for (name, rule) in recorded {
        if !used_records.contains(*name) {
            findings.push(Finding::new(
                stack::RULES,
                rule.line,
                format!("no dashboard or alert reads the recording rule `{name}` (OBS-12)"),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NAMES: &str = r#"pub const ERROR_TYPE: &str = "error.type";"#;
    const DECLARED: &str =
        r#"static P: Counter<0> = Counter::new("platform.panics", "{panic}", "Panics.", []);"#;
    const EXTERNAL_LIST: &str = "metrics:\n  - name: up\n    kind: gauge\n    producer: Prometheus\n    description: Whether a target answers.\n";

    fn dashboard(title: &str, expr: &str) -> String {
        serde_json::json!({"panels": [{"type": "timeseries", "title": title,
            "datasource": {"type": "prometheus"}, "targets": [{"expr": expr}]}]})
        .to_string()
    }

    fn findings(dashboard: &str, rules: &str) -> Vec<String> {
        let mut files = vec![
            ("crates/platform/src/telemetry/names.rs", NAMES.to_owned()),
            ("crates/platform/src/telemetry/mod.rs", DECLARED.to_owned()),
            (EXTERNAL, EXTERNAL_LIST.to_owned()),
            (
                "deploy/observability/grafana/dashboards/a.json",
                dashboard.to_owned(),
            ),
            (stack::RULES, rules.to_owned()),
        ];
        let repo = Repo::of(
            &files
                .iter()
                .map(|(p, t)| (*p, t.as_str()))
                .collect::<Vec<_>>(),
        );
        let catalog = write(&repo).unwrap();
        files.push((CATALOG, catalog));
        let repo = Repo::of(
            &files
                .iter()
                .map(|(p, t)| (*p, t.as_str()))
                .collect::<Vec<_>>(),
        );
        check(&repo)
            .findings
            .into_iter()
            .map(|f| f.message)
            .collect()
    }

    #[test]
    fn obs_12_every_metric_read_is_declared_and_every_declared_one_is_read() {
        let rules =
            "groups:\n  - name: a\n    rules:\n      - alert: Down\n        expr: up == 0\n";
        assert!(
            findings(
                &dashboard("Did anything panic?", "platform_panics_total"),
                rules
            )
            .is_empty()
        );

        let unknown = findings(
            &dashboard("Is it up?", "up + platform_crashes_total"),
            rules,
        );
        assert_eq!(unknown.len(), 2, "{unknown:?}");
        assert!(unknown.iter().any(|f| f.contains("platform_crashes_total")));
        assert!(
            unknown
                .iter()
                .any(|f| f.contains("no dashboard or alert reads `platform.panics`"))
        );
    }

    #[test]
    fn obs_12_a_recording_rule_counts_as_read_only_when_something_reads_it() {
        let rules = "groups:\n  - name: a\n    rules:\n      - record: job:panics:rate5m\n        expr: rate(platform_panics_total[5m])\n      - alert: Down\n        expr: up == 0\n";
        let unread = findings(&dashboard("Is it up?", "up"), rules);
        assert!(
            unread.iter().any(|f| f.contains("job:panics:rate5m")),
            "{unread:?}"
        );
        assert!(
            unread.iter().any(|f| f.contains("platform.panics")),
            "{unread:?}"
        );
        assert!(
            findings(
                &dashboard("Did anything panic?", "job:panics:rate5m"),
                rules
            )
            .is_empty()
        );
    }

    #[test]
    fn obs_13_a_panel_title_is_a_question() {
        let rules =
            "groups:\n  - name: a\n    rules:\n      - alert: Down\n        expr: up == 0\n";
        let titled = findings(&dashboard("Panics", "platform_panics_total"), rules);
        assert_eq!(titled.len(), 1, "{titled:?}");
        assert!(titled[0].contains("OBS-13"));
    }

    #[test]
    fn obs_12_the_committed_catalog_matches_the_declarations() {
        let repo = Repo::of(&[
            ("crates/platform/src/telemetry/names.rs", NAMES),
            ("crates/platform/src/telemetry/mod.rs", DECLARED),
            (EXTERNAL, EXTERNAL_LIST),
            (CATALOG, "# Metric catalog\n"),
        ]);
        assert!(check(&repo).findings.iter().any(|f| f.path == CATALOG));
    }
}
