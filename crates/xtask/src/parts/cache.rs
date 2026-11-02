//! The entries of the query cache (DATA-20) that a removal leaves behind: those whose query no
//! code it kept holds, such as a marked function's in another module (ARCH-07).

use std::collections::HashSet;

use proc_macro2::TokenTree;
use serde_json::Value;

use crate::{Finding, repo::Repo, rust::Source};

/// The directory of the query cache, from the repository root.
const CACHE: &str = ".sqlx/";

/// The entries of the cache whose query no string literal of the repository's Rust code holds,
/// or the first Rust file that can't be read.
pub fn stale(repo: &Repo) -> Result<Vec<String>, Finding> {
    let mut literals = HashSet::new();
    for file in repo.files().filter(|file| file.path.ends_with(".rs")) {
        collect(&Source::read(file)?.tokens, &mut literals);
    }
    Ok(repo
        .files()
        .filter(|file| file.path.starts_with(CACHE) && file.path.ends_with(".json"))
        .filter(|file| {
            serde_json::from_str::<Value>(&file.text).is_ok_and(|entry| {
                entry["query"]
                    .as_str()
                    .is_some_and(|query| !literals.contains(query))
            })
        })
        .map(|file| file.path.clone())
        .collect())
}

/// Adds the value of each string literal of `tokens` to `literals`.
fn collect(tokens: &[TokenTree], literals: &mut HashSet<String>) {
    for token in tokens {
        match token {
            TokenTree::Literal(literal) => {
                if let Ok(text) = syn::parse_str::<syn::LitStr>(&literal.to_string()) {
                    literals.insert(text.value());
                }
            }
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                collect(&inner, literals);
            }
            TokenTree::Ident(_) | TokenTree::Punct(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_07_an_entry_whose_query_no_kept_code_holds_is_stale() {
        let repo = Repo::of(&[
            (
                "crates/app/src/modules/a/repo.rs",
                "fn f() { sqlx::query!(r#\"SELECT \"id\" FROM a\"#); sqlx::query!(\"SELECT 1\\n\"); }",
            ),
            (".sqlx/query-1.json", r#"{"query": "SELECT \"id\" FROM a"}"#),
            (".sqlx/query-2.json", r#"{"query": "SELECT 1\n"}"#),
            (".sqlx/query-3.json", r#"{"query": "SELECT id FROM gone"}"#),
        ]);

        assert_eq!(stale(&repo).unwrap(), [".sqlx/query-3.json"]);
    }
}
