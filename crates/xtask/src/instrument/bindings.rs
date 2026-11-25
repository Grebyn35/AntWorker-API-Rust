//! What gives the name `names` a meaning in the API's code, for the two checks that read a path
//! `names::X` as the constant `X` of the list of telemetry names, which
//! `crates/platform/src/telemetry/names.rs` holds: `check-instrument`, as a field's or a
//! `record`'s name (OBS-05), and `check-metrics`, as a metric's label (OBS-11).
//!
//! In each Rust file under `crates/platform/src/` and `crates/app/src/`, among all its tokens, a
//! macro's and a test module's included, [`check`] refuses:
//! - the identifier `names`, raw or not, right after `mod`, `struct`, `enum`, `union`, `trait`,
//!   `type`, `crate` or `as`, but the `mod names;` at the top level of
//!   `crates/platform/src/telemetry/mod.rs`, which declares the list's module;
//! - a `use` that takes a name `names`, raw or not, by its own name, or takes a glob, as
//!   [`crate::rust::uses`] reads a `use`, by a path whose first segment is none of `crate`,
//!   `self`, `super`, a module that the file declares at its top level and, in a file of
//!   `crates/app/src/`, `platform`.

use proc_macro2::{TokenStream, TokenTree};

use crate::{Finding, rust};

/// The file that declares the list's module, `mod names;`, at its top level.
const DECLARING: &str = "crates/platform/src/telemetry/mod.rs";

/// The `app` crate, whose `use` may take `names` through `platform`.
const APP: &str = "crates/app/src/";

/// The keywords right after which an identifier is the name of what they declare or rename.
const NAMING: [&str; 8] = [
    "mod", "struct", "enum", "union", "trait", "type", "crate", "as",
];

/// The first segments of a path that starts in the file's own crate.
const OWN: [&str; 3] = ["crate", "self", "super"];

/// The finding of an item that [`NAMING`] names `names`.
const NAMED: &str = "`names` is the name of the module of the list of telemetry names alone: name \
                     this otherwise (OBS-05)";

/// The finding of a `use` that may take another `names`.
const TAKEN: &str = "this `use` takes `names`, or a glob, by a path that starts with none of \
                     `crate`, `self`, `super`, a module of the file and, in `app`, `platform`, \
                     while `names` is the name of the module of the list of telemetry names alone \
                     (OBS-05)";

/// A finding for each name and each `use` among `tokens`, those of the Rust file at `path`, that
/// the module's doc says the check refuses.
pub fn check(path: &str, tokens: TokenStream, findings: &mut Vec<Finding>) {
    let tokens: Vec<TokenTree> = tokens.into_iter().collect();
    named(path, &tokens, true, findings);
    let modules: Vec<String> = rust::declared(&tokens)
        .into_iter()
        .map(|(name, _, _)| unraw(&name).to_owned())
        .collect();
    for (import, line) in rust::uses(&tokens) {
        let takes = match &import.alias {
            None => true,
            Some(alias) => {
                unraw(alias) == "names"
                    && import
                        .path
                        .last()
                        .is_some_and(|last| unraw(last) == "names")
            }
        };
        let own = import
            .path
            .first()
            .map(|first| unraw(first))
            .is_some_and(|first| {
                OWN.contains(&first)
                    || modules.iter().any(|module| module == first)
                    || (path.starts_with(APP) && first == "platform")
            });
        if takes && !own {
            findings.push(Finding::new(path, line, TAKEN));
        }
    }
}

/// A finding for each identifier `names` among `tokens` right after one of [`NAMING`], and among
/// the tokens of each group they hold, `top` when `tokens` are the file's top level.
fn named(path: &str, tokens: &[TokenTree], top: bool, findings: &mut Vec<Finding>) {
    for (at, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Ident(name) if unraw(&name.to_string()) == "names" => {
                let Some(TokenTree::Ident(keyword)) =
                    at.checked_sub(1).and_then(|before| tokens.get(before))
                else {
                    continue;
                };
                let out_of_line = matches!(
                    tokens.get(at + 1),
                    Some(TokenTree::Punct(end)) if end.as_char() == ';'
                );
                let declares_the_list = top && path == DECLARING && keyword == "mod" && out_of_line;
                if NAMING.iter().any(|naming| keyword == *naming) && !declares_the_list {
                    findings.push(Finding::new(path, name.span().start().line, NAMED));
                }
            }
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                named(path, &inner, false, findings);
            }
            _ => {}
        }
    }
}

/// An identifier's name without the `r#` of a raw identifier.
fn unraw(ident: &str) -> &str {
    ident.strip_prefix("r#").unwrap_or(ident)
}

#[cfg(test)]
mod tests {
    use crate::repo::Repo;

    /// The line of each finding of `check-instrument` in the Rust file at `path`, which holds
    /// `code`, that names `names`, beside a list of telemetry names.
    fn lines(path: &str, code: &str) -> Vec<usize> {
        let list = r#"pub const HTTP_ROUTE: &str = convention("http.route", otel::HTTP_ROUTE);"#;
        let repo = Repo::of(&[(super::super::NAMES, list), (path, code)]);
        super::super::check(&repo)
            .findings
            .into_iter()
            .filter(|finding| finding.path == path && finding.message.contains("`names`"))
            .map(|finding| finding.line)
            .collect()
    }

    #[test]
    fn obs_05_names_is_the_name_of_the_list_s_module_alone() {
        let code = r#"mod names { pub const HTTP_ROUTE: &str = "user.email"; }
            pub(crate) mod r#names;
            macro_rules! m { () => { mod names {} }; }
            struct names;
            enum names {}
            union names { a: u8 }
            trait names {}
            type names = Other;
            extern crate names;
            extern crate other as names;
            use crate::other::Route as names;
            fn names() {}
            const names: u8 = 0;
            fn f(names: u8) { let names = names::HTTP_ROUTE; }"#;
        let declaring = "pub mod names;\nmod inner { mod names; }\nmod names {}";

        assert_eq!(
            [
                lines("crates/platform/src/http/stack.rs", code),
                lines("crates/platform/src/telemetry/mod.rs", declaring),
                lines("crates/platform/src/telemetry/relay.rs", "pub mod names;"),
            ],
            [(1..=11).collect::<Vec<usize>>(), vec![2, 3], vec![1],]
        );
    }

    #[test]
    fn obs_05_a_use_takes_names_from_its_own_crate_or_from_platform_alone() {
        let platform = "mod child;
            use crate::telemetry::names;
            use super::{names, Other};
            use self::child::names;
            use child::names;
            use crate::telemetry::{self, names::{self}};
            use other::names;
            use other::{Thing, names::{self}};
            use other::*;
            use platform::telemetry::names;
            use other::names as renamed;
            fn f() { use ::other::r#names; }
            macro_rules! m { () => { use $crate::telemetry::names; use $other::names; }; }";
        let app = "use platform::telemetry::names;
            use platform::telemetry::*;
            use opentelemetry::names;
            use super::*;";

        assert_eq!(
            [
                lines("crates/platform/src/http/stack.rs", platform),
                lines("crates/app/src/modules/auth/service.rs", app),
            ],
            [vec![7, 8, 9, 10, 12, 13], vec![3]]
        );
    }
}
