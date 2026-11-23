//! TEST-09's helper written once: no two Rust files whose paths start with `crates/app/tests/`
//! each define a function of the same body. A function is a `fn` item that `syn` parses, outside
//! a macro's tokens, an `impl` block and a trait, that carries no attribute whose path's last
//! segment is `test`, such as `#[tokio::test]`. Its body is the tokens between its braces, as
//! `proc-macro2` prints them, so its whitespace and its comments other than doc comments are left
//! out. Two functions of one body pass when the doc comment of each holds `TEST-09`, as the copy
//! that each of two parts' files keeps says it is one. A body that differs in a name, such as a
//! local's, and a helper's steps that a test writes inline, are review's.

use std::collections::BTreeMap;

use proc_macro2::{LineColumn, TokenStream};
use syn::{
    Attribute, Expr, ExprLit, ItemFn, ItemImpl, ItemTrait, Lit, Meta,
    visit::{self, Visit},
};

use crate::{Finding, repo::Repo};

/// Where the functions that `check` reads stand.
const TESTS: &str = "crates/app/tests/";

/// A function of a file of the tests.
struct Helper {
    path: String,
    line: usize,
    name: String,
    /// Whether its doc comment holds `TEST-09`.
    kept: bool,
}

/// A finding for each function whose body a function of another file holds too.
pub(super) fn check(repo: &Repo) -> Vec<Finding> {
    let mut bodies: BTreeMap<String, Vec<Helper>> = BTreeMap::new();
    for file in repo
        .files()
        .filter(|file| file.path.starts_with(TESTS) && file.path.ends_with(".rs"))
    {
        // `check-test-map` reports a file that doesn't parse.
        let Ok(syntax) = syn::parse_file(&file.text) else {
            continue;
        };
        let mut helpers = Helpers(&file.text, Vec::new());
        helpers.visit_file(&syntax);
        for (body, line, name, kept) in helpers.1 {
            let path = file.path.clone();
            let helper = Helper {
                path,
                line,
                name,
                kept,
            };
            bodies.entry(body).or_default().push(helper);
        }
    }
    let mut findings = Vec::new();
    for helpers in bodies.values() {
        for helper in helpers {
            let other = helpers
                .iter()
                .find(|other| other.path != helper.path && !(other.kept && helper.kept));
            if let Some(other) = other {
                let message = format!(
                    "`{}` has the body of `{}` at {}:{}: write the helper once, in the test \
                     support or in one of the files, which the others import (TEST-09)",
                    helper.name, other.name, other.path, other.line
                );
                findings.push(Finding::new(&helper.path, helper.line, message));
            }
        }
    }
    findings
}

/// The functions of a file's text, each as its body, its line, its name and whether its doc
/// comment holds `TEST-09`.
struct Helpers<'a>(&'a str, Vec<(String, usize, String, bool)>);

impl<'ast> Visit<'ast> for Helpers<'_> {
    fn visit_item_impl(&mut self, _: &'ast ItemImpl) {}

    fn visit_item_trait(&mut self, _: &'ast ItemTrait) {}

    fn visit_item_fn(&mut self, item: &'ast ItemFn) {
        let braces = item.block.brace_token.span;
        let (start, end) = (braces.open().end(), braces.close().start());
        let body = &self.0[offset(self.0, start)..offset(self.0, end)];
        if !item.attrs.iter().any(super::is_test)
            && let Ok(tokens) = body.parse::<TokenStream>()
        {
            let name = &item.sig.ident;
            let kept = item.attrs.iter().any(holds_test_09);
            let line = name.span().start().line;
            self.1
                .push((tokens.to_string(), line, name.to_string(), kept));
        }
        visit::visit_item_fn(self, item);
    }
}

/// Whether `attribute` is a doc comment that holds `TEST-09`.
fn holds_test_09(attribute: &Attribute) -> bool {
    matches!(&attribute.meta, Meta::NameValue(pair) if pair.path.is_ident("doc")
        && matches!(&pair.value, Expr::Lit(ExprLit { lit: Lit::Str(text), .. })
            if text.value().contains("TEST-09")))
}

/// The byte offset in `text` of `at`, a line counted from 1 and a column counted in characters
/// from 0.
fn offset(text: &str, at: LineColumn) -> usize {
    let start: usize = text
        .split_inclusive('\n')
        .take(at.line - 1)
        .map(str::len)
        .sum();
    let column: usize = text[start..]
        .chars()
        .take(at.column)
        .map(char::len_utf8)
        .sum();
    start + column
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_09_a_function_whose_body_another_file_s_function_holds_fails() {
        let first = "/// Reads.\n\
                     async fn read(api: &Api) -> u16 {\n\
                     \x20   api.get(\"/x\").send().await.status // its status\n\
                     }\n\
                     impl Api { fn read(api: &Api) -> u16 { api.get(\"/x\").send().await.status } }\n\
                     #[tokio::test(flavor = \"multi_thread\")]\n\
                     async fn t() -> u16 { api.get(\"/x\").send().await.status }\n\
                     /// A part's copy (TEST-09).\n\
                     fn kept() { one(); }\n\
                     /// A part's copy (TEST-09).\n\
                     fn half() { two(); }\n\
                     fn other() { api.get(\"/y\").send().await.status }\n";
        let second = "mod inner {\n\
                      \x20   pub(crate) async fn fetch(api: &Api) -> u16 {\n\
                      \x20       api.get(\"/x\")\n\
                      \x20           .send() /* sent */ .await.status\n\
                      \x20   }\n\
                      }\n\
                      /// The other part's (TEST-09).\n\
                      fn kept() { one(); }\n\
                      fn half() { two(); }\n";
        let third = "#[must_use]\nfn status() -> u16 { api.get(\"/x\").send().await.status }\n";
        let repo = Repo::of(&[
            ("crates/app/tests/integration/first.rs", first),
            ("crates/app/tests/integration/second.rs", second),
            ("crates/app/tests/e2e/third.rs", third),
            ("crates/platform/src/fourth.rs", third),
        ]);

        let found: Vec<String> = check(&repo)
            .iter()
            .map(|finding| format!("{}:{}", finding.path, finding.line))
            .collect();

        assert_eq!(
            found,
            [
                "crates/app/tests/integration/first.rs:2",
                "crates/app/tests/integration/second.rs:2",
                "crates/app/tests/e2e/third.rs:2",
                "crates/app/tests/integration/first.rs:11",
                "crates/app/tests/integration/second.rs:9",
            ]
        );
    }

    #[test]
    fn test_09_a_function_inside_another_or_a_test_module_is_read_and_one_file_may_repeat_it() {
        let first = "fn twice() { one(); }\n\
                     fn again() { one(); }\n\
                     /* \u{e9} */ fn after() { two(); }\n";
        let second = "async fn outer() {\n\
                      \x20   fn inner() { two(); }\n\
                      }\n\
                      #[cfg(test)]\n\
                      mod tests {\n\
                      \x20   fn helper() { three(); }\n\
                      }\n";
        let third = "fn helper() { three(); }\n";
        let repo = Repo::of(&[
            ("crates/app/tests/integration/first.rs", first),
            ("crates/app/tests/integration/second.rs", second),
            ("crates/app/tests/e2e/third.rs", third),
        ]);

        let found: Vec<String> = check(&repo)
            .iter()
            .map(|finding| format!("{}:{}", finding.path, finding.line))
            .collect();

        assert_eq!(
            found,
            [
                "crates/app/tests/integration/second.rs:6",
                "crates/app/tests/e2e/third.rs:1",
                "crates/app/tests/integration/first.rs:3",
                "crates/app/tests/integration/second.rs:2",
            ]
        );
    }
}
