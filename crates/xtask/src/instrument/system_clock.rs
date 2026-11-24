//! `check-instrument`, for CODE-06: the identifier `SystemClock`, the name of the system clock,
//! stands in no Rust file of `crates/` but the two that may name it:
//! `crates/platform/src/clock.rs`, which defines it, and `crates/app/src/main.rs`, the binary's,
//! which hands it to what it builds. Clippy's ban of the type doesn't reach the unit struct's
//! value, which an expression such as `Arc::new(SystemClock)` builds.
//!
//! It reads the identifiers among a file's tokens, those inside a macro's body too, raw, as
//! `r#SystemClock`, or not, and nothing of a comment or a string. A file whose tokens don't lex
//! fails `check-instrument` already. A name that one of the two files gives the system clock,
//! through `use ... as` or a type alias, lets other files name it otherwise, which it doesn't
//! read.

use proc_macro2::{TokenStream, TokenTree};

use crate::Finding;

/// The system clock's name.
const NAME: &str = "SystemClock";

/// The files that may name it (CODE-06).
const NAMING: [&str; 2] = ["crates/platform/src/clock.rs", "crates/app/src/main.rs"];

/// A finding for each identifier `SystemClock` among `tokens`, those of the Rust file at `path`,
/// when the file isn't one of those that may name it.
pub fn check(path: &str, tokens: TokenStream, findings: &mut Vec<Finding>) {
    if NAMING.contains(&path) {
        return;
    }
    for line in lines(tokens) {
        findings.push(Finding::new(
            path,
            line,
            "the system clock, `SystemClock`, outside platform's clock.rs and the binary's \
             main.rs, which alone hands it: wait on the clock you're given, a test clock in a \
             test (CODE-06)",
        ));
    }
}

/// The line of each identifier `SystemClock`, raw or not, among `tokens`, and the groups they
/// hold.
fn lines(tokens: TokenStream) -> Vec<usize> {
    let mut found = Vec::new();
    for token in tokens {
        match token {
            TokenTree::Ident(ident) => {
                let name = ident.to_string();
                if name.strip_prefix("r#").unwrap_or(&name) == NAME {
                    found.push(ident.span().start().line);
                }
            }
            TokenTree::Group(group) => found.extend(lines(group.stream())),
            TokenTree::Punct(_) | TokenTree::Literal(_) => {}
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use crate::repo::Repo;

    /// The findings of `check-instrument` in the Rust file at `path`, which holds `code`, beside
    /// an empty list of telemetry names.
    fn found(path: &str, code: &str) -> Vec<String> {
        let repo = Repo::of(&[(super::super::NAMES, ""), (path, code)]);
        super::super::check(&repo)
            .findings
            .into_iter()
            .map(|finding| format!("{}:{}: {}", finding.path, finding.line, finding.message))
            .collect()
    }

    /// The line of each finding of `check-instrument` in the Rust file at `path`, which holds
    /// `code`.
    fn lines(path: &str, code: &str) -> Vec<usize> {
        found(path, code)
            .iter()
            .filter_map(|finding| finding.split(':').nth(1)?.parse().ok())
            .collect()
    }

    #[test]
    fn code_06_the_system_clock_is_named_only_where_it_is_defined_and_handed() {
        let code = "use platform::clock::SystemClock;\n\
                    fn f() { let _clock = std::sync::Arc::new(SystemClock); }\n";

        for path in ["crates/platform/src/clock.rs", "crates/app/src/main.rs"] {
            assert_eq!(found(path, code), Vec::<String>::new(), "{path}");
        }
        assert_eq!(
            found("crates/platform/src/outbound/mod.rs", code)[1],
            "crates/platform/src/outbound/mod.rs:2: the system clock, `SystemClock`, outside \
             platform's clock.rs and the binary's main.rs, which alone hands it: wait on the \
             clock you're given, a test clock in a test (CODE-06)"
        );
        for path in [
            "crates/platform/src/outbound/mod.rs",
            "crates/platform/src/telemetry/clock.rs",
            "crates/app/src/modules/system/main.rs",
            "crates/app/tests/integration/main.rs",
            "crates/xtask/src/main.rs",
        ] {
            assert_eq!(lines(path, code), [1, 2], "{path}");
        }
    }

    #[test]
    fn code_06_the_system_clock_is_found_raw_renamed_and_inside_a_macro() {
        let code = "fn f() {\n\
                    \x20   let _clock = Arc::new(r#SystemClock);\n\
                    }\n\
                    use platform::clock::{SystemClock as Wall};\n\
                    macro_rules! clock {\n\
                    \x20   () => { crate::clock::SystemClock };\n\
                    }\n\
                    fn g() { tokio::select! { _ = a => Arc::new(crate::clock::SystemClock) } }\n";

        assert_eq!(
            lines("crates/platform/src/outbound/mod.rs", code),
            [2, 4, 6, 8]
        );
    }

    #[test]
    fn code_06_a_comment_a_string_or_another_name_is_no_finding() {
        let code = "// SystemClock\n\
                    /// The `SystemClock`.\n\
                    const NAME: &str = \"SystemClock\";\n\
                    struct SystemClocks;\n\
                    fn system_clock() {}\n";

        assert_eq!(
            found("crates/platform/src/outbound/mod.rs", code),
            Vec::<String>::new()
        );
    }

    #[test]
    fn code_06_a_file_whose_tokens_don_t_lex_fails_the_check() {
        let code = "fn f() {\n    let _clock = Arc::new(SystemClock);\n";

        assert_eq!(lines("crates/platform/src/outbound/mod.rs", code), [1]);
    }
}
