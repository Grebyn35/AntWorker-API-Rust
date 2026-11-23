//! TEST-06's count of polls: no `for` loop of a Rust file under a crate's `tests/` calls a
//! function named `yield_now`, such as `tokio::task::yield_now`, in its body outside the loops
//! that its body holds, since a loop over a range of polls bounds a wait by a count, while a
//! loop over cases may hold a wait of its own. The check reads the loops and the calls that `syn`
//! parses, outside a macro's tokens; a count kept otherwise, such as in a `while`, is review's.

use syn::visit::{self, Visit};

use crate::{Finding, repo::Repo};

/// A finding for each `for` loop of a test file whose body calls `yield_now`.
pub fn check(repo: &Repo) -> Vec<Finding> {
    let mut findings = Vec::new();
    for file in repo
        .files()
        .filter(|file| file.path.ends_with(".rs") && file.path.split('/').nth(2) == Some("tests"))
    {
        // `check-test-map` reports a file that doesn't parse.
        let Ok(syntax) = syn::parse_file(&file.text) else {
            continue;
        };
        let mut loops = Loops(Vec::new());
        loops.visit_file(&syntax);
        findings.extend(loops.0.into_iter().map(|line| {
            Finding::new(
                &file.path,
                line,
                "a `for` loop yields between polls, so a count bounds the wait: wait for the \
                 state with no bound of the test's own (TEST-06)",
            )
        }));
    }
    findings
}

/// The lines of the `for` loops whose bodies call `yield_now`.
struct Loops(Vec<usize>);

impl<'ast> Visit<'ast> for Loops {
    fn visit_expr_for_loop(&mut self, looped: &'ast syn::ExprForLoop) {
        let mut yields = Yields(false);
        yields.visit_block(&looped.body);
        if yields.0 {
            self.0.push(looped.for_token.span.start().line);
        }
        visit::visit_expr_for_loop(self, looped);
    }
}

/// Whether a path whose last segment is `yield_now` is called, outside a loop.
struct Yields(bool);

impl<'ast> Visit<'ast> for Yields {
    fn visit_expr_for_loop(&mut self, _: &'ast syn::ExprForLoop) {}

    fn visit_expr_while(&mut self, _: &'ast syn::ExprWhile) {}

    fn visit_expr_loop(&mut self, _: &'ast syn::ExprLoop) {}

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(function) = &*call.func {
            self.0 |= function
                .path
                .segments
                .last()
                .is_some_and(|last| last.ident == "yield_now");
        }
        visit::visit_expr_call(self, call);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_06_a_for_loop_that_yields_between_polls_fails() {
        let code = "async fn waits() {\n\
                    \x20   for _ in 0..100 {\n\
                    \x20       tokio::task::yield_now().await;\n\
                    \x20   }\n\
                    \x20   while !done() {\n\
                    \x20       tokio::task::yield_now().await;\n\
                    \x20   }\n\
                    \x20   for row in rows {\n\
                    \x20       check(row);\n\
                    \x20   }\n\
                    \x20   for _ in 0..3 { std::thread::yield_now(); }\n\
                    \x20   for case in cases {\n\
                    \x20       while !reached(case) { tokio::task::yield_now().await; }\n\
                    \x20   }\n\
                    }\n";
        let repo = Repo::of(&[
            ("crates/app/tests/integration/waits.rs", code),
            ("crates/platform/src/clock.rs", code),
        ]);

        let lines: Vec<String> = check(&repo)
            .iter()
            .map(|finding| format!("{}:{}", finding.path, finding.line))
            .collect();

        assert_eq!(
            lines,
            [
                "crates/app/tests/integration/waits.rs:2",
                "crates/app/tests/integration/waits.rs:11",
            ]
        );
    }
}
