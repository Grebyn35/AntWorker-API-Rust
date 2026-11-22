//! What each module's `mod.rs` exports, and the lines that name an export as `<module>::<name>`,
//! read once for every removal that `check-parts` does in memory: a removal that deletes every
//! line of a `mod.rs` that exports a name leaves each kept line that names it so pointing at
//! nothing, though it holds no word of the part (ARCH-07).

use std::collections::{HashMap, HashSet};

use syn::{Item, UseTree, Visibility};

use super::{
    dry_run::{Removal, is_rewritten},
    is_migration,
};
use crate::{Finding, repo::Repo, text};

/// The directory of the modules, each of which gives the others what its `mod.rs` exports.
const MODULES: &str = "crates/app/src/modules/";

/// A name that a module's `mod.rs` exports.
struct Export<'a> {
    /// The `mod.rs`.
    path: &'a str,
    module: &'a str,
    name: String,
    /// The line that holds the name, counted from 1.
    line: usize,
}

/// The modules' exports, and each `<module>::<name>` that a line holds where `<module>` is a
/// module, outside the migration's documents, the files a removal writes again and the tests of
/// the checks, whose code is made up for a check to read (DOC-12): the line's file, its number,
/// the module and the name.
pub(super) struct Exports<'a> {
    exported: Vec<Export<'a>>,
    named: Vec<(&'a str, usize, &'a str, &'a str)>,
}

impl<'a> Exports<'a> {
    /// Reads `repo`: the names that each `pub` `use` of `crates/app/src/modules/<module>/mod.rs`
    /// gives, the last name of each of its paths or the name after its `as`, with the line that
    /// holds that name; and the lines that hold `<module>::<name>`, `::` between two words of
    /// letters, digits and `_`. A `mod.rs` that doesn't parse exports nothing here: the build
    /// refuses it.
    pub(super) fn read(repo: &'a Repo) -> Self {
        let mut exported = Vec::new();
        for file in repo.files() {
            let Some(module) = file
                .path
                .strip_prefix(MODULES)
                .and_then(|rest| rest.strip_suffix("/mod.rs"))
                .filter(|module| !module.contains('/'))
            else {
                continue;
            };
            let Ok(parsed) = syn::parse_file(&file.text) else {
                continue;
            };
            for item in &parsed.items {
                if let Item::Use(item) = item
                    && !matches!(item.vis, Visibility::Inherited)
                {
                    for (name, line) in names(&item.tree, None) {
                        exported.push(Export {
                            path: &file.path,
                            module,
                            name,
                            line,
                        });
                    }
                }
            }
        }
        let modules: HashSet<&str> = exported.iter().map(|export| export.module).collect();
        let mut named = Vec::new();
        for file in repo
            .files()
            .filter(|file| !is_migration(&file.path) && !is_rewritten(&file.path))
        {
            let citing = text::citing_lines(&file.path, &file.text);
            for (at, line) in file.text.lines().take(citing).enumerate() {
                for (module, name) in paths(line) {
                    if modules.contains(module) {
                        named.push((file.path.as_str(), at + 1, module, name));
                    }
                }
            }
        }
        Self { exported, named }
    }

    /// A finding for each line that `removal` keeps and that names `<module>::<name>` for a name
    /// that the module's `mod.rs` exports only on lines that the removal deletes.
    pub(super) fn dangling(&self, removal: &Removal<'_>) -> Vec<Finding> {
        let (gone, kept): (Vec<&Export<'_>>, Vec<&Export<'_>>) = self
            .exported
            .iter()
            .partition(|export| removal.deletes(export.path, export.line));
        let kept: HashSet<(&str, &str)> = kept
            .iter()
            .map(|export| (export.module, export.name.as_str()))
            .collect();
        let gone: HashMap<(&str, &str), &str> = gone
            .iter()
            .map(|export| ((export.module, export.name.as_str()), export.path))
            .filter(|(name, _)| !kept.contains(name))
            .collect();
        self.named
            .iter()
            .filter(|(path, number, _, _)| removal.keeps(path, *number))
            .filter_map(|(path, number, module, name)| {
                let from = gone.get(&(*module, *name))?;
                Some(Finding::new(
                    path,
                    *number,
                    format!(
                        "names `{module}::{name}`, which only lines of {from} that the removal \
                         deletes export (ARCH-07)"
                    ),
                ))
            })
            .collect()
    }
}

/// The names that the `use` tree `tree` gives, each with the line that holds it: the last name of
/// each path, or the name after its `as`, and for `self` the name before it. `last` is the name
/// before the tree, if any. A glob gives none that the check can name.
fn names(tree: &UseTree, last: Option<&syn::Ident>) -> Vec<(String, usize)> {
    let named = |ident: &syn::Ident| (ident.to_string(), ident.span().start().line);
    match tree {
        UseTree::Path(path) => names(&path.tree, Some(&path.ident)),
        UseTree::Name(name) if name.ident == "self" => last.map(named).into_iter().collect(),
        UseTree::Name(name) => vec![named(&name.ident)],
        UseTree::Rename(rename) => vec![named(&rename.rename)],
        UseTree::Glob(_) => Vec::new(),
        UseTree::Group(group) => group
            .items
            .iter()
            .flat_map(|tree| names(tree, last))
            .collect(),
    }
}

/// Each `<word>::<word>` of `line`, where a word is the longest run of ASCII letters, digits and
/// `_` on each side of the `::`, so `workspaces::matching_all` names no `matching`.
fn paths(line: &str) -> Vec<(&str, &str)> {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices("::")
        .filter_map(|(at, _)| {
            let before = &line[..at];
            let start = before
                .rfind(|c: char| !word(c))
                .map_or(0, |found| found + 1);
            let after = &line[at + 2..];
            let end = after.find(|c: char| !word(c)).unwrap_or(after.len());
            let (module, name) = (&before[start..], &after[..end]);
            (!module.is_empty() && !name.is_empty()).then_some((module, name))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_07_a_path_is_two_whole_words_on_each_side_of_its_colons() {
        assert_eq!(
            paths("use crate::modules::workspaces::matching; `my_a::b_c`, a::, ::b, a::b2x"),
            [
                ("crate", "modules"),
                ("modules", "workspaces"),
                ("workspaces", "matching"),
                ("my_a", "b_c"),
                ("a", "b2x"),
            ]
        );
    }

    #[test]
    fn arch_07_a_module_exports_the_last_name_of_each_path_or_its_rename() {
        let file = syn::parse_file(
            "pub use a::B;\npub(crate) use c::{d::E, F as G,\n    h::{self, I}};\npub use j::*;\n",
        )
        .unwrap();
        let mut found = Vec::new();
        for item in &file.items {
            if let Item::Use(item) = item {
                found.extend(names(&item.tree, None));
            }
        }

        let found: Vec<(&str, usize)> = found
            .iter()
            .map(|(name, line)| (name.as_str(), *line))
            .collect();
        assert_eq!(found, [("B", 1), ("E", 2), ("G", 2), ("h", 3), ("I", 3)]);
    }
}
