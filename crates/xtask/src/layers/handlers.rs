//! ARCH-08's reading of a module's routes: the handlers that its `routes!` register, and the
//! calls of each free function, as [`limits_counted`] says.

use std::collections::{BTreeMap, BTreeSet};

use syn::{
    Expr, FnArg, Pat, PatType, Type,
    visit::{self, Visit},
};

use crate::{
    Finding,
    repo::{File, Repo},
    rust,
};

/// The names of each module's handlers: the functions that a `routes!` of its routes registers.
pub(super) fn handlers(repo: &Repo) -> BTreeMap<String, BTreeSet<String>> {
    let mut handlers: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for file in repo.files() {
        let Some((module, parts)) = rust::feature_module(&file.path) else {
            continue;
        };
        if parts.first() != Some(&"routes") {
            continue;
        }
        let Ok(parsed) = syn::parse_file(&file.text) else {
            continue;
        };
        let mut registered = Registered::default();
        registered.visit_file(&parsed);
        handlers
            .entry(module.to_owned())
            .or_default()
            .extend(registered.names);
    }
    handlers
}

/// The functions that the `routes!` macros of a file register.
#[derive(Default)]
struct Registered {
    names: BTreeSet<String>,
}

impl<'ast> Visit<'ast> for Registered {
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if mac
            .path
            .segments
            .last()
            .is_some_and(|last| last.ident == "routes")
        {
            self.names.extend(
                mac.tokens
                    .clone()
                    .into_iter()
                    .filter_map(|token| match token {
                        proc_macro2::TokenTree::Ident(ident) => Some(ident.to_string()),
                        _ => None,
                    }),
            );
        }
        visit::visit_macro(self, mac);
    }
}

/// ARCH-08: a handler calls `check` on no parameter of type `RateLimits`, since the service
/// counts a per-action limit (the platform spec's Unit of work), and calls at most one service
/// function; a function that no `routes!` of the module registers, of a module that registers
/// any, calls none; and nothing opens a unit of work. It reads the file's free `fn` items as syn
/// parses them, outside macros' tokens. The compiler reports a file that doesn't parse.
pub(super) fn limits_counted(file: &File, handlers: &BTreeSet<String>) -> Vec<Finding> {
    let Ok(parsed) = syn::parse_file(&file.text) else {
        return Vec::new();
    };
    let mut visitor = Handlers {
        path: &file.path,
        handlers,
        findings: Vec::new(),
    };
    visitor.visit_file(&parsed);
    visitor.findings
}

struct Handlers<'a> {
    path: &'a str,
    handlers: &'a BTreeSet<String>,
    findings: Vec<Finding>,
}

impl<'ast> Visit<'ast> for Handlers<'_> {
    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        let limits = rate_limits(item);
        let states = states(item);
        let mut calls = Calls {
            limits: &limits,
            states: &states,
            checks: Vec::new(),
            services: Vec::new(),
            begins: Vec::new(),
        };
        calls.visit_block(&item.block);
        for line in &calls.begins {
            self.findings.push(Finding::new(
                self.path,
                *line,
                "a module's routes open a unit of work: the service opens it, or `Authorize` on \
                 a workspace route (ARCH-08)",
            ));
        }
        let registered = self.handlers.contains(&item.sig.ident.to_string());
        if !self.handlers.is_empty() && !registered {
            for line in calls.services {
                self.findings.push(Finding::new(
                    self.path,
                    line,
                    format!(
                        "`{}`, which no route registers, calls the service: a handler calls its \
                         one service function itself (ARCH-08)",
                        item.sig.ident
                    ),
                ));
            }
            return;
        }
        for line in calls.checks {
            self.findings.push(Finding::new(
                self.path,
                line,
                "a handler counts a per-action limit: it hands its `RateLimits` to the service, \
                 which counts it (ARCH-08)",
            ));
        }
        for line in calls.services.into_iter().skip(1) {
            self.findings.push(Finding::new(
                self.path,
                line,
                "a handler calls a second service function: it calls one (ARCH-08)",
            ));
        }
    }
}

/// The names of `item`'s parameters whose type's last segment is `RateLimits`.
fn rate_limits(item: &syn::ItemFn) -> Vec<String> {
    item.sig
        .inputs
        .iter()
        .filter_map(|input| match input {
            FnArg::Typed(PatType { pat, ty, .. }) => match (&**pat, &**ty) {
                (Pat::Ident(named), Type::Path(path))
                    if path
                        .path
                        .segments
                        .last()
                        .is_some_and(|last| last.ident == "RateLimits") =>
                {
                    Some(named.ident.to_string())
                }
                _ => None,
            },
            FnArg::Receiver(_) => None,
        })
        .collect()
}

/// The names that `item`'s `State` parameters bind: `name` in `State(name)`, and each name that
/// is a field's whole pattern, as `name` and `renamed` are in
/// `State(Routes { name, other: renamed })`.
fn states(item: &syn::ItemFn) -> Vec<String> {
    let mut names = Vec::new();
    for input in &item.sig.inputs {
        let FnArg::Typed(PatType { pat, .. }) = input else {
            continue;
        };
        let Pat::TupleStruct(state) = &**pat else {
            continue;
        };
        if !state
            .path
            .segments
            .last()
            .is_some_and(|last| last.ident == "State")
        {
            continue;
        }
        match state.elems.first() {
            Some(Pat::Ident(name)) => names.push(name.ident.to_string()),
            Some(Pat::Struct(fields)) => {
                names.extend(fields.fields.iter().filter_map(|field| match &*field.pat {
                    Pat::Ident(name) => Some(name.ident.to_string()),
                    _ => None,
                }))
            }
            _ => {}
        }
    }
    names
}

/// The lines where a block calls `check` on one of `limits`, those where it calls a service
/// function: a method of one of `states`, or a function of `service`, and those where it calls
/// `begin`, which opens a unit of work.
struct Calls<'a> {
    limits: &'a [String],
    states: &'a [String],
    checks: Vec<usize>,
    services: Vec<usize>,
    begins: Vec<usize>,
}

/// Whether `expr` is the bare name of one of `names`.
fn names_one_of(expr: &Expr, names: &[String]) -> bool {
    matches!(expr, Expr::Path(path) if path
        .path
        .get_ident()
        .is_some_and(|ident| names.iter().any(|name| ident == name)))
}

impl<'ast> Visit<'ast> for Calls<'_> {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        let line = call.method.span().start().line;
        if call.method == "check" && names_one_of(&call.receiver, self.limits) {
            self.checks.push(line);
        }
        if names_one_of(&call.receiver, self.states) {
            self.services.push(line);
        }
        if call.method == "begin" {
            self.begins.push(line);
        }
        visit::visit_expr_method_call(self, call);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let Expr::Path(function) = &*call.func {
            let segments = &function.path.segments;
            let line = segments[0].ident.span().start().line;
            if segments.len() > 1 && segments[0].ident == "service" {
                self.services.push(line);
            }
            if segments.last().is_some_and(|last| last.ident == "begin") {
                self.begins.push(line);
            }
        }
        visit::visit_expr_call(self, call);
    }
}
