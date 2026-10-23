//! `check-instrument`: logs and spans name their fields only from the list in
//! `platform::telemetry::names`, their messages are constant, every `#[instrument]` skips its
//! arguments, and module code writes only `DEBUG` events and opens no spans (OBS-01, OBS-03 to
//! OBS-06).
//!
//! It reads token streams rather than syntax trees, so it also finds the calls inside other
//! macros, such as `tokio::select!`.

use std::collections::BTreeMap;

use proc_macro2::{Delimiter, Group, Spacing, TokenStream, TokenTree};

use crate::{Finding, Outcome, repo::Repo};

/// The file that holds the list of names.
const NAMES: &str = "crates/platform/src/telemetry/names.rs";

/// The code the API runs.
const CODE: [&str; 2] = ["crates/platform/src/", "crates/app/src/"];

/// The feature modules, which write only `DEBUG` events and open no spans.
const MODULES: &str = "crates/app/src/modules/";

/// The event macros, with the level each writes at, or `None` for `event!`, which takes it.
const EVENTS: [(&str, Option<&str>); 6] = [
    ("trace", Some("TRACE")),
    ("debug", Some("DEBUG")),
    ("info", Some("INFO")),
    ("warn", Some("WARN")),
    ("error", Some("ERROR")),
    ("event", None),
];

/// The span macros.
const SPANS: [&str; 6] = [
    "span",
    "trace_span",
    "debug_span",
    "info_span",
    "warn_span",
    "error_span",
];

/// Checks every file of the API's code against the list of names.
pub fn check(repo: &Repo) -> Outcome {
    let mut findings = Vec::new();
    let names = match repo.file(NAMES).map(|file| list(&file.text)) {
        Some(Ok(names)) => names,
        Some(Err(message)) => return failed(Finding::new(NAMES, 1, message)),
        None => {
            return failed(Finding::new(
                NAMES,
                1,
                "the list of telemetry names is missing",
            ));
        }
    };
    for file in repo.files() {
        let in_code = CODE.iter().any(|root| file.path.starts_with(root));
        if !in_code || !file.path.ends_with(".rs") || file.path == NAMES {
            continue;
        }
        match file.text.parse::<TokenStream>() {
            Ok(tokens) => {
                let mut scan = Scan {
                    path: &file.path,
                    module: file.path.starts_with(MODULES),
                    names: &names,
                    findings: &mut findings,
                };
                scan.stream(tokens);
            }
            Err(error) => findings.push(Finding::new(&file.path, 1, error.to_string())),
        }
    }
    Outcome {
        report: Vec::new(),
        findings,
    }
}

fn failed(finding: Finding) -> Outcome {
    Outcome {
        report: Vec::new(),
        findings: vec![finding],
    }
}

/// The list's constants, each with the name it holds: `const X: &str = "name";` or
/// `const X: &str = convention("name", ...);`.
fn list(text: &str) -> Result<BTreeMap<String, String>, String> {
    let file = syn::parse_file(text).map_err(|error| error.to_string())?;
    let mut names = BTreeMap::new();
    for item in file.items {
        let syn::Item::Const(constant) = item else {
            continue;
        };
        let literal = match &*constant.expr {
            syn::Expr::Lit(literal) => Some(&literal.lit),
            syn::Expr::Call(call) => call.args.first().and_then(|argument| match argument {
                syn::Expr::Lit(literal) => Some(&literal.lit),
                _ => None,
            }),
            _ => None,
        };
        if let Some(syn::Lit::Str(name)) = literal {
            names.insert(constant.ident.to_string(), name.value());
        }
    }
    Ok(names)
}

struct Scan<'a> {
    path: &'a str,
    module: bool,
    names: &'a BTreeMap<String, String>,
    findings: &'a mut Vec<Finding>,
}

impl Scan<'_> {
    fn stream(&mut self, tokens: TokenStream) {
        let tokens: Vec<TokenTree> = tokens.into_iter().collect();
        for (at, token) in tokens.iter().enumerate() {
            match token {
                TokenTree::Ident(ident) => {
                    let name = ident.to_string();
                    if let (Some(TokenTree::Punct(bang)), Some(TokenTree::Group(group))) =
                        (tokens.get(at + 1), tokens.get(at + 2))
                        && bang.as_char() == '!'
                        && from_tracing(&tokens[..at])
                    {
                        self.tracing_macro(&name, group);
                    }
                    if name == "record"
                        && at > 0
                        && matches!(&tokens[at - 1], TokenTree::Punct(dot) if dot.as_char() == '.')
                        && let Some(TokenTree::Group(group)) = tokens.get(at + 1)
                        && group.delimiter() == Delimiter::Parenthesis
                    {
                        self.record(group);
                    }
                }
                TokenTree::Punct(hash) if hash.as_char() == '#' => {
                    if let Some(TokenTree::Group(attribute)) = tokens.get(at + 1)
                        && attribute.delimiter() == Delimiter::Bracket
                    {
                        self.attribute(attribute);
                    }
                }
                _ => {}
            }
            if let TokenTree::Group(group) = token {
                self.stream(group.stream());
            }
        }
    }

    fn tracing_macro(&mut self, name: &str, group: &Group) {
        let line = group.span().start().line;
        let segments = segments(group.stream());
        let mut segments: Vec<&[TokenTree]> = segments
            .iter()
            .map(Vec::as_slice)
            .filter(|segment| !is_keyword_argument(segment))
            .collect();
        if let Some((_, level)) = EVENTS.iter().find(|(event, _)| *event == name) {
            let level = match level {
                Some(level) => (*level).to_owned(),
                None if segments.is_empty() => String::new(),
                None => last_ident(segments.remove(0)).unwrap_or_default(),
            };
            if self.module && !matches!(level.as_str(), "DEBUG" | "TRACE") {
                self.find(
                    line,
                    format!("module code writes a {level} event; only DEBUG is allowed (OBS-03)"),
                );
            }
            self.fields(line, &segments, true);
        } else if SPANS.contains(&name) {
            if self.module {
                self.find(line, "module code opens a span (OBS-06)");
            }
            if name == "span" && !segments.is_empty() {
                segments.remove(0);
            }
            if !segments.is_empty() {
                // The span's name.
                segments.remove(0);
            }
            self.fields(line, &segments, false);
        }
    }

    /// Checks each field's name, and that an event's message is constant (OBS-01, OBS-05).
    fn fields(&mut self, line: usize, segments: &[&[TokenTree]], event: bool) {
        let mut message_seen = false;
        for segment in segments {
            if message_seen {
                self.find(line, "a message with arguments isn't constant (OBS-01)");
                break;
            }
            if event && let [TokenTree::Literal(literal)] = segment {
                let text = literal.to_string();
                if text.starts_with('"') {
                    message_seen = true;
                    if text.replace("{{", "").contains('{') {
                        self.find(line, "a message with placeholders isn't constant (OBS-01)");
                    }
                    continue;
                }
            }
            let name = match segment.iter().position(is_assignment) {
                Some(equals) => self.field_name(&segment[..equals]),
                None => shorthand_name(segment),
            };
            match name {
                Some(name) if self.names.values().any(|listed| *listed == name) => {}
                Some(name) => self.find(
                    line,
                    format!("the field name {name} isn't in the list of telemetry names (OBS-05)"),
                ),
                None => self.find(
                    line,
                    "a field name that isn't a name or one of the list's constants (OBS-05)",
                ),
            }
        }
    }

    /// The name before a field's `=`: dotted identifiers, a string, or `{ CONSTANT }`.
    fn field_name(&self, tokens: &[TokenTree]) -> Option<String> {
        match tokens {
            [TokenTree::Group(group)] if group.delimiter() == Delimiter::Brace => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                let constant = last_ident(&inner)?;
                self.names.get(&constant).cloned()
            }
            [TokenTree::Literal(literal)] => string(literal),
            _ => dotted(tokens),
        }
    }

    /// `.record(name, value)`: the name is a listed name in a string, or one of the list's
    /// constants (OBS-05).
    fn record(&mut self, group: &Group) {
        let line = group.span().start().line;
        let segments = segments(group.stream());
        let Some(first) = segments.first() else {
            return;
        };
        let name = match first.as_slice() {
            [TokenTree::Literal(literal)] => string(literal),
            tokens => last_ident(tokens).and_then(|constant| self.names.get(&constant).cloned()),
        };
        if !name.is_some_and(|name| self.names.values().any(|listed| *listed == name)) {
            self.find(
                line,
                "a record name that isn't a listed name or one of the list's constants (OBS-05)",
            );
        }
    }

    /// `#[instrument(...)]`: it skips every argument, and names its fields from the list
    /// (OBS-04, OBS-05). Module code opens no spans, so it has none (OBS-06).
    fn attribute(&mut self, attribute: &Group) {
        let tokens: Vec<TokenTree> = attribute.stream().into_iter().collect();
        let Some(at) = tokens
            .iter()
            .position(|token| matches!(token, TokenTree::Ident(ident) if ident == "instrument"))
        else {
            return;
        };
        if !from_tracing(&tokens[..at]) {
            return;
        }
        let line = attribute.span().start().line;
        if self.module {
            self.find(line, "module code opens a span with #[instrument] (OBS-06)");
        }
        let arguments = match tokens.get(at + 1) {
            Some(TokenTree::Group(arguments)) => arguments.stream(),
            _ => TokenStream::new(),
        };
        let arguments: Vec<TokenTree> = arguments.into_iter().collect();
        if !arguments
            .iter()
            .any(|token| matches!(token, TokenTree::Ident(ident) if ident == "skip_all"))
        {
            self.find(line, "#[instrument] without skip_all (OBS-04)");
        }
        for (at, token) in arguments.iter().enumerate() {
            if matches!(token, TokenTree::Ident(ident) if ident == "fields")
                && let Some(TokenTree::Group(fields)) = arguments.get(at + 1)
            {
                let segments = segments(fields.stream());
                let segments: Vec<&[TokenTree]> = segments.iter().map(Vec::as_slice).collect();
                self.fields(line, &segments, false);
            }
        }
    }

    fn find(&mut self, line: usize, message: impl Into<String>) {
        self.findings.push(Finding::new(self.path, line, message));
    }
}

/// Whether the tokens before a macro's name make it a `tracing` macro: `tracing::`, or no path
/// at all, as after `use tracing::info`.
fn from_tracing(before: &[TokenTree]) -> bool {
    match before {
        [
            ..,
            TokenTree::Ident(crate_name),
            TokenTree::Punct(a),
            TokenTree::Punct(b),
        ] if a.as_char() == ':' && b.as_char() == ':' => crate_name == "tracing",
        [.., TokenTree::Punct(colon)] if colon.as_char() == ':' => false,
        _ => true,
    }
}

/// The tokens between top-level commas.
fn segments(tokens: TokenStream) -> Vec<Vec<TokenTree>> {
    let mut segments = vec![Vec::new()];
    for token in tokens {
        match &token {
            TokenTree::Punct(comma) if comma.as_char() == ',' => segments.push(Vec::new()),
            _ => segments
                .last_mut()
                .map(|segment| segment.push(token))
                .unwrap_or_default(),
        }
    }
    segments.retain(|segment| !segment.is_empty());
    segments
}

/// `target: ...`, `parent: ...` and `name: ...`, which name no field.
fn is_keyword_argument(segment: &[TokenTree]) -> bool {
    matches!(
        segment,
        [TokenTree::Ident(keyword), TokenTree::Punct(colon), ..]
            if colon.as_char() == ':'
                && colon.spacing() == Spacing::Alone
                && ["target", "parent", "name"].iter().any(|k| keyword == k)
    )
}

/// A lone `=`, which separates a field's name from its value.
fn is_assignment(token: &TokenTree) -> bool {
    matches!(token, TokenTree::Punct(equals) if equals.as_char() == '=' && equals.spacing() == Spacing::Alone)
}

/// The name of a field written without a value: `name`, `%name`, `?name` or `a.b`.
fn shorthand_name(tokens: &[TokenTree]) -> Option<String> {
    match tokens {
        [TokenTree::Punct(sigil), rest @ ..] if matches!(sigil.as_char(), '%' | '?') => {
            dotted(rest)
        }
        _ => dotted(tokens),
    }
}

/// Identifiers joined by dots, such as `http.route`.
fn dotted(tokens: &[TokenTree]) -> Option<String> {
    let mut name = String::new();
    for (at, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Ident(ident) if at % 2 == 0 => name.push_str(&ident.to_string()),
            TokenTree::Punct(dot) if at % 2 == 1 && dot.as_char() == '.' => name.push('.'),
            _ => return None,
        }
    }
    (!name.is_empty() && !name.ends_with('.')).then_some(name)
}

fn last_ident(tokens: &[TokenTree]) -> Option<String> {
    tokens.iter().rev().find_map(|token| match token {
        TokenTree::Ident(ident) => Some(ident.to_string()),
        _ => None,
    })
}

/// A string literal's value, without escapes, which names never hold.
fn string(literal: &proc_macro2::Literal) -> Option<String> {
    let text = literal.to_string();
    text.strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST: &str = r#"
        pub const HTTP_ROUTE: &str = convention("http.route", otel::HTTP_ROUTE);
        pub const STARTUP_AREA: &str = "startup.area";
    "#;

    fn findings(path: &str, code: &str) -> Vec<String> {
        let repo = Repo::of(&[(NAMES, LIST), (path, code)]);
        check(&repo)
            .findings
            .into_iter()
            .map(|finding| finding.message)
            .collect()
    }

    const PLATFORM: &str = "crates/platform/src/http/stack.rs";
    const MODULE: &str = "crates/app/src/modules/workspaces/service.rs";

    #[test]
    fn obs_05_listed_names_pass_in_every_form() {
        let code = r#"
            fn f() {
                tracing::info!(http.route = %route, "request completed");
                tracing::info!({ names::HTTP_ROUTE } = route, "request completed");
                tracing::error!("startup.area" = area, "startup failed");
                let span = tracing::info_span!("request", { names::STARTUP_AREA } = Empty);
                span.record(names::HTTP_ROUTE, 1);
                span.record("startup.area", 1);
                tokio::select! { _ = a => tracing::warn!(?http.route, "stopped"), }
            }
        "#;
        assert_eq!(findings(PLATFORM, code), Vec::<String>::new());
    }

    #[test]
    fn obs_05_a_name_outside_the_list_fails() {
        let code = r#"
            fn f() {
                tokio::select! { _ = a => tracing::info!(area, "startup failed"), }
                tracing::info!(applied = ?applied, "migrations applied");
                tracing::info!({ names::UNLISTED } = 1, "x");
            }
        "#;
        assert_eq!(
            findings(PLATFORM, code),
            [
                "the field name area isn't in the list of telemetry names (OBS-05)",
                "the field name applied isn't in the list of telemetry names (OBS-05)",
                "a field name that isn't a name or one of the list's constants (OBS-05)",
            ]
        );
    }

    #[test]
    fn obs_05_record_names_come_from_the_list() {
        let code = r#"fn f() { span.record("route", 1); span.record(name, 1); }"#;
        assert_eq!(findings(PLATFORM, code).len(), 2);
    }

    #[test]
    fn obs_01_a_message_with_arguments_or_placeholders_fails() {
        let code = r#"
            fn f() {
                tracing::info!("listening on {}", address);
                tracing::info!("listening on {address}");
                tracing::info!("braces {{ are }} fine");
            }
        "#;
        assert_eq!(
            findings(PLATFORM, code),
            [
                "a message with placeholders isn't constant (OBS-01)",
                "a message with arguments isn't constant (OBS-01)",
                "a message with placeholders isn't constant (OBS-01)",
            ]
        );
    }

    #[test]
    fn obs_04_instrument_skips_every_argument() {
        let code = r#"
            #[tracing::instrument(skip_all, fields(http.route = %route))]
            fn kept() {}
            #[instrument(fields(http.route))]
            fn leaks() {}
        "#;
        assert_eq!(
            findings(PLATFORM, code),
            ["#[instrument] without skip_all (OBS-04)"]
        );
    }

    #[test]
    fn obs_03_module_code_writes_only_debug_events() {
        let code = r#"
            fn f() {
                tracing::debug!(http.route = %route, "loaded");
                tracing::info!("created");
                tracing::event!(Level::ERROR, "failed");
            }
        "#;
        assert_eq!(
            findings(MODULE, code),
            [
                "module code writes a INFO event; only DEBUG is allowed (OBS-03)",
                "module code writes a ERROR event; only DEBUG is allowed (OBS-03)",
            ]
        );
    }

    #[test]
    fn obs_06_module_code_opens_no_spans() {
        let code = r#"
            #[tracing::instrument(skip_all)]
            fn f() { let _span = tracing::debug_span!("work").entered(); }
        "#;
        assert_eq!(
            findings(MODULE, code),
            [
                "module code opens a span with #[instrument] (OBS-06)",
                "module code opens a span (OBS-06)",
            ]
        );
    }
}
