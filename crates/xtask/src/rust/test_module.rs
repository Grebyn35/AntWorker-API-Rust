//! Where a Rust file's test module stands, for the checks that read a file's lines before its
//! tests: `check-layout`, which counts them (CODE-08), `check-openapi`, which reads schema names
//! in them (API-17), and `check-docs`, which reads the IDs that a file of `crates/xtask/` cites
//! in them (DOC-12).
//!
//! A file's test module is its first item at the file's top level, as `syn` parses the file,
//! that is a `mod`, with a body or without one, and carries `#[cfg(test)]`, a `cfg` whose
//! argument is `test` alone, among any other attributes, or `#![cfg(test)]` inside its body. A
//! `cfg` that names more than `test`, such as `#[cfg(all(test, unix))]`, isn't one; a module
//! inside another item, or one that a macro writes, isn't at the file's top level; and a file's
//! own `#![cfg(test)]` makes none of its items the test module. The lines before the test module
//! are those before its first outer attribute, a doc comment among them, or, without one, before
//! its visibility or its `mod`; a file without a test module has all its lines before it.

use proc_macro2::{LineColumn, Span, TokenStream};
use syn::{AttrStyle, Item, ItemMod, Visibility};

use crate::declarations::only_tests;

/// Where a file's test module stands.
#[derive(Debug, PartialEq, Eq)]
pub struct TestModule {
    /// How many lines of the file come before its test module.
    pub before: usize,
    /// The line of the first item after the test module, if an item follows it.
    pub followed: Option<usize>,
}

/// Where the test module of `text`, a Rust file, stands, or why `syn` can't parse the file.
pub fn test_module(text: &str) -> Result<TestModule, syn::Error> {
    let file = syn::parse_file(text)?;
    let found = file
        .items
        .iter()
        .enumerate()
        .find_map(|(at, item)| match item {
            Item::Mod(module) if only_tests(&module.attrs) => Some((at, module)),
            _ => None,
        });
    let Some((at, module)) = found else {
        return Ok(TestModule {
            before: text.lines().count(),
            followed: None,
        });
    };
    let followed = if at + 1 < file.items.len() {
        let end = match (&module.content, &module.semi) {
            (Some((brace, _)), _) => brace.span.close().end(),
            (None, Some(semi)) => semi.spans[0].end(),
            (None, None) => module.ident.span().end(),
        };
        // The item's first token, an attribute's `#` or its keyword, is the first of the file's
        // tokens that starts after the test module ends.
        let tokens: TokenStream = text.parse().map_err(syn::Error::from)?;
        tokens
            .into_iter()
            .map(|token| token.span().start())
            .find(|start| after(*start, end))
            .map(|start| start.line)
    } else {
        None
    };
    Ok(TestModule {
        before: first_line(module) - 1,
        followed,
    })
}

/// The line of `module`'s first token: its first outer attribute's, a doc comment's among them,
/// or else its visibility's or its keyword's.
fn first_line(module: &ItemMod) -> usize {
    let attributes = module
        .attrs
        .iter()
        .filter(|attribute| matches!(attribute.style, AttrStyle::Outer))
        .map(|attribute| attribute.pound_token.spans[0]);
    let visibility = match &module.vis {
        Visibility::Public(public) => Some(public.span),
        Visibility::Restricted(restricted) => Some(restricted.pub_token.span),
        Visibility::Inherited => None,
    };
    attributes
        .chain(visibility)
        .chain(module.unsafety.as_ref().map(|unsafety| unsafety.span))
        .chain([module.mod_token.span])
        .map(|span: Span| span.start().line)
        .min()
        .unwrap_or(1)
}

/// Whether `start` comes after `end` in the file.
fn after(start: LineColumn, end: LineColumn) -> bool {
    (start.line, start.column) >= (end.line, end.column)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(text: &str) -> TestModule {
        test_module(text).unwrap()
    }

    #[test]
    fn code_08_the_test_module_is_the_first_top_level_mod_under_cfg_test() {
        let direct = "fn a() {}\n\n/// The tests.\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n";
        let between = "fn a() {}\n#[cfg(test)]\n#[expect(\n    clippy::x,\n    reason = \"r\"\n)]\n\
                       pub(crate) mod tests {}\n";
        let declared = "fn a() {}\n#[cfg(test)]\nmod tests;\n";
        let inside = "fn a() {}\n\npub mod tests {\n    #![cfg(test)]\n}\n";
        assert_eq!(
            read(direct),
            TestModule {
                before: 2,
                followed: None
            }
        );
        assert_eq!(read(between).before, 1);
        assert_eq!(read(declared).before, 1);
        assert_eq!(read(inside).before, 2);
    }

    #[test]
    fn code_08_a_file_without_a_top_level_test_module_counts_all_its_lines() {
        let nested = "pub mod testing {\n    fn a() {}\n    #[cfg(test)]\n    mod tests {}\n}\n";
        let none = "fn a() {}\nfn b() {}\n";
        let other_items = "#[cfg(test)]\nuse std::fmt;\n#[cfg(test)]\nimpl A {}\n\
                           #[cfg(test)]\nfn f() {}\n";
        let narrower = "fn a() {}\n#[cfg(all(test, unix))]\nmod tests {}\n";
        let inner = "#![cfg(test)]\nfn a() {}\nmod helpers {}\n";
        let written = "macro_rules! tested {\n    () => {\n        #[cfg(test)]\n        mod tests {}\n    \
                       };\n}\ntested!();\n";
        for (text, lines) in [
            (nested, 5),
            (none, 2),
            (other_items, 6),
            (narrower, 3),
            (inner, 3),
            (written, 7),
        ] {
            assert_eq!(
                read(text),
                TestModule {
                    before: lines,
                    followed: None
                },
                "{text}"
            );
        }
    }

    #[test]
    fn code_08_the_first_item_after_the_test_module_is_found() {
        let derived = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn t() {}\n}\n\n// A comment.\n\
                       #[derive(Debug)]\nstruct After;\n";
        let declared = "#[cfg(test)]\nmod tests;\nfn a() {}\n";
        let second = "#[cfg(test)]\nmod tests {}\n#[cfg(test)]\nmod more {}\n";
        let last = "fn a() {}\n#[cfg(test)]\nmod tests {}\n// A comment after it.\n";
        assert_eq!(read(derived).followed, Some(8));
        assert_eq!(read(declared).followed, Some(3));
        assert_eq!(read(second).followed, Some(3));
        assert_eq!(read(last).followed, None);
    }

    #[test]
    fn code_08_a_file_that_doesn_t_parse_is_an_error() {
        let error = test_module("fn a() {}\n#[cfg(test)]\nmod tests {\n    fn t( {}\n}\n").err();
        assert!(error.is_some());
        assert!(test_module("#[attribute]\n").is_err());
    }
}
