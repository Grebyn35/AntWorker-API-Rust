//! The metric declarations in the API's code (OBS-11), with the names Prometheus gives their
//! series. The check reads each Rust file under `crates/platform/src/` and `crates/app/src/` as
//! `syn` parses it, but the items inside the braces of a `mod` that carries `#[cfg(test)]`, a
//! `cfg` whose argument is `test` alone. A declaration is a `static` of a declaration type, one
//! whose path's last segment is `Counter`, `Histogram` or `Gauge`, and the check reads it from
//! its initializer, a call of a path whose last two segments are one of the three and `new`, as
//! `Counter::new` is, or of `<T>::new` for such a type `T`, each identifier raw or not. Each of
//! its labels is the path `names::X`, neither identifier raw, for `X` a constant of the list of
//! telemetry names, where [`bindings`] says what the check refuses that would give `names`
//! another meaning.
//!
//! It refuses a static of a declaration type that anything else builds, a path of that form to
//! `new` anywhere else, a declaration's arguments among them, a `use` or a `type` that gives a
//! declaration type another name, and a macro call, such as `thread_local! { ... }` or a
//! `macro_rules!`, whose tokens, those of the groups they hold among them, hold the name of a
//! declaration type as an identifier, raw or not. It reads no macro call's tokens as
//! declarations, since they are code only as the macro expands them, as a `macro_rules!` fills
//! in its metavariables.

use std::collections::BTreeMap;

use proc_macro2::{TokenStream, TokenTree};
use syn::{
    Expr, ExprArray, ExprCall, ExprLit, Lit, Type,
    ext::IdentExt,
    visit::{self, Visit},
};

use crate::{Finding, instrument::bindings, repo::Repo};

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

/// The metrics that the API's code declares, as the module's doc says the check reads them,
/// checked against OBS-11: an own name is `<module>.<name>` for the module that declares it, a
/// module of `app` declares its metrics in its `metrics.rs`, and each label is `names::X` for `X`
/// a constant of the list of telemetry names; with a finding for what the module's doc says the
/// check refuses.
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
        if let Ok(tokens) = file.text.parse::<TokenStream>() {
            bindings::check(&file.path, tokens, findings);
        }
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
    /// Skips the items inside the braces of a `mod` that carries `#[cfg(test)]`, as
    /// [`only_tests`] reads one, whose statics the API never records.
    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        if !only_tests(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    /// Reads a static of a declaration type as a metric, from its initializer, which is a call of
    /// a path that [`names_new`], then visits the call's arguments; reads no further into another
    /// initializer.
    fn visit_item_static(&mut self, item: &'ast syn::ItemStatic) {
        let Some(kind) = declared_kind(&item.ty) else {
            visit::visit_item_static(self, item);
            return;
        };
        let line = item.static_token.span.start().line;
        let declared = match &*item.expr {
            Expr::Call(call) if matches!(&*call.func, Expr::Path(path) if names_new(path)) => {
                let metric = self.metric(kind, call, line);
                for argument in &call.args {
                    self.visit_expr(argument);
                }
                metric
            }
            _ => Err(
                "a metric's static is a call of its declaration type's `new`, which \
                      check-metrics reads (OBS-11)"
                    .into(),
            ),
        };
        match declared {
            Ok(metric) => {
                self.check(&metric);
                self.metrics.push(metric);
            }
            Err(message) => self.findings.push(Finding::new(self.path, line, message)),
        }
    }

    /// Refuses a macro call whose tokens hold a declaration type's name as an identifier, raw or
    /// not, since the check reads no macro call's tokens as declarations.
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if let Some((kind, line)) = kind_among(mac.tokens.clone()) {
            self.findings.push(Finding::new(
                self.path,
                line,
                format!(
                    "a macro's tokens name the declaration type `{kind}`, which check-metrics \
                     reads as a declaration in no macro's tokens: declare a metric outside a \
                     macro (OBS-11)"
                ),
            ));
        }
        visit::visit_macro(self, mac);
    }

    /// Refuses a path that [`names_new`] anywhere but in the initializer of a static of a
    /// declaration type, which the visitor reads no further into.
    fn visit_expr_path(&mut self, path: &'ast syn::ExprPath) {
        if names_new(path) {
            let line = path
                .path
                .segments
                .last()
                .map_or(1, |segment| segment.ident.span().start().line);
            self.findings.push(Finding::new(
                self.path,
                line,
                "a declaration type's `new` builds a metric's static alone, whose initializer \
                 check-metrics reads (OBS-11)",
            ));
        }
        visit::visit_expr_path(self, path);
    }

    /// Refuses a `use` that renames a declaration type.
    fn visit_use_rename(&mut self, rename: &'ast syn::UseRename) {
        if kind_named(&rename.ident).is_some() {
            self.findings
                .push(renamed(self.path, rename.ident.span().start().line));
        }
        visit::visit_use_rename(self, rename);
    }

    /// Refuses a `type` whose type is a declaration type.
    fn visit_item_type(&mut self, item: &'ast syn::ItemType) {
        if declared_kind(&item.ty).is_some() {
            self.findings
                .push(renamed(self.path, item.type_token.span.start().line));
        }
        visit::visit_item_type(self, item);
    }
}

impl Declarations<'_> {
    /// The metric that `call`, a `new` of the declaration type `kind`, declares. Its name is a
    /// string literal, or a convention's: the first argument, a string literal, of a call whose
    /// path's last segment is `convention`.
    fn metric(&self, kind: Kind, call: &ExprCall, line: usize) -> Result<Metric, String> {
        let arguments: Vec<&Expr> = call.args.iter().collect();
        let [name, unit, description, labels, ..] = arguments.as_slice() else {
            return Err(
                "a metric's declaration takes its name, unit, description and labels".into(),
            );
        };
        let (name, convention) = match name {
            Expr::Call(convention) if calls_convention(convention) => (
                convention
                    .args
                    .first()
                    .and_then(literal)
                    .ok_or("a convention's name is a literal")?,
                true,
            ),
            name => (
                literal(name)
                    .ok_or("a metric's name is a literal, or `convention` of one (OBS-11)")?,
                false,
            ),
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

    /// The name that the label `label` holds, which is the path `names::X`, neither identifier
    /// raw, for `X` a constant of the list (OBS-11).
    fn label(&self, label: &Expr) -> Result<String, String> {
        let written = "a label is `names::X`, for `X` a constant of the list of telemetry names \
                       (OBS-11)";
        let Expr::Path(syn::ExprPath {
            qself: None, path, ..
        }) = label
        else {
            return Err(written.into());
        };
        let segments: Vec<&syn::PathSegment> = path.segments.iter().collect();
        let [module, constant] = segments.as_slice() else {
            return Err(written.into());
        };
        if path.leading_colon.is_some()
            || module.ident != "names"
            || !module.arguments.is_none()
            || !constant.arguments.is_none()
        {
            return Err(written.into());
        }
        let constant = constant.ident.to_string();
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

/// Whether `attrs` hold `#[cfg(test)]`.
pub fn only_tests(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr
                .parse_args::<syn::Path>()
                .is_ok_and(|path| path.is_ident("test"))
    })
}

/// The kind of a declaration of type `Counter<N>`, `Histogram<N>` or `Gauge<N>`: a path whose
/// last segment is one of the three.
fn declared_kind(ty: &Type) -> Option<Kind> {
    let Type::Path(path) = ty else {
        return None;
    };
    kind_named(&path.path.segments.last()?.ident)
}

/// The kind of the declaration type that `ident` names, raw or not, if it names one.
fn kind_named(ident: &syn::Ident) -> Option<Kind> {
    match ident.unraw().to_string().as_str() {
        "Counter" => Some(Kind::Counter),
        "Histogram" => Some(Kind::Histogram),
        "Gauge" => Some(Kind::Gauge),
        _ => None,
    }
}

/// The first identifier among `tokens`, and the tokens of the groups they hold, that names a
/// declaration type, raw or not, with its name and its line.
fn kind_among(tokens: TokenStream) -> Option<(String, usize)> {
    tokens.into_iter().find_map(|token| match token {
        TokenTree::Ident(ident) => kind_named(&ident)
            .is_some()
            .then(|| (ident.unraw().to_string(), ident.span().start().line)),
        TokenTree::Group(group) => kind_among(group.stream()),
        TokenTree::Punct(_) | TokenTree::Literal(_) => None,
    })
}

/// Whether `path` names a declaration type's `new`: its last two segments are `Counter`,
/// `Histogram` or `Gauge`, then `new`, as in `Counter::<1>::new`, or its last is `new` and its
/// qualified type is a declaration type, as in `<Counter<1>>::new`, each identifier raw or not.
fn names_new(path: &syn::ExprPath) -> bool {
    let segments: Vec<&syn::PathSegment> = path.path.segments.iter().collect();
    let new = |segment: &syn::PathSegment| segment.ident.unraw() == "new";
    match (&path.qself, segments.as_slice()) {
        (Some(qself), [.., last]) => new(last) && declared_kind(&qself.ty).is_some(),
        (None, [.., kind, last]) => new(last) && kind_named(&kind.ident).is_some(),
        _ => false,
    }
}

/// The finding of a `use` or a `type` that gives a declaration type another name, at `line` of
/// the file at `path`.
fn renamed(path: &str, line: usize) -> Finding {
    Finding::new(
        path,
        line,
        "a declaration type keeps its name, by which check-metrics finds a metric's static \
         (OBS-11)",
    )
}

/// Whether `call` is a call of a path whose last segment is `convention`.
fn calls_convention(call: &ExprCall) -> bool {
    matches!(&*call.func, Expr::Path(path)
        if path.path.segments.last().is_some_and(|segment| segment.ident == "convention"))
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
    fn obs_11_a_name_is_a_convention_s_only_through_convention() {
        let (metrics, findings) = read_code(
            "crates/platform/src/http/stack.rs",
            r#"static A: Counter<0> = Counter::new(core::convert::identity("orders"), "{order}", "O.", []);
               static B: Counter<0> = Counter::new(names::convention("http.server.active_requests", otel::X), "{request}", "R.", []);"#,
        );

        assert_eq!(
            findings,
            ["a metric's name is a literal, or `convention` of one (OBS-11)"]
        );
        assert_eq!(metrics.len(), 1, "{metrics:?}");
        assert!(metrics[0].convention, "{metrics:?}");
    }

    #[test]
    fn obs_11_a_metric_s_static_that_isn_t_a_call_of_its_type_s_new_is_refused() {
        let (metrics, findings) = read_code(
            "crates/platform/src/errors.rs",
            r#"static A: Counter<0> = { Counter::new("orders", "{order}", "O.", []) };
               static B: Counter<0> = own("platform.b", "{b}", "B.", []);
               static C: Counter<0> = OTHER;
               static D: metrics::Counter<0> = metrics::Counter::<0>::new("platform.d", "{d}", "D.", []);"#,
        );

        let refused = "a metric's static is a call of its declaration type's `new`, which \
                       check-metrics reads (OBS-11)";
        assert_eq!(findings, [refused, refused, refused]);
        let names: Vec<&str> = metrics.iter().map(|metric| metric.name.as_str()).collect();
        assert_eq!(names, ["platform.d"]);
    }

    #[test]
    fn obs_11_a_declaration_type_s_new_outside_a_static_is_refused() {
        let (metrics, findings) = read_code(
            "crates/platform/src/errors.rs",
            r#"const A: Counter<0> = Counter::new("orders", "{order}", "O.", []);
               const B: Counter<0> = <Counter<0>>::new("orders", "{order}", "O.", []);
               fn c() { Counter::<0>::new("orders", "{order}", "O.", []).add(1, []); }
               fn d() { let make = Histogram::<0>::new; }"#,
        );

        let refused = "a declaration type's `new` builds a metric's static alone, whose \
                       initializer check-metrics reads (OBS-11)";
        assert_eq!(findings, [refused, refused, refused, refused]);
        assert!(metrics.is_empty(), "{metrics:?}");
    }

    #[test]
    fn obs_11_a_declaration_type_keeps_its_name() {
        let (_, findings) = read_code(
            "crates/platform/src/errors.rs",
            r#"use crate::telemetry::metrics::{Counter as Tally, Gauge};
               type Measure<const N: usize> = metrics::Histogram<N>;
               static A: Tally<0> = Tally::new("orders", "{order}", "O.", []);"#,
        );

        let refused = "a declaration type keeps its name, by which check-metrics finds a \
                       metric's static (OBS-11)";
        assert_eq!(findings, [refused, refused]);
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
    fn obs_11_a_static_only_tests_compile_declares_no_metric() {
        let (metrics, findings) = read_code(
            "crates/platform/src/errors.rs",
            r#"#[cfg(test)] mod tests { static T: Counter<0> = Counter::new("platform.t", "{t}", "T.", []); }"#,
        );
        assert!(metrics.is_empty(), "{metrics:?}");
        assert!(findings.is_empty(), "{findings:?}");
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

    /// The line of each finding of `check-metrics`' declarations in the Rust file at `path`,
    /// which holds `code`, with the names of the metrics it declares.
    fn lines(path: &str, code: &str) -> (Vec<String>, Vec<usize>) {
        let names = BTreeMap::from([("ERROR_TYPE".to_owned(), "error.type".to_owned())]);
        let repo = Repo::of(&[(path, code)]);
        let mut findings = Vec::new();
        let metrics = read(&repo, &names, &mut findings);
        (
            metrics.into_iter().map(|metric| metric.name).collect(),
            findings.into_iter().map(|finding| finding.line).collect(),
        )
    }

    #[test]
    fn obs_11_a_label_is_names_and_a_constant_of_the_list_written_so() {
        // A constant of another module, or none, that bears the name of one of the list's.
        let (metrics, findings) = read_code(
            "crates/platform/src/errors.rs",
            r#"static A: Counter<1> = Counter::new("platform.a", "{a}", "A.", [own::ERROR_TYPE]);
               static B: Counter<1> = Counter::new("platform.b", "{b}", "B.", [ERROR_TYPE]);
               static C: Counter<1> = Counter::new("platform.c", "{c}", "C.", [crate::telemetry::names::ERROR_TYPE]);
               static D: Counter<1> = Counter::new("platform.d", "{d}", "D.", [r#names::ERROR_TYPE]);
               static E: Counter<1> = Counter::new("platform.e", "{e}", "E.", [names::ERROR_TYPE]);"#,
        );

        let refused = "a label is `names::X`, for `X` a constant of the list of telemetry names \
                       (OBS-11)";
        assert_eq!(findings, [refused, refused, refused, refused]);
        let names: Vec<&str> = metrics.iter().map(|metric| metric.name.as_str()).collect();
        assert_eq!(names, ["platform.e"]);
    }

    #[test]
    fn obs_11_the_names_of_a_label_is_the_list_s_module() {
        let code = r#"use crate::own as names;
            use other::names;
            static A: Counter<1> = Counter::new("platform.a", "{a}", "A.", [names::ERROR_TYPE]);"#;

        let (metrics, findings) = lines("crates/platform/src/errors.rs", code);

        assert_eq!(
            (metrics, findings),
            (vec!["platform.a".to_owned()], vec![1, 2])
        );
    }

    #[test]
    fn obs_11_a_declaration_type_named_in_a_macro_s_tokens_is_refused() {
        let code = r#"thread_local! { static A: Counter<0> = const { Counter::new("platform.a", "{a}", "A.", []) }; }
            macro_rules! declare { () => { static B: metrics::Gauge<0> = metrics::Gauge::new("platform.b", "{b}", "B.", []); }; }
            declare!();
            fn c() { let all = vec![Histogram::<0>::new("platform.c", "s", "C.", [], &[0.5])]; }
            macro_rules! of { ($t:ident) => { static D: $t<0> = $t::new("platform.d", "{d}", "D.", []); }; }
            of!(r#Counter);
            fn e() { assert!(matches!(kind, Kind::Gauge)); }"#;

        let (metrics, findings) = lines("crates/platform/src/errors.rs", code);

        assert_eq!((metrics, findings), (vec![], vec![1, 2, 4, 6, 7]));
    }

    #[test]
    fn obs_11_a_declaration_type_s_new_inside_a_declaration_s_arguments_is_refused() {
        let code = r#"static A: Histogram<0> = Histogram::new("platform.a", "s", "A.", [], {
                const B: Counter<0> = Counter::new("platform.b", "{b}", "B.", []);
                &[0.5]
            });"#;

        let (metrics, findings) = lines("crates/platform/src/errors.rs", code);

        assert_eq!(
            (metrics, findings),
            (vec!["platform.a".to_owned()], vec![2])
        );
    }

    #[test]
    fn obs_11_a_raw_identifier_names_a_declaration_type_and_its_new() {
        let code = r#"static A: r#Counter<0> = r#Counter::new("platform.a", "{a}", "A.", []);
            fn b() { Counter::<0>::r#new("platform.b", "{b}", "B.", []).add(1, []); }
            use crate::telemetry::metrics::r#Gauge as Level;"#;

        let (metrics, findings) = lines("crates/platform/src/errors.rs", code);

        assert_eq!(
            (metrics, findings),
            (vec!["platform.a".to_owned()], vec![2, 3])
        );
    }

    #[test]
    fn obs_11_a_module_whose_cfg_names_more_than_test_is_read() {
        let code = r#"#[cfg(any(test, feature = "test-support"))]
            mod testing { static T: Counter<0> = Counter::new("platform.t", "{t}", "T.", []); }"#;

        let (metrics, findings) = lines("crates/platform/src/errors.rs", code);

        assert_eq!((metrics, findings), (vec!["platform.t".to_owned()], vec![]));
    }
}
