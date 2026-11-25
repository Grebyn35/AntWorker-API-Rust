//! `check-instrument`, for CODE-03: an `#[expect]` names a print lint, or the group that holds
//! them, only in the places where printing is allowed, whether it is written alone or inside a
//! `cfg_attr`, in every Rust file under `crates/`.

use proc_macro2::TokenTree;

use super::{MAIN, Scan, segments};

/// Where an exception to a print lint is allowed, with the lints it may name (CODE-03): the
/// binary's startup line before logging exists, on stderr, and the accounts `seed` reports, on
/// stdout; and the output of the checks and the other tasks.
const PRINTING: [(&str, &[&str]); 2] = [
    (MAIN, &["print_stdout", "print_stderr"]),
    ("crates/xtask/src/", &["print_stdout", "print_stderr"]),
];

/// The print lints.
const PRINT_LINTS: [&str; 2] = ["print_stdout", "print_stderr"];

/// The lint group that holds the print lints: an exception to it excepts them both.
const PRINT_GROUP: &str = "restriction";

impl Scan<'_> {
    /// An `expect` attribute that names `print_stdout` or `print_stderr` outside the places
    /// CODE-03 allows them, whether written alone or inside a `cfg_attr`.
    pub(super) fn printing(&mut self, tokens: &[TokenTree], line: usize) {
        let (Some(TokenTree::Ident(kind)), Some(TokenTree::Group(lints))) =
            (tokens.first(), tokens.get(1))
        else {
            return;
        };
        if kind == "cfg_attr" {
            // `cfg_attr(condition, attribute, ...)`: the attributes follow the condition.
            for attribute in segments(lints.stream()).iter().skip(1) {
                self.printing(attribute, line);
            }
            return;
        }
        if kind != "expect" {
            return;
        }
        let allowed: &[&str] = PRINTING
            .iter()
            .find(|(place, _)| self.path.starts_with(place))
            .map_or(&[], |(_, lints)| lints);
        for token in lints.stream() {
            if let TokenTree::Ident(lint) = token {
                let lint = lint.to_string();
                if lint.starts_with("print_") && !allowed.contains(&lint.as_str()) {
                    self.find(
                        line,
                        format!("an exception to `{lint}` outside the places CODE-03 allows printing (CODE-03)"),
                    );
                } else if lint == PRINT_GROUP
                    && let Some(excepted) =
                        PRINT_LINTS.iter().find(|print| !allowed.contains(print))
                {
                    self.find(
                        line,
                        format!("an exception to `{lint}`, the group that holds `{excepted}`, outside the places CODE-03 allows printing (CODE-03)"),
                    );
                }
            }
        }
    }
}
