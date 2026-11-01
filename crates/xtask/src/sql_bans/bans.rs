//! The bans of `clippy.toml`, and what each reaches in the sqlx crates, resolved as clippy
//! resolves a ban's path: its first part names a crate, each part but the last a module, an
//! enum or a trait among the names its parent holds, and the last part an item it holds, or a
//! method of an impl block of the type the part before names, or a method of the trait it names.
//! A type alias holds nothing, so a method is banned by its type's own path.

use std::collections::BTreeMap;

use syn::Item;

use super::model::{Def, ItemId, Model, ModuleId, Ns, name};

/// A ban: the path it names, and its line in `clippy.toml`.
pub struct Ban {
    pub path: String,
    pub line: usize,
}

/// A list of bans, such as `disallowed-methods`, with the line of its key.
pub struct List {
    pub key: &'static str,
    pub line: usize,
    pub bans: Vec<Ban>,
}

/// The list under `key` in `config`, the text of `clippy.toml`; an empty list at line 1 when it
/// has none.
pub fn list(config: &str, key: &'static str) -> List {
    let mut list = List {
        key,
        line: 1,
        bans: Vec::new(),
    };
    let mut offset = 0;
    let mut start = None;
    for (index, line) in config.split_inclusive('\n').enumerate() {
        let rest = line.trim_start().strip_prefix(key).map(str::trim_start);
        if rest.is_some_and(|rest| rest.starts_with('=')) {
            list.line = index + 1;
            start = Some(offset);
            break;
        }
        offset += line.len();
    }
    if let Some(start) = start {
        list.bans = entries(config.get(start..).unwrap_or_default(), list.line);
    }
    list
}

/// The paths of the array that starts in `text`, whose first line is `line`: each element that
/// is a string, and the `path` of each that is a table.
fn entries(text: &str, mut line: usize) -> Vec<Ban> {
    let mut bans = Vec::new();
    let (mut brackets, mut braces) = (0_usize, 0_usize);
    let mut key = String::new();
    let mut last_key = String::new();
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        match character {
            '\n' => line += 1,
            '#' => {
                if chars.by_ref().any(|skipped| skipped == '\n') {
                    line += 1;
                }
            }
            '[' => brackets += 1,
            ']' => {
                brackets = brackets.saturating_sub(1);
                if brackets == 0 {
                    break;
                }
            }
            '{' => braces += 1,
            '}' => braces = braces.saturating_sub(1),
            '=' => last_key = std::mem::take(&mut key),
            '"' => {
                let value: String = chars.by_ref().take_while(|next| *next != '"').collect();
                if (brackets == 1 && braces == 0) || (braces == 1 && last_key == "path") {
                    bans.push(Ban { path: value, line });
                }
            }
            ',' => {
                key.clear();
                last_key.clear();
            }
            _ if character.is_alphanumeric() || character == '-' || character == '_' => {
                key.push(character);
            }
            _ => {}
        }
    }
    bans
}

/// What the ban of `path` reaches in `ns`: `None` when its first part names no crate of the
/// model, so it bans an item of another crate.
pub fn reaches(
    model: &Model,
    impls: &BTreeMap<ItemId, Vec<ItemId>>,
    path: &str,
    ns: Ns,
) -> Option<Vec<Def>> {
    let segments: Vec<&str> = path.split("::").collect();
    let (first, rest) = segments.split_first()?;
    let krate = model.crates.iter().position(|krate| krate.name == *first)?;
    let mut current = vec![Def::Module(ModuleId { krate, index: 0 })];
    for (position, segment) in rest.iter().enumerate() {
        let last = position + 1 == rest.len();
        let mut next = Vec::new();
        for def in &current {
            match def {
                Def::Module(module) => {
                    let here = if last { ns } else { Ns::Type };
                    next.extend(model.lookup(*module, segment, here, false));
                }
                Def::Item(id) if last => next.extend(associated(model, impls, *id, segment)),
                _ => {}
            }
        }
        current = next;
    }
    current.retain(|def| *def != Def::Outside);
    Some(current)
}

/// The methods named `method` of the type or trait `id`: its trait's, or its impl blocks'.
fn associated(
    model: &Model,
    impls: &BTreeMap<ItemId, Vec<ItemId>>,
    id: ItemId,
    method: &str,
) -> Vec<Def> {
    let mut found = Vec::new();
    match model.item(id) {
        Some(Item::Trait(trait_)) => {
            for (index, inner) in trait_.items.iter().enumerate() {
                if let syn::TraitItem::Fn(function) = inner
                    && name(&function.sig.ident) == method
                {
                    found.push(Def::Assoc(id, index));
                }
            }
        }
        Some(Item::Struct(_) | Item::Enum(_) | Item::Union(_)) => {
            for block in impls.get(&id).map_or(&[][..], Vec::as_slice) {
                let Some(Item::Impl(items)) = model.item(*block) else {
                    continue;
                };
                for (index, inner) in items.items.iter().enumerate() {
                    if let syn::ImplItem::Fn(function) = inner
                        && name(&function.sig.ident) == method
                    {
                        found.push(Def::Assoc(*block, index));
                    }
                }
            }
        }
        _ => {}
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_20_the_bans_are_read_with_their_lines() {
        let config = r#"too-many-lines-threshold = 80

disallowed-methods = [
    { path = "std::fs::read", reason = "blocking (CODE-05)" },
    # a comment with "quotes"
    { path = "sqlx::query", reason = "SQL goes through the macros (DATA-20)", allow-invalid = true },
    "sqlx::raw_sql",
]

disallowed-macros = [{ path = "sqlx::query_unchecked", reason = "unchecked" }]
"#;
        let methods = list(config, "disallowed-methods");
        let macros = list(config, "disallowed-macros");
        let types = list(config, "disallowed-types");

        let read: Vec<(&str, usize)> = methods
            .bans
            .iter()
            .map(|ban| (ban.path.as_str(), ban.line))
            .collect();
        assert_eq!(methods.line, 3);
        assert_eq!(
            read,
            [
                ("std::fs::read", 4),
                ("sqlx::query", 6),
                ("sqlx::raw_sql", 7)
            ]
        );
        assert_eq!(macros.line, 10);
        assert_eq!(macros.bans.len(), 1);
        assert_eq!((types.line, types.bans.len()), (1, 0));
    }
}
