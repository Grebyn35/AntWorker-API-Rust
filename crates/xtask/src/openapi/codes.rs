//! ERR-13: every code that an `ErrorCode` of `platform` or `app` gives, as a string or as a
//! constant that holds one, is among the codes of the document's problem responses: an
//! operation's, or a shared one's, such as the 405 that answers the methods no operation
//! documents. The integration tests check each problem a test receives against the document
//! (API-17), so this finds the code that the document lists nowhere and no test receives.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use syn::{
    Expr, ExprLit, ExprPath, ImplItem, ItemConst, ItemImpl, ItemMod, Lit, LitStr,
    visit::{self, Visit},
};

use crate::{Finding, declarations::only_tests, repo::Repo};

/// The code the API runs, where errors implement `ErrorCode`.
const CODE: [&str; 2] = ["crates/platform/src/", "crates/app/src/"];

/// A finding for each code that an `ErrorCode`'s `code` returns, outside tests, and that no
/// problem of `document` lists.
pub fn check(repo: &Repo, document: &Value) -> Vec<Finding> {
    let mut listed = BTreeSet::new();
    listed_codes(document, &mut listed);
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
        let named = visitor
            .constants
            .into_iter()
            .filter_map(|(name, line)| Some((constants.strings.get(&name)?.clone(), line)));
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

/// Every string that a `code` property's `enum` lists, anywhere in `value`.
fn listed_codes(value: &Value, listed: &mut BTreeSet<String>) {
    match value {
        Value::Object(object) => {
            if let Some(values) = object
                .get("properties")
                .and_then(|properties| properties["code"]["enum"].as_array())
            {
                listed.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
            }
            for value in object.values() {
                listed_codes(value, listed);
            }
        }
        Value::Array(values) => {
            for value in values {
                listed_codes(value, listed);
            }
        }
        _ => {}
    }
}

/// The string literals in the `code` methods of a file's implementations of `ErrorCode`, and the
/// constants they name, with their lines.
#[derive(Default)]
struct Codes {
    codes: Vec<(String, usize)>,
    constants: Vec<(String, usize)>,
}

/// The string constants of the code, by name, outside the tests.
#[derive(Default)]
struct Constants {
    strings: BTreeMap<String, String>,
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
            self.strings.insert(item.ident.to_string(), text.value());
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
                let mut literals = Literals::default();
                literals.visit_block(&function.block);
                self.codes.extend(literals.found);
                self.constants.extend(literals.constants);
            }
        }
    }
}

#[derive(Default)]
struct Literals {
    found: Vec<(String, usize)>,
    constants: Vec<(String, usize)>,
}

impl<'ast> Visit<'ast> for Literals {
    fn visit_lit_str(&mut self, literal: &'ast LitStr) {
        self.found
            .push((literal.value(), literal.span().start().line));
    }

    /// A constant's name, such as `INTERNAL_CODE`: a path of one segment in capitals.
    fn visit_expr_path(&mut self, path: &'ast ExprPath) {
        if let Some(ident) = path.path.get_ident()
            && ident
                .to_string()
                .chars()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        {
            self.constants
                .push((ident.to_string(), ident.span().start().line));
        }
    }
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
}
