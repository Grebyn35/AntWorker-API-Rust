//! The `#[cfg]` predicates of the sqlx crates, evaluated for the features a crate is built with.
//!
//! A predicate is true, false, or unknown: `feature = "..."` is known from the features, `test`,
//! `doc` and `docsrs` are false for a dependency, and every other predicate, such as `unix` or
//! `debug_assertions`, depends on the platform or the profile, so it is unknown. An item counts
//! unless its predicate is false: it is compiled on some platform, where code can call it.

use std::collections::BTreeSet;

use proc_macro2::{Delimiter, TokenStream, TokenTree};

/// The value of a predicate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Truth {
    True,
    False,
    Unknown,
}

impl Truth {
    fn not(self) -> Self {
        match self {
            Self::True => Self::False,
            Self::False => Self::True,
            Self::Unknown => Self::Unknown,
        }
    }
}

/// Whether the item with `attrs` is compiled with `features` on some platform: none of its
/// `#[cfg]` predicates is false.
pub fn active(attrs: &[syn::Attribute], features: &BTreeSet<String>) -> bool {
    attrs
        .iter()
        .filter(|attr| attr.path().is_ident("cfg"))
        .all(|attr| match &attr.meta {
            syn::Meta::List(list) => {
                let tokens: Vec<TokenTree> = list.tokens.clone().into_iter().collect();
                predicate(&tokens, features) != Truth::False
            }
            _ => true,
        })
}

/// The value of one predicate: `name`, `name = "value"`, or `any(...)`, `all(...)`, `not(...)`.
fn predicate(tokens: &[TokenTree], features: &BTreeSet<String>) -> Truth {
    match tokens {
        [TokenTree::Ident(name), TokenTree::Group(group)]
            if group.delimiter() == Delimiter::Parenthesis =>
        {
            let operands: Vec<Truth> = split(group.stream())
                .iter()
                .map(|operand| predicate(operand, features))
                .collect();
            match name.to_string().as_str() {
                "any" => fold(&operands, Truth::True, Truth::False),
                "all" => fold(&operands, Truth::False, Truth::True),
                "not" => match operands.as_slice() {
                    [operand] => operand.not(),
                    _ => Truth::Unknown,
                },
                _ => Truth::Unknown,
            }
        }
        [
            TokenTree::Ident(name),
            TokenTree::Punct(equals),
            TokenTree::Literal(value),
        ] if equals.as_char() == '=' && name == "feature" => {
            let value = value.to_string();
            let value = value.trim_matches('"');
            if features.contains(value) {
                Truth::True
            } else {
                Truth::False
            }
        }
        [TokenTree::Ident(name)] => match name.to_string().as_str() {
            "test" | "doc" | "docsrs" | "false" => Truth::False,
            "true" => Truth::True,
            _ => Truth::Unknown,
        },
        _ => Truth::Unknown,
    }
}

/// `any` when `decisive` is true, `all` when it is false: an operand equal to `decisive`
/// decides, and with none, an unknown operand leaves the result unknown.
fn fold(operands: &[Truth], decisive: Truth, otherwise: Truth) -> Truth {
    if operands.contains(&decisive) {
        decisive
    } else if operands.contains(&Truth::Unknown) {
        Truth::Unknown
    } else {
        otherwise
    }
}

/// The tokens between top-level commas.
fn split(tokens: TokenStream) -> Vec<Vec<TokenTree>> {
    let mut parts = vec![Vec::new()];
    for token in tokens {
        match &token {
            TokenTree::Punct(comma) if comma.as_char() == ',' => parts.push(Vec::new()),
            _ => {
                if let Some(part) = parts.last_mut() {
                    part.push(token);
                }
            }
        }
    }
    parts.retain(|part| !part.is_empty());
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    fn is_active(attribute: &str) -> bool {
        let item: syn::ItemFn = syn::parse_str(&format!("{attribute} fn f() {{}}")).unwrap();
        let features = BTreeSet::from(["postgres".to_owned(), "migrate".to_owned()]);
        active(&item.attrs, &features)
    }

    #[test]
    fn data_20_items_count_unless_the_locked_features_rule_them_out() {
        assert!(is_active(r#"#[cfg(feature = "postgres")]"#));
        assert!(!is_active(r#"#[cfg(feature = "mysql")]"#));
        assert!(is_active(
            r#"#[cfg(any(feature = "mysql", feature = "postgres"))]"#
        ));
        assert!(!is_active(
            r#"#[cfg(all(feature = "migrate", not(feature = "postgres")))]"#
        ));
        assert!(!is_active("#[cfg(test)]"));
        assert!(!is_active("#[cfg(docsrs)]"));
        assert!(!is_active("#[cfg(any())]"));
        assert!(is_active("#[cfg(all())]"));
    }

    #[test]
    fn data_20_an_item_of_some_platform_or_profile_counts() {
        assert!(is_active("#[cfg(unix)]"));
        assert!(is_active("#[cfg(not(unix))]"));
        assert!(is_active(r#"#[cfg(target_os = "windows")]"#));
        assert!(is_active(
            r#"#[cfg(all(feature = "postgres", not(debug_assertions)))]"#
        ));
        assert!(!is_active(
            r#"#[cfg(all(feature = "mysql", not(debug_assertions)))]"#
        ));
    }
}
