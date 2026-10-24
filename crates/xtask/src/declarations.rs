//! The metric declarations in the API's code (OBS-11): every `static` built by
//! `Counter::new`, `Histogram::new` or `Gauge::new`, with the names Prometheus gives its series.

use std::collections::BTreeMap;

use syn::{
    Expr, ExprArray, ExprCall, ExprLit, Lit, Type,
    visit::{self, Visit},
};

use crate::{Finding, repo::Repo};

/// The code the API runs, where metrics are declared.
const CODE: [&str; 2] = ["crates/platform/src/", "crates/app/src/"];

/// The root of `platform`, whose metrics are named `platform.<name>`.
const PLATFORM: &str = "crates/platform/src/";

/// The modules of `app`, each of which declares its metrics in `metrics.rs`.
const MODULES: &str = "crates/app/src/modules/";

/// The kinds of metric, by the name of their declaration type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A count that only grows.
    Counter,
    /// A distribution counted in buckets.
    Histogram,
    /// A value that is set.
    Gauge,
}

impl Kind {
    /// The kind's name, in lowercase.
    pub fn name(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Histogram => "histogram",
            Self::Gauge => "gauge",
        }
    }
}

/// One declared metric.
#[derive(Debug, PartialEq)]
pub struct Metric {
    /// Its OpenTelemetry name, such as `platform.panics`.
    pub name: String,
    /// Whether the name is a semantic convention's, declared through `convention`.
    pub convention: bool,
    /// Its kind.
    pub kind: Kind,
    /// Its unit, such as `s` or `{panic}`.
    pub unit: String,
    /// What it measures.
    pub description: String,
    /// Its label names, from the list of telemetry names.
    pub labels: Vec<String>,
    /// The file that declares it.
    pub path: String,
    /// The line of its declaration.
    pub line: usize,
}

impl Metric {
    /// The names of the series Prometheus stores for the metric, under its OTLP receiver's
    /// default translation: dots become underscores, then the unit's suffix and the kind's.
    pub fn series(&self) -> Result<Vec<String>, String> {
        let mut base = self.name.replace('.', "_");
        let suffix = match self.unit.as_str() {
            "s" => Some("seconds"),
            "ms" => Some("milliseconds"),
            "By" => Some("bytes"),
            "1" if self.kind == Kind::Gauge => Some("ratio"),
            unit if unit.is_empty() || (unit.starts_with('{') && unit.ends_with('}')) => None,
            unit => {
                return Err(format!(
                    "check-metrics can't name the unit `{unit}` in Prometheus"
                ));
            }
        };
        if let Some(suffix) = suffix {
            base = format!("{base}_{suffix}");
        }
        Ok(match self.kind {
            Kind::Counter => vec![format!("{base}_total")],
            Kind::Gauge => vec![base],
            Kind::Histogram => ["bucket", "sum", "count"]
                .iter()
                .map(|part| format!("{base}_{part}"))
                .collect(),
        })
    }
}

/// Every metric declared in the API's code, checked against OBS-11: an own name is
/// `<module>.<name>` for the module that declares it, a module of `app` declares its metrics in
/// its `metrics.rs`, and every label is a constant of the list of telemetry names.
pub fn read(
    repo: &Repo,
    names: &BTreeMap<String, String>,
    findings: &mut Vec<Finding>,
) -> Vec<Metric> {
    let mut metrics = Vec::new();
    for file in repo.files() {
        if !CODE.iter().any(|root| file.path.starts_with(root)) || !file.path.ends_with(".rs") {
            continue;
        }
        let parsed = match syn::parse_file(&file.text) {
            Ok(parsed) => parsed,
            Err(error) => {
                findings.push(Finding::new(&file.path, 1, error.to_string()));
                continue;
            }
        };
        let mut visitor = Declarations {
            path: &file.path,
            names,
            metrics: &mut metrics,
            findings,
        };
        visitor.visit_file(&parsed);
    }
    let mut seen: BTreeMap<&str, &Metric> = BTreeMap::new();
    for metric in &metrics {
        if let Some(first) = seen.insert(&metric.name, metric) {
            findings.push(Finding::new(
                &metric.path,
                metric.line,
                format!(
                    "`{}` is declared twice; it is also declared in {}:{} (OBS-11)",
                    metric.name, first.path, first.line
                ),
            ));
        }
    }
    metrics
}

struct Declarations<'a> {
    path: &'a str,
    names: &'a BTreeMap<String, String>,
    metrics: &'a mut Vec<Metric>,
    findings: &'a mut Vec<Finding>,
}

impl<'ast> Visit<'ast> for Declarations<'_> {
    fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
        if let Some(kind) = declared_kind(&item.ty)
            && let Expr::Call(call) = &*item.expr
        {
            let line = item.static_token.span.start().line;
            match self.metric(kind, call, line) {
                Ok(metric) => {
                    self.check(&metric);
                    self.metrics.push(metric);
                }
                Err(message) => self.findings.push(Finding::new(self.path, line, message)),
            }
        }
        visit::visit_item_static(self, item);
    }
}

impl Declarations<'_> {
    /// The metric that `call`, a `new` of the declaration type `kind`, declares.
    fn metric(&self, kind: Kind, call: &ExprCall, line: usize) -> Result<Metric, String> {
        let arguments: Vec<&Expr> = call.args.iter().collect();
        let [name, unit, description, labels, ..] = arguments.as_slice() else {
            return Err(
                "a metric's declaration takes its name, unit, description and labels".into(),
            );
        };
        let (name, convention) = match name {
            Expr::Call(convention) => (
                convention
                    .args
                    .first()
                    .and_then(literal)
                    .ok_or("a convention's name is a literal")?,
                true,
            ),
            name => (literal(name).ok_or("a metric's name is a literal")?, false),
        };
        let Expr::Array(ExprArray { elems, .. }) = labels else {
            return Err("a metric's labels are an array of the list's constants".into());
        };
        let labels = elems
            .iter()
            .map(|label| self.label(label))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Metric {
            name,
            convention,
            kind,
            unit: literal(unit).ok_or("a metric's unit is a literal")?,
            description: literal(description).ok_or("a metric's description is a literal")?,
            labels,
            path: self.path.to_owned(),
            line,
        })
    }

    /// The name of the label `label`, which must be a constant of the list (OBS-05).
    fn label(&self, label: &Expr) -> Result<String, String> {
        let Expr::Path(path) = label else {
            return Err("a label is a constant of the list of telemetry names (OBS-11)".into());
        };
        let constant = path
            .path
            .segments
            .last()
            .map(|segment| segment.ident.to_string())
            .unwrap_or_default();
        self.names.get(&constant).cloned().ok_or_else(|| {
            format!("`{constant}` isn't a constant of the list of telemetry names (OBS-11)")
        })
    }

    /// Fails an own name outside `<module>.<name>`, and a module's declaration outside its
    /// `metrics.rs` (OBS-11).
    fn check(&mut self, metric: &Metric) {
        let module = if self.path.starts_with(PLATFORM) {
            Some("platform".to_owned())
        } else {
            let module = self
                .path
                .strip_prefix(MODULES)
                .and_then(|rest| rest.strip_suffix("/metrics.rs"))
                .filter(|module| !module.contains('/'));
            if module.is_none() {
                self.findings.push(Finding::new(
                    self.path,
                    metric.line,
                    "a module declares its metrics in its metrics.rs (OBS-11)",
                ));
            }
            module.map(str::to_owned)
        };
        let own = module.is_some_and(|module| {
            metric
                .name
                .strip_prefix(&format!("{module}."))
                .is_some_and(|rest| {
                    rest.chars().next().is_some_and(|c| c.is_ascii_lowercase())
                        && rest
                            .chars()
                            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                })
        });
        if !metric.convention && !own {
            self.findings.push(Finding::new(
                self.path,
                metric.line,
                format!(
                    "`{}` is neither a convention's name nor `<module>.<name>` for the module that declares it (OBS-11)",
                    metric.name
                ),
            ));
        }
    }
}

/// The kind of a declaration of type `Counter<N>`, `Histogram<N>` or `Gauge<N>`.
fn declared_kind(ty: &Type) -> Option<Kind> {
    let Type::Path(path) = ty else {
        return None;
    };
    match path.path.segments.last()?.ident.to_string().as_str() {
        "Counter" => Some(Kind::Counter),
        "Histogram" => Some(Kind::Histogram),
        "Gauge" => Some(Kind::Gauge),
        _ => None,
    }
}

fn literal(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(text),
            ..
        }) => Some(text.value()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_code(path: &str, code: &str) -> (Vec<Metric>, Vec<String>) {
        let names = BTreeMap::from([("ERROR_TYPE".to_owned(), "error.type".to_owned())]);
        let repo = Repo::of(&[(path, code)]);
        let mut findings = Vec::new();
        let metrics = read(&repo, &names, &mut findings);
        (metrics, findings.into_iter().map(|f| f.message).collect())
    }

    #[test]
    fn obs_11_a_declaration_is_read_with_its_labels_and_series() {
        let (metrics, findings) = read_code(
            "crates/platform/src/errors.rs",
            r#"static IGNORED: Counter<1> = Counter::new("platform.errors_ignored", "{error}", "Ignored.", [names::ERROR_TYPE]);"#,
        );
        assert!(findings.is_empty(), "{findings:?}");
        assert_eq!(metrics[0].labels, ["error.type"]);
        assert_eq!(
            metrics[0].series().unwrap(),
            ["platform_errors_ignored_total"]
        );
    }

    #[test]
    fn obs_11_a_histogram_in_seconds_has_three_series() {
        let (metrics, _) = read_code(
            "crates/platform/src/http/stack.rs",
            r#"static D: Histogram<0> = Histogram::new(convention("http.server.request.duration", otel::X), "s", "D.", [], &[0.5]);"#,
        );
        assert!(metrics[0].convention);
        assert_eq!(
            metrics[0].series().unwrap(),
            [
                "http_server_request_duration_seconds_bucket",
                "http_server_request_duration_seconds_sum",
                "http_server_request_duration_seconds_count",
            ]
        );
    }

    #[test]
    fn obs_11_an_own_name_starts_with_its_module() {
        let (_, findings) = read_code(
            "crates/app/src/modules/auth/metrics.rs",
            r#"static A: Counter<0> = Counter::new("app.auth.sign_ins", "{sign_in}", "S.", []);
               static B: Counter<0> = Counter::new("auth.sign_ins", "{sign_in}", "S.", []);"#,
        );
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(findings[0].contains("app.auth.sign_ins"));
    }

    #[test]
    fn obs_11_a_module_declares_its_metrics_in_metrics_rs() {
        let (_, findings) = read_code(
            "crates/app/src/modules/auth/service.rs",
            r#"static A: Counter<0> = Counter::new("auth.sign_ins", "{sign_in}", "S.", []);"#,
        );
        assert!(
            findings.iter().any(|f| f.contains("metrics.rs")),
            "{findings:?}"
        );
    }

    #[test]
    fn obs_11_a_label_comes_from_the_list() {
        let (_, findings) = read_code(
            "crates/platform/src/errors.rs",
            r#"static A: Counter<1> = Counter::new("platform.a", "{a}", "A.", ["error.type"]);
               static B: Counter<1> = Counter::new("platform.b", "{b}", "B.", [names::ROUTE]);"#,
        );
        assert_eq!(findings.len(), 2, "{findings:?}");
    }
}
