//! Rust source read as token streams, for the checks of the API's layers and of its modules'
//! dependencies (`check-layers`, `check-deps`): the module a file is, the paths it names, what
//! its `use` declarations import, and its functions' signatures. Token streams hold the paths
//! inside macros and attributes too, such as a `#[utoipa::path]`'s `body`.

use std::collections::BTreeMap;

use proc_macro2::{Delimiter, Spacing, TokenStream, TokenTree};

use crate::{Finding, repo::File};

mod items;

use items::is_public;
pub use items::{holds, public_fields, signatures, types_with_attributes};

/// The `app` crate's code.
pub const APP: &str = "crates/app/src/";

/// The feature modules, one directory each (ARCH-03).
pub const MODULES: &str = "crates/app/src/modules/";

/// A path a file names, with its line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Named {
    /// The path's segments, as written, or as its `use` declaration writes them in full.
    pub segments: Vec<String>,
    /// The line, counted from 1.
    pub line: usize,
}

/// A Rust file, read.
pub struct Source<'a> {
    /// The file, as a path from the repository root.
    pub path: &'a str,
    /// The file's module, as a path from its crate's root, such as `["modules", "auth",
    /// "service"]`, or `None` outside `app`'s library.
    pub module: Option<Vec<String>>,
    /// The file's tokens.
    pub tokens: Vec<TokenTree>,
    /// What the file's `use` declarations import: each name, with the path it stands for, from
    /// `crate` or from an external crate. A glob imports the name `*` of its path.
    pub imports: BTreeMap<String, Vec<String>>,
}

impl<'a> Source<'a> {
    /// Reads `file`, or reports why it isn't Rust.
    pub fn read(file: &'a File) -> Result<Self, Finding> {
        let tokens: Vec<TokenTree> = file
            .text
            .parse::<TokenStream>()
            .map_err(|error| {
                Finding::new(&file.path, error.span().start().line, error.to_string())
            })?
            .into_iter()
            .collect();
        let module = file
            .path
            .strip_prefix(APP)
            .filter(|_| file.path != "crates/app/src/main.rs")
            .and_then(|path| path.strip_suffix(".rs"))
            .map(|path| {
                let mut segments: Vec<String> = path.split('/').map(str::to_owned).collect();
                if segments
                    .last()
                    .is_some_and(|last| last == "mod" || last == "lib")
                {
                    segments.pop();
                }
                segments
            });
        let mut source = Self {
            path: &file.path,
            module,
            tokens,
            imports: BTreeMap::new(),
        };
        let children = children(&source.tokens);
        for (import, _) in uses(&source.tokens) {
            let path = source.import_path(&import.path, &children);
            let name = import.alias.unwrap_or_else(|| "*".to_owned());
            source.imports.insert(name, path);
        }
        Ok(source)
    }

    /// The path a `use` declaration names, from `crate` when it starts in this crate.
    fn import_path(&self, path: &[String], children: &[String]) -> Vec<String> {
        match (path.first(), &self.module) {
            (Some(first), Some(module)) if children.contains(first) => {
                let mut absolute = vec!["crate".to_owned()];
                absolute.extend(module.iter().cloned());
                absolute.extend(path.iter().cloned());
                absolute
            }
            _ => self.absolute(path),
        }
    }

    /// `segments` as a path from `crate`, when it starts with `crate`, `self`, `super` or a
    /// name this file imports from the crate; otherwise as written, for a path that starts
    /// with an external crate.
    pub fn absolute(&self, segments: &[String]) -> Vec<String> {
        let Some(first) = segments.first() else {
            return Vec::new();
        };
        let rest = &segments[1..];
        if let Some(imported) = self.imports.get(first)
            && !["crate", "self", "super"].contains(&first.as_str())
        {
            let mut absolute = imported.clone();
            absolute.extend(rest.iter().cloned());
            return absolute;
        }
        let Some(module) = &self.module else {
            return segments.to_vec();
        };
        match first.as_str() {
            "crate" => segments.to_vec(),
            "self" | "super" => {
                let mut current = module.clone();
                let mut at = 0;
                while let Some(segment) = segments.get(at) {
                    match segment.as_str() {
                        "self" => {}
                        "super" => {
                            current.pop();
                        }
                        _ => break,
                    }
                    at += 1;
                }
                let mut absolute = vec!["crate".to_owned()];
                absolute.extend(current);
                absolute.extend(segments[at..].iter().cloned());
                absolute
            }
            // The crate root names its own modules without `crate::`.
            _ if module.is_empty() && first == "modules" => {
                let mut absolute = vec!["crate".to_owned()];
                absolute.extend(segments.iter().cloned());
                absolute
            }
            _ => segments.to_vec(),
        }
    }

    /// Every path the file names, each as [`Source::absolute`] gives it.
    pub fn named(&self) -> Vec<Named> {
        self.resolved(paths(&self.tokens))
    }

    /// `names`, each as [`Source::absolute`] gives it.
    pub fn resolved(&self, names: Vec<Named>) -> Vec<Named> {
        names
            .into_iter()
            .map(|named| Named {
                segments: self.absolute(&named.segments),
                line: named.line,
            })
            .collect()
    }
}

/// The feature module that the file at `path` belongs to, and the file's place in it, such as
/// `("auth", ["service"])` for `crates/app/src/modules/auth/service.rs`, or `("auth", [])` for
/// its `mod.rs`.
pub fn feature_module(path: &str) -> Option<(&str, Vec<&str>)> {
    let rest = path.strip_prefix(MODULES)?.strip_suffix(".rs")?;
    let (module, inside) = rest.split_once('/')?;
    let mut parts: Vec<&str> = inside.split('/').collect();
    if parts.last() == Some(&"mod") {
        parts.pop();
    }
    Some((module, parts))
}

/// A name a `use` declaration imports.
#[derive(Debug, PartialEq, Eq)]
pub struct Import {
    /// The path, as written, without the leading `::` of a global path.
    pub path: Vec<String>,
    /// The name it is imported as, or `None` for a glob.
    pub alias: Option<String>,
}

/// Every name the `use` declarations among `tokens` import, at any depth, with the line of each.
pub fn uses(tokens: &[TokenTree]) -> Vec<(Import, usize)> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(token) = tokens.get(at) {
        match token {
            TokenTree::Ident(ident) if ident == "use" => {
                let end = tokens[at..]
                    .iter()
                    .position(|token| matches!(token, TokenTree::Punct(p) if p.as_char() == ';'))
                    .map_or(tokens.len(), |end| at + end);
                let line = ident.span().start().line;
                let mut imports = Vec::new();
                use_tree(&tokens[at + 1..end], &[], &mut imports);
                found.extend(imports.into_iter().map(|import| (import, line)));
                at = end;
            }
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                found.extend(uses(&inner));
            }
            _ => {}
        }
        at += 1;
    }
    found
}

/// The imports of one use tree, `tokens`, under `prefix`.
fn use_tree(tokens: &[TokenTree], prefix: &[String], imports: &mut Vec<Import>) {
    let mut path = prefix.to_vec();
    let mut at = 0;
    while let Some(token) = tokens.get(at) {
        match token {
            TokenTree::Ident(ident) if ident == "as" => {
                let alias = tokens.get(at + 1).map(ToString::to_string);
                imports.push(Import { path, alias });
                return;
            }
            TokenTree::Ident(ident) if ident == "self" && !path.is_empty() => {}
            TokenTree::Ident(ident) => path.push(ident.to_string()),
            TokenTree::Punct(star) if star.as_char() == '*' => {
                imports.push(Import { path, alias: None });
                return;
            }
            TokenTree::Group(group) if group.delimiter() == Delimiter::Brace => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                for part in split(&inner) {
                    use_tree(part, &path, imports);
                }
                return;
            }
            _ => {}
        }
        at += 1;
    }
    if let Some(last) = path.last().cloned() {
        imports.push(Import {
            path,
            alias: Some(last),
        });
    }
}

/// `tokens` split at their top-level commas.
pub fn split(tokens: &[TokenTree]) -> Vec<&[TokenTree]> {
    tokens
        .split(|token| matches!(token, TokenTree::Punct(comma) if comma.as_char() == ','))
        .filter(|part| !part.is_empty())
        .collect()
}

/// Every path of two or more segments among `tokens`, at any depth, inside macros and
/// attributes too, and every path a `use` declaration imports.
pub fn paths(tokens: &[TokenTree]) -> Vec<Named> {
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(token) = tokens.get(at) {
        match token {
            TokenTree::Ident(ident) if ident == "use" => {
                let end = tokens[at..]
                    .iter()
                    .position(|token| matches!(token, TokenTree::Punct(p) if p.as_char() == ';'))
                    .map_or(tokens.len(), |end| at + end);
                let mut imports = Vec::new();
                use_tree(&tokens[at + 1..end], &[], &mut imports);
                let line = ident.span().start().line;
                found.extend(imports.into_iter().map(|import| Named {
                    segments: import.path,
                    line,
                }));
                at = end;
            }
            TokenTree::Ident(ident) if !continues_path(tokens, at) => {
                let mut segments = vec![ident.to_string()];
                let mut next = at + 1;
                while is_path_separator(tokens, next)
                    && let Some(TokenTree::Ident(segment)) = tokens.get(next + 2)
                {
                    segments.push(segment.to_string());
                    next += 3;
                }
                if segments.len() > 1 {
                    found.push(Named {
                        segments,
                        line: ident.span().start().line,
                    });
                }
            }
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                found.extend(paths(&inner));
            }
            _ => {}
        }
        at += 1;
    }
    found
}

/// Whether the tokens at `at` and after it are `::`.
fn is_path_separator(tokens: &[TokenTree], at: usize) -> bool {
    matches!(
        (tokens.get(at), tokens.get(at + 1)),
        (Some(TokenTree::Punct(a)), Some(TokenTree::Punct(b)))
            if a.as_char() == ':' && a.spacing() == Spacing::Joint && b.as_char() == ':'
    )
}

/// Whether the identifier at `at` continues a path, after `name::` or `<...>::`, or follows a
/// `.`, as a method or a field does.
fn continues_path(tokens: &[TokenTree], at: usize) -> bool {
    if at >= 1 && matches!(&tokens[at - 1], TokenTree::Punct(dot) if dot.as_char() == '.') {
        return true;
    }
    at >= 3
        && is_path_separator(tokens, at - 2)
        && match &tokens[at - 3] {
            TokenTree::Ident(_) => true,
            TokenTree::Punct(close) => close.as_char() == '>',
            _ => false,
        }
}

/// The modules a file declares at its top level, `mod name;` and `mod name { ... }`, each with
/// whether it is `pub` and its line.
pub fn declared(tokens: &[TokenTree]) -> Vec<(String, bool, usize)> {
    let mut found = Vec::new();
    for (at, token) in tokens.iter().enumerate() {
        if let (TokenTree::Ident(keyword), Some(TokenTree::Ident(name))) =
            (token, tokens.get(at + 1))
            && keyword == "mod"
        {
            found.push((
                name.to_string(),
                is_public(&tokens[..at]),
                keyword.span().start().line,
            ));
        }
    }
    found
}

/// The names of the modules a file declares at its top level.
fn children(tokens: &[TokenTree]) -> Vec<String> {
    declared(tokens)
        .into_iter()
        .map(|(name, _, _)| name)
        .collect()
}

/// Every name among `tokens`: each path, as [`paths`] finds it, and each identifier that stands
/// alone, such as a type a `use` declaration imported.
pub fn names(tokens: &[TokenTree]) -> Vec<Named> {
    let mut found = paths(tokens);
    singles(tokens, &mut found);
    found
}

/// Every identifier among `tokens` that is no segment of a longer path and follows no `.`.
fn singles(tokens: &[TokenTree], found: &mut Vec<Named>) {
    for (at, token) in tokens.iter().enumerate() {
        match token {
            TokenTree::Ident(ident)
                if !continues_path(tokens, at) && !is_path_separator(tokens, at + 1) =>
            {
                found.push(Named {
                    segments: vec![ident.to_string()],
                    line: ident.span().start().line,
                });
            }
            TokenTree::Group(group) => {
                let inner: Vec<TokenTree> = group.stream().into_iter().collect();
                singles(&inner, found);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens(code: &str) -> Vec<TokenTree> {
        code.parse::<TokenStream>().unwrap().into_iter().collect()
    }

    fn file(path: &str, text: &str) -> File {
        File {
            path: path.to_owned(),
            text: text.to_owned(),
        }
    }

    fn strings(named: &[Named]) -> Vec<String> {
        named.iter().map(|n| n.segments.join("::")).collect()
    }

    #[test]
    fn arch_05_use_trees_expand_to_the_paths_they_import() {
        let code = "use crate::modules::{accounts::{self, Profile as P}, audit::*}; use ::sqlx;";
        let imports: Vec<(Vec<String>, Option<String>)> = uses(&tokens(code))
            .into_iter()
            .map(|(import, _)| (import.path, import.alias))
            .collect();
        let path = |p: &str| p.split("::").map(str::to_owned).collect::<Vec<_>>();
        assert_eq!(
            imports,
            [
                (
                    path("crate::modules::accounts"),
                    Some("accounts".to_owned())
                ),
                (
                    path("crate::modules::accounts::Profile"),
                    Some("P".to_owned())
                ),
                (path("crate::modules::audit"), None),
                (path("sqlx"), Some("sqlx".to_owned())),
            ]
        );
    }

    #[test]
    fn arch_05_paths_inside_macros_and_attributes_resolve_from_the_file_s_module() {
        let text = r#"
            use super::model::Entry;
            #[utoipa::path(get, path = "/v1/x", responses((status = 200, body = crate::modules::accounts::Profile)))]
            fn f() { super::super::audit::record(x.y::<T>()); sqlx::query!("SELECT 1"); }
        "#;
        let file = file("crates/app/src/modules/auth/service.rs", text);
        let source = Source::read(&file).unwrap();
        assert_eq!(
            strings(&source.named()),
            [
                "crate::modules::auth::model::Entry",
                "utoipa::path",
                "crate::modules::accounts::Profile",
                "crate::modules::audit::record",
                "sqlx::query",
            ]
        );
    }

    #[test]
    fn arch_04_a_module_s_root_imports_from_its_own_submodules() {
        let text = "mod error; mod routes; pub(crate) use error::AccountsError; pub(crate) use routes::routes;";
        let file = file("crates/app/src/modules/accounts/mod.rs", text);
        let source = Source::read(&file).unwrap();
        assert_eq!(
            source.imports.get("AccountsError").map(|p| p.join("::")),
            Some("crate::modules::accounts::error::AccountsError".to_owned())
        );
        assert_eq!(
            feature_module("crates/app/src/modules/accounts/mod.rs"),
            Some(("accounts", Vec::new()))
        );
    }

    #[test]
    fn err_06_signatures_know_whether_they_are_public() {
        let code = "impl A { pub(crate) async fn a(x: X) -> Y {} fn b() {} } pub fn c();";
        let found: Vec<bool> = signatures(&tokens(code)).iter().map(|s| s.public).collect();
        assert_eq!(found, [true, false, true]);
    }
}
