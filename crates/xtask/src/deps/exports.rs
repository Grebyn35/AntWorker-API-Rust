//! ARCH-04 for what a module's `mod.rs` re-exports: the types that the signature of a function it
//! re-exports names, which its callers use, are re-exported too. A module's exported functions
//! are the functions that a file of the module declares at its top level and that its `mod.rs`
//! imports with a `pub use`, by name or by a glob, or declares `pub` itself, and a type that one's
//! signature names is a struct, an enum, a union, a type alias or a trait that a file of the
//! module declares at its top level, which a name of the signature, past the function's own,
//! reaches through the file's `use` declarations, of whose globs it follows the last, or as a
//! name of the file's own module (`rust`).

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Finding,
    rust::{self, Item, Named, Source},
};

/// The items that each file of `app`'s library declares at its top level, by the file's module.
type Index<'a> = BTreeMap<Vec<String>, (&'a Source<'a>, Vec<Item>)>;

/// An item, by its path from `crate`, with the file that declares it.
type Defined<'a> = (Vec<String>, &'a Source<'a>, &'a Item);

/// ARCH-04: a type of a module that the signature of a function its `mod.rs` re-exports names, and
/// that `mod.rs` doesn't re-export.
pub(super) fn unexported_types(sources: &[Source]) -> Vec<Finding> {
    let index: Index = sources
        .iter()
        .filter_map(|source| {
            Some((
                source.module.clone()?,
                (source, rust::items(&source.tokens)),
            ))
        })
        .collect();
    let mut findings = Vec::new();
    for (module, (root, items)) in &index {
        let [modules, own] = module.as_slice() else {
            continue;
        };
        if modules != "modules" {
            continue;
        }
        let own_path = ["crate".to_owned(), modules.clone(), own.clone()];
        let public = public_items(&index, root, items);
        let exported: BTreeSet<&Vec<String>> = public.iter().map(|(path, ..)| path).collect();
        for (path, source, item) in &public {
            let Some(signature) = item
                .signature
                .as_ref()
                .filter(|_| path.starts_with(&own_path))
            else {
                continue;
            };
            let named = rust::names(signature.tokens.get(1..).unwrap_or_default());
            for Named { segments, .. } in named {
                let Some((type_path, _, type_item)) = path_of(source, &segments)
                    .and_then(|named| definition(&index, &named, 0))
                    .filter(|(_, _, item)| rust::TYPES.contains(&item.keyword.as_str()))
                else {
                    continue;
                };
                if type_path.starts_with(&own_path) && !exported.contains(&type_path) {
                    findings.push(Finding::new(
                        source.path,
                        signature.line,
                        format!("`{own}::{}`, which `{own}`'s `mod.rs` re-exports, names `{name}` in its signature, which `mod.rs` doesn't re-export: other modules use only what it re-exports, so it re-exports `{name}` too, and code that takes one names it, as a binding's type or in a pattern, since the lint refuses a re-export that nothing names (ARCH-04)", item.name, name = type_item.name),
                    ));
                }
            }
        }
    }
    findings
}

/// What the module whose `mod.rs` is `root`, which declares `items`, makes public: each item a
/// `pub use` of it imports, by name or by a glob, and each `pub` item it declares itself.
fn public_items<'a>(
    index: &'a Index<'a>,
    root: &'a Source<'a>,
    items: &'a [Item],
) -> Vec<Defined<'a>> {
    let mut public = Vec::new();
    let mut own = vec!["crate".to_owned()];
    own.extend(root.module.iter().flatten().cloned());
    for name in rust::reexported(&root.tokens) {
        let Some(path) = root.imports.get(&name) else {
            continue;
        };
        if name == "*" {
            if let Some((source, items)) = path.get(1..).and_then(|module| index.get(module)) {
                for item in items.iter().filter(|item| item.public) {
                    let mut at = path.clone();
                    at.push(item.name.clone());
                    public.push((at, *source, item));
                }
            }
        } else {
            public.extend(definition(index, path, 0));
        }
    }
    for item in items.iter().filter(|item| item.public) {
        let mut at = own.clone();
        at.push(item.name.clone());
        public.push((at, root, item));
    }
    public
}

/// The path from `crate` that `segments`, named in `source`, stands for: through the file's
/// imports, or, for a name it doesn't import, from the file's own module, where a name it declares
/// is; `None` for a path into another crate.
fn path_of(source: &Source, segments: &[String]) -> Option<Vec<String>> {
    let absolute = source.absolute(segments);
    if absolute.first().is_some_and(|first| first == "crate") {
        return Some(absolute);
    }
    if source.imports.contains_key(segments.first()?) {
        return None;
    }
    let mut path = vec!["crate".to_owned()];
    path.extend(source.module.clone()?);
    path.extend(segments.iter().cloned());
    Some(path)
}

/// The item that `path`, from `crate`, names: one that the file of its module declares, or the
/// one that a `use` of that file imports under its name, by name or by a glob, `depth` imports on.
fn definition<'a>(index: &'a Index<'a>, path: &[String], depth: usize) -> Option<Defined<'a>> {
    let (name, module) = path.split_last()?;
    let (source, items) = index.get(module.strip_prefix(&["crate".to_owned()])?)?;
    if let Some(item) = items.iter().find(|item| item.name == *name) {
        return Some((path.to_vec(), source, item));
    }
    let target = match source.imports.get(name) {
        Some(target) => target.clone(),
        None => {
            let mut glob = source.imports.get("*")?.clone();
            glob.push(name.clone());
            glob
        }
    };
    (depth < index.len()).then(|| definition(index, &target, depth + 1))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repo::File;

    /// The types that the signatures of the functions that `things` re-exports name, which its
    /// `mod.rs` doesn't re-export, each after the function's name.
    fn unexported(files: &[(&str, &str)]) -> Vec<String> {
        let files: Vec<File> = files
            .iter()
            .map(|(path, text)| File {
                path: (*path).to_owned(),
                text: (*text).to_owned(),
            })
            .collect();
        let sources: Vec<Source> = files
            .iter()
            .map(|file| Source::read(file).unwrap())
            .collect();
        let mut found: Vec<String> = unexported_types(&sources)
            .into_iter()
            .map(|finding| finding.message)
            .filter_map(|message| {
                let (function, rest) = message.strip_prefix("`things::")?.split_once('`')?;
                let named = rest.split_once("names `")?.1.split_once('`')?.0;
                Some(format!("{function}: {named}"))
            })
            .collect();
        found.sort();
        found
    }

    #[test]
    fn arch_04_a_type_that_a_re_exported_function_s_signature_names_is_re_exported_too() {
        let root = "mod globbed; mod model; mod other; mod service;
            use model::Hidden;
            pub(crate) use globbed::*;
            pub(crate) use model::{Kind, Shown};
            pub(crate) use other::same_name;
            pub(crate) use crate::modules::accounts::leak;
            pub(crate) use service::{alias_of, boxed, elsewhere, fine, inner, plain as renamed, tuple, where_clause};
            pub(crate) fn declared(hidden: Hidden) {}";
        let model = "pub(crate) struct Shown; pub(crate) struct Hidden; pub(crate) enum Kind {}
            pub(crate) trait Shows {} pub(crate) type Alias = Hidden;";
        let service = "use super::model::{Hidden as H, Kind, Shown, Shows};
            use crate::modules::accounts::Secret;
            pub(crate) struct Local;
            pub(crate) fn plain() -> H {}
            pub(crate) fn alias_of() -> super::model::Alias {}
            pub(crate) fn boxed(shows: Box<dyn Shows>) -> impl Iterator<Item = Shown> {}
            pub(crate) fn tuple(pair: (u8, &Local)) -> Option<&[Kind]> {}
            pub(crate) fn where_clause<T>(t: T) where T: Into<Local> {}
            pub(crate) fn elsewhere() -> Secret {}
            pub(crate) fn fine(kind: Kind) -> Result<Shown, ()> {}
            fn private() -> Local {}
            impl Local { pub(crate) fn method(&self) -> Local {} }
            mod inline { pub(crate) struct Deep; }
            pub(crate) fn inner() -> inline::Deep {}";
        let other = "pub(crate) struct Shown; pub(crate) fn same_name() -> Shown {}";
        let globbed =
            "pub(crate) struct Open; struct Closed; pub(crate) fn g(open: Open) -> Closed {}";
        let accounts = "mod model; use model::{Secret, leak};";
        assert_eq!(
            unexported(&[
                ("crates/app/src/modules/things/mod.rs", root),
                ("crates/app/src/modules/things/model.rs", model),
                ("crates/app/src/modules/things/service.rs", service),
                ("crates/app/src/modules/things/other.rs", other),
                ("crates/app/src/modules/things/globbed.rs", globbed),
                ("crates/app/src/modules/accounts/mod.rs", accounts),
                (
                    "crates/app/src/modules/accounts/model.rs",
                    "pub(crate) struct Secret; pub(crate) fn leak() -> Secret {}"
                ),
            ]),
            [
                "alias_of: Alias",
                "boxed: Shows",
                "declared: Hidden",
                "g: Closed",
                "plain: Hidden",
                "same_name: Shown",
                "tuple: Local",
                "where_clause: Local",
            ],
            "`elsewhere` answers `accounts`' type, `leak` is `accounts`', and `inner` answers a type that no file declares at its top level"
        );
    }
}
