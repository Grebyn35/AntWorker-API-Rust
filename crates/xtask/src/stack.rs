//! What the checks read from the observability stack's configuration: the dashboards' queries,
//! the Prometheus rules and their tests, and the collector's configuration.

use serde_json::Value;

use crate::{Finding, repo::Repo, text};

/// The directory of the dashboards Grafana provisions (OBS-13).
pub const DASHBOARDS: &str = "deploy/observability/grafana/dashboards/";

/// The Prometheus rules: the recording rules and the alerts (OBS-14).
pub const RULES: &str = "deploy/observability/prometheus/rules.yaml";

/// The tests of the rules, which promtool runs.
pub const RULE_TESTS: &str = "deploy/observability/prometheus/rules.test.yaml";

/// The collector's configuration (OBS-08).
pub const COLLECTOR: &str = "deploy/observability/collector/config.yaml";

/// The store a query reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Store {
    /// PromQL, against Prometheus.
    Prometheus,
    /// TraceQL, against Tempo.
    Tempo,
    /// Anything else.
    Other,
}

/// One query of one dashboard panel.
#[derive(Debug)]
pub struct PanelQuery {
    /// The dashboard's file.
    pub path: String,
    /// The line the query is on.
    pub line: usize,
    /// The panel's title.
    pub title: String,
    /// The store the query reads.
    pub store: Store,
    /// The query.
    pub query: String,
    /// The values of the panel's threshold steps, such as the line a graph draws.
    pub thresholds: Vec<f64>,
}

/// A panel's title, with where it is.
#[derive(Debug)]
pub struct PanelTitle {
    /// The dashboard's file.
    pub path: String,
    /// The line the title is on.
    pub line: usize,
    /// The title.
    pub title: String,
}

/// One dashboard: what makes it read-only, and where it links.
#[derive(Debug)]
pub struct Board {
    /// The dashboard's file.
    pub path: String,
    /// Its unique ID, by which a link reaches it as `/d/<uid>`.
    pub uid: String,
    /// Whether its `editable` is anything but `false`, which lets Grafana's users change it.
    pub editable: bool,
    /// The address of each of its links.
    pub links: Vec<String>,
}

/// Every dashboard, with its panel titles and queries.
#[derive(Debug, Default)]
pub struct Dashboards {
    /// Each dashboard.
    pub boards: Vec<Board>,
    /// Each panel's title.
    pub titles: Vec<PanelTitle>,
    /// Each query of each panel.
    pub queries: Vec<PanelQuery>,
}

/// Reads every dashboard under [`DASHBOARDS`], adding a finding for one that isn't JSON.
pub fn dashboards(repo: &Repo, findings: &mut Vec<Finding>) -> Dashboards {
    let mut dashboards = Dashboards::default();
    for file in repo.files() {
        if !file.path.starts_with(DASHBOARDS) || !file.path.ends_with(".json") {
            continue;
        }
        let board: Value = match serde_json::from_str(&file.text) {
            Ok(board) => board,
            Err(error) => {
                findings.push(Finding::new(&file.path, 1, error.to_string()));
                continue;
            }
        };
        dashboards.boards.push(Board {
            path: file.path.clone(),
            uid: board["uid"].as_str().unwrap_or_default().to_owned(),
            editable: board["editable"] != false,
            links: board["links"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|link| link["url"].as_str().map(str::to_owned))
                .collect(),
        });
        let mut panels: Vec<&Value> = Vec::new();
        collect_panels(&board["panels"], &mut panels);
        for panel in panels {
            if panel["type"] == "row" {
                continue;
            }
            let title = panel["title"].as_str().unwrap_or_default().to_owned();
            dashboards.titles.push(PanelTitle {
                path: file.path.clone(),
                line: text::line_of(&file.text, &format!("\"title\": {}", json(&title))),
                title: title.clone(),
            });
            for target in panel["targets"].as_array().into_iter().flatten() {
                let source = if target["datasource"].is_object() {
                    &target["datasource"]
                } else {
                    &panel["datasource"]
                };
                let store = match source["type"].as_str() {
                    Some("prometheus") => Store::Prometheus,
                    Some("tempo") => Store::Tempo,
                    _ => Store::Other,
                };
                let field = if store == Store::Prometheus {
                    "expr"
                } else {
                    "query"
                };
                if let Some(query) = target[field].as_str() {
                    dashboards.queries.push(PanelQuery {
                        path: file.path.clone(),
                        line: text::line_of(&file.text, &json(query)),
                        title: title.clone(),
                        store,
                        query: query.to_owned(),
                        thresholds: panel["fieldConfig"]["defaults"]["thresholds"]["steps"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|step| step["value"].as_f64())
                            .collect(),
                    });
                }
            }
        }
    }
    dashboards
}

/// The panels in `panels`, and the panels of each row among them.
fn collect_panels<'a>(panels: &'a Value, into: &mut Vec<&'a Value>) {
    for panel in panels.as_array().into_iter().flatten() {
        into.push(panel);
        collect_panels(&panel["panels"], into);
    }
}

/// `value` as a JSON string, as a dashboard's file holds it.
fn json(value: &str) -> String {
    Value::String(value.to_owned()).to_string()
}

/// A Prometheus rule.
#[derive(Debug)]
pub struct Rule {
    /// The line the rule starts on.
    pub line: usize,
    /// The name a recording rule records, or `None` for an alert.
    pub record: Option<String>,
    /// An alert's name, or `None` for a recording rule.
    pub alert: Option<String>,
    /// The expression.
    pub expr: String,
    /// The rule's labels.
    pub labels: Value,
    /// The rule's annotations.
    pub annotations: Value,
}

/// The rules of [`RULES`], adding a finding if the file is missing or isn't YAML.
pub fn rules(repo: &Repo, findings: &mut Vec<Finding>) -> Vec<Rule> {
    let Some(document) = yaml(repo, RULES, findings) else {
        return Vec::new();
    };
    let text = repo.file(RULES).map_or("", |file| file.text.as_str());
    let mut rules = Vec::new();
    for group in document["groups"].as_array().into_iter().flatten() {
        for rule in group["rules"].as_array().into_iter().flatten() {
            let record = rule["record"].as_str().map(str::to_owned);
            let alert = rule["alert"].as_str().map(str::to_owned);
            let key = match (&record, &alert) {
                (Some(record), _) => format!("record: {record}"),
                (_, Some(alert)) => format!("alert: {alert}"),
                _ => String::new(),
            };
            rules.push(Rule {
                line: text::line_of(text, &key),
                record,
                alert,
                expr: rule["expr"].as_str().unwrap_or_default().to_owned(),
                labels: rule["labels"].clone(),
                annotations: rule["annotations"].clone(),
            });
        }
    }
    rules
}

/// The YAML file at `path` as JSON values, adding a finding if it is missing or isn't YAML.
pub fn yaml(repo: &Repo, path: &str, findings: &mut Vec<Finding>) -> Option<Value> {
    let Some(file) = repo.file(path) else {
        findings.push(Finding::new(path, 1, "the file is missing"));
        return None;
    };
    match serde_saphyr::from_str::<Value>(&file.text) {
        Ok(value) => Some(value),
        Err(error) => {
            findings.push(Finding::new(path, 1, format!("not YAML: {error}")));
            None
        }
    }
}
