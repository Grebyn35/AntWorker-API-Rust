//! ERR-13: every code that the `code` method of an `ErrorCode` of `platform` or `app` returns,
//! outside tests, is among the codes of the document's problem responses: an operation's, or a
//! shared one's, such as the 405 that answers the methods no operation documents. The integration
//! tests check each problem a test receives against the document (API-17), so this finds the code
//! that the document lists nowhere and no test receives.
//!
//! The check reads the expression that the method's body is: a string; a name of one segment in
//! capitals, such as `INTERNAL_CODE`, whose codes are the strings that each `const` of that name
//! in `platform` or `app`, outside tests, is set to; a `match` whose arms each return one of
//! these forms; or a call of `code` without arguments, such as `error.code()`, whose codes the
//! check reads in that error's own `code`. It fails on any other form, such as `Self::LOST`,
//! `errors::LOST` or a body with a statement, and on a name that no such `const` sets, since it
//! can't read the code.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value};
use syn::{
    Expr, ExprLit, ExprPath, Ident, ImplItem, ItemConst, ItemImpl, ItemMod, Lit, Stmt,
    visit::{self, Visit},
};

use super::{METHODS, listed_codes};
use crate::{Finding, declarations::only_tests, repo::Repo};

/// The code the API runs, where errors implement `ErrorCode`.
const CODE: [&str; 2] = ["crates/platform/src/", "crates/app/src/"];

/// A finding for each code that an `ErrorCode`'s `code` returns, outside tests, and that no
/// problem of `document` lists.
pub fn check(repo: &Repo, document: &Value) -> Vec<Finding> {
    let listed = problem_codes(document);
    let files: Vec<(&str, syn::File)> = repo
        .files()
        .filter(|file| {
            CODE.iter().any(|root| file.path.starts_with(root)) && file.path.ends_with(".rs")
        })
        // check-docs and the compiler report a file that doesn't parse.
        .filter_map(|file| Some((file.path.as_str(), syn::parse_file(&file.text).ok()?)))
        .collect();
    let mut constants = Constants::default();
    for (_, parsed) in &files {
        constants.visit_file(parsed);
    }
    let mut findings = Vec::new();
    for (path, parsed) in &files {
        let mut visitor = Codes::default();
        visitor.visit_file(parsed);
        let mut named = Vec::new();
        for (name, line) in visitor.constants {
            match constants.strings.get(&name) {
                Some(codes) => named.extend(codes.iter().map(|code| (code.clone(), line))),
                None => findings.push(Finding::new(
                    path,
                    line,
                    format!(
                        "the check reads `{name}` from a `const` of `platform` or `app` that sets \
                         it to a string, and none does (ERR-13)"
                    ),
                )),
            }
        }
        for line in visitor.unread {
            findings.push(Finding::new(
                path,
                line,
                "the check can't read what this `code` returns: return a string, a constant \
                 that holds one, a `match` whose arms each return one, or another error's \
                 `.code()` (ERR-13)",
            ));
        }
        for (code, line) in visitor.codes.into_iter().chain(named) {
            if !listed.contains(&code) {
                findings.push(Finding::new(
                    path,
                    line,
                    format!(
                        "`{code}` is in no problem response of openapi.json: add the error that \
                         answers it to the responses of each route that can answer it, then run \
                         `just openapi` (ERR-13)"
                    ),
                ));
            }
        }
    }
    findings
}

/// The codes that the problem responses of `document` list: each response of an operation, and
/// each of `components/responses`, with the codes that the `allOf` of its
/// `application/problem+json` schema lists, as [`listed_codes`] reads them.
fn problem_codes(document: &Value) -> BTreeSet<String> {
    let empty = Map::new();
    let operations = document["paths"]
        .as_object()
        .unwrap_or(&empty)
        .values()
        .flat_map(|item| METHODS.iter().filter_map(|method| item.get(*method)));
    let responses = operations
        .flat_map(|operation| {
            operation["responses"]
                .as_object()
                .unwrap_or(&empty)
                .values()
        })
        .chain(
            document["components"]["responses"]
                .as_object()
                .unwrap_or(&empty)
                .values(),
        );
    responses
        .flat_map(listed_codes)
        .map(str::to_owned)
        .collect()
}

/// What the `code` methods of a file's implementations of `ErrorCode` return, as the module's
/// comment says the check reads it: the strings and the constants' names, with their lines, and
/// the line of each form that the check can't read.
#[derive(Default)]
struct Codes {
    codes: Vec<(String, usize)>,
    constants: Vec<(String, usize)>,
    unread: Vec<usize>,
}

/// The strings that the constants of the code are set to, by name, outside the tests.
#[derive(Default)]
struct Constants {
    strings: BTreeMap<String, Vec<String>>,
}

impl<'ast> Visit<'ast> for Constants {
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !only_tests(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_const(&mut self, item: &'ast ItemConst) {
        if let Expr::Lit(ExprLit {
            lit: Lit::Str(text),
            ..
        }) = item.expr.as_ref()
        {
            self.strings
                .entry(item.ident.to_string())
                .or_default()
                .push(text.value());
        }
    }
}

impl<'ast> Visit<'ast> for Codes {
    /// Skips a module that only tests compile, whose errors no route answers.
    fn visit_item_mod(&mut self, item: &'ast ItemMod) {
        if !only_tests(&item.attrs) {
            visit::visit_item_mod(self, item);
        }
    }

    fn visit_item_impl(&mut self, item: &'ast ItemImpl) {
        let implements = item.trait_.as_ref().is_some_and(|(path, _)| {
            path.segments
                .last()
                .is_some_and(|segment| segment.ident == "ErrorCode")
        });
        if !implements {
            return;
        }
        for item in &item.items {
            if let ImplItem::Fn(function) = item
                && function.sig.ident == "code"
            {
                let line = function.sig.ident.span().start().line;
                match function.block.stmts.as_slice() {
                    [Stmt::Expr(returned, None)] => self.returned(returned, line),
                    _ => self.unread.push(line),
                }
            }
        }
    }
}

impl Codes {
    /// Reads `returned`, what a `code` method returns, in the forms that the module's comment
    /// lists, and names `line` for a form that it can't read.
    fn returned(&mut self, returned: &Expr, line: usize) {
        match returned {
            Expr::Lit(ExprLit {
                lit: Lit::Str(text),
                ..
            }) => self.codes.push((text.value(), text.span().start().line)),
            Expr::Match(matched) => {
                for arm in &matched.arms {
                    self.returned(&arm.body, arm.fat_arrow_token.spans[0].start().line);
                }
            }
            Expr::MethodCall(call) if call.method == "code" && call.args.is_empty() => {}
            other => match constant(other) {
                Some(name) => self
                    .constants
                    .push((name.to_string(), name.span().start().line)),
                None => self.unread.push(line),
            },
        }
    }
}

/// The name that `expr` is, where it is a constant's as the module's comment reads one: a path
/// of one segment in capitals.
fn constant(expr: &Expr) -> Option<&Ident> {
    let Expr::Path(ExprPath {
        qself: None, path, ..
    }) = expr
    else {
        return None;
    };
    path.get_ident().filter(|name| {
        name.to_string()
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn err_13_a_code_that_no_problem_of_the_document_lists_fails() {
        let error = "impl ErrorCode for ThingsError {\n\
                     \x20   fn code(&self) -> &'static str {\n\
                     \x20       match self {\n\
                     \x20           Self::Gone => \"things.gone\",\n\
                     \x20           Self::Taken => \"things.taken\",\n\
                     \x20           Self::Lost => LOST,\n\
                     \x20           Self::Moved => MOVED,\n\
                     \x20       }\n\
                     \x20   }\n\
                     \x20   fn title(&self) -> &'static str { \"Not a code\" }\n\
                     }\n\
                     const LOST: &str = \"things.lost\";\n\
                     #[cfg(test)]\n\
                     mod tests {\n\
                     \x20   impl ErrorCode for Fake { fn code(&self) -> &'static str { \"fake.only\" } }\n\
                     }\n";
        let repo = Repo::of(&[
            ("crates/app/src/modules/things/error.rs", error),
            (
                "crates/platform/src/errors/mod.rs",
                "pub const MOVED: &str = \"request.moved\";\n",
            ),
            (
                "crates/app/tests/integration/support/mod.rs",
                "impl ErrorCode for Test { fn code(&self) -> &'static str { \"test.only\" } }",
            ),
        ]);
        let document = json!({
            "paths": { "/v1/things": { "get": { "responses": { "404": {
                "content": { "application/problem+json": { "schema": { "allOf": [
                    { "$ref": "#/components/schemas/ProblemBody" },
                    { "properties": { "code": { "enum": ["things.gone"] } } },
                ] } } },
            } } } } },
            "components": { "responses": { "Moved": {
                "content": { "application/problem+json": { "schema": { "allOf": [
                    { "$ref": "#/components/schemas/ProblemBody" },
                    { "properties": { "code": { "enum": ["request.moved"] } } },
                ] } } },
            } } },
        });

        let messages: Vec<String> = check(&repo, &document)
            .into_iter()
            .map(|finding| format!("{}:{}", finding.path, finding.line))
            .collect();

        assert_eq!(
            messages,
            [
                "crates/app/src/modules/things/error.rs:5",
                "crates/app/src/modules/things/error.rs:6"
            ]
        );
    }

    #[test]
    fn err_13_a_code_in_a_form_that_the_check_can_t_read_fails() {
        let error = "impl ErrorCode for ThingsError {\n\
                     \x20   fn code(&self) -> &'static str {\n\
                     \x20       match self {\n\
                     \x20           Self::Gone => \"things.gone\",\n\
                     \x20           Self::Inner(error) => error.code(),\n\
                     \x20           Self::Lost => Self::LOST,\n\
                     \x20           Self::Moved => errors::MOVED,\n\
                     \x20           Self::Named => named(),\n\
                     \x20           Self::Unset => UNSET,\n\
                     \x20       }\n\
                     \x20   }\n\
                     }\n\
                     impl ErrorCode for OtherError {\n\
                     \x20   fn code(&self) -> &'static str {\n\
                     \x20       let code = \"things.gone\";\n\
                     \x20       code\n\
                     \x20   }\n\
                     }\n";
        let repo = Repo::of(&[("crates/app/src/modules/things/error.rs", error)]);
        let document = json!({
            "paths": { "/v1/things": { "get": { "responses": { "404": {
                "content": { "application/problem+json": { "schema": { "allOf": [
                    { "$ref": "#/components/schemas/ProblemBody" },
                    { "properties": { "code": { "enum": ["things.gone"] } } },
                ] } } },
            } } } } },
        });

        let messages: Vec<String> = check(&repo, &document)
            .into_iter()
            .map(|finding| format!("{}:{}", finding.path, finding.line))
            .collect();

        assert_eq!(
            messages,
            [
                "crates/app/src/modules/things/error.rs:9",
                "crates/app/src/modules/things/error.rs:6",
                "crates/app/src/modules/things/error.rs:7",
                "crates/app/src/modules/things/error.rs:8",
                "crates/app/src/modules/things/error.rs:14",
            ]
        );
    }

    #[test]
    fn err_13_a_constant_s_codes_are_the_strings_of_each_const_of_its_name() {
        let error = "impl ErrorCode for ThingsError {\n\
                     \x20   fn code(&self) -> &'static str {\n\
                     \x20       LOST\n\
                     \x20   }\n\
                     }\n\
                     mod a { const LOST: &str = \"things.lost\"; }\n\
                     mod b { const LOST: &str = \"things.gone\"; }\n";
        let repo = Repo::of(&[("crates/app/src/modules/things/error.rs", error)]);
        let document = json!({
            "paths": { "/v1/things": { "get": { "responses": { "404": {
                "content": { "application/problem+json": { "schema": { "allOf": [
                    { "$ref": "#/components/schemas/ProblemBody" },
                    { "properties": { "code": { "enum": ["things.gone"] } } },
                ] } } },
            } } } } },
        });

        let messages: Vec<String> = check(&repo, &document)
            .into_iter()
            .map(|finding| finding.message)
            .collect();

        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(messages[0].starts_with("`things.lost` is in no problem response"));
    }

    #[test]
    fn err_13_a_code_that_only_a_schema_other_than_a_problem_s_lists_fails() {
        let error = "impl ErrorCode for ThingsError {\n\
                     \x20   fn code(&self) -> &'static str {\n\
                     \x20       match self {\n\
                     \x20           Self::Gone => \"things.gone\",\n\
                     \x20           Self::Taken => \"things.taken\",\n\
                     \x20       }\n\
                     \x20   }\n\
                     }\n";
        let repo = Repo::of(&[("crates/app/src/modules/things/error.rs", error)]);
        let document = json!({
            "paths": { "/v1/things": { "get": { "responses": {
                "200": { "content": { "application/json": { "schema": {
                    "properties": { "code": { "enum": ["things.taken"] } },
                } } } },
                "404": { "content": { "application/problem+json": { "schema": { "allOf": [
                    { "$ref": "#/components/schemas/ProblemBody" },
                    { "properties": { "code": { "enum": ["things.gone"] } } },
                ] } } } },
            } } } },
            "components": { "schemas": { "Outcome": {
                "properties": { "code": { "enum": ["things.taken"] } },
            } } },
        });

        let messages: Vec<String> = check(&repo, &document)
            .into_iter()
            .map(|finding| format!("{}:{}", finding.path, finding.line))
            .collect();

        assert_eq!(messages, ["crates/app/src/modules/things/error.rs:5"]);
    }
}
