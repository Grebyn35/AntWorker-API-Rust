//! The sqlx crates' modules and names: what a path reaches through modules, `use` imports and
//! re-exports (renames and globs among them), in the items that the features of the lock enable
//! ([`cfg`](super::cfg)).
//!
//! The model holds `sqlx` and the crates of the sqlx project it names in a `use` or an `extern
//! crate`: packages named `sqlx-...`, such as `sqlx-core` and `sqlx-postgres`. A path into any
//! other crate reaches [`Def::Outside`]. A procedural macro crate exports only its macros, so the
//! model reads only its root file. [`load`] reads the crates, and [`resolve`] follows names.

mod load;
mod resolve;

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::Path,
};

use syn::{Item, UseTree};

/// A namespace: a module holds a type, a value and a macro under the same name apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Ns {
    Type,
    Value,
    Macro,
}

const NAMESPACES: [Ns; 3] = [Ns::Type, Ns::Value, Ns::Macro];

/// A module of a crate of the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ModuleId {
    pub krate: usize,
    pub index: usize,
}

/// An item of a module: its index among the module's items.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ItemId {
    pub module: ModuleId,
    pub index: usize,
}

/// What a path reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Def {
    Module(ModuleId),
    Item(ItemId),
    /// A function of an impl block or a trait: the block or trait, and the function's index
    /// among its items.
    Assoc(ItemId, usize),
    /// An item of a crate outside the model.
    Outside,
}

/// A module, with the items its features enable.
pub struct Module {
    pub parent: Option<usize>,
    /// The file that holds it, as a path from the directory that holds its package.
    pub file: String,
    pub items: Vec<Item>,
    /// The module each `mod` item declares, by the item's index.
    children: BTreeMap<usize, usize>,
}

/// A crate of the model.
pub struct Crate {
    /// The name code gives the crate, such as `sqlx_core`.
    pub name: String,
    proc_macro: bool,
    features: BTreeSet<String>,
    /// The crates it depends on, by the name its code gives each: the crate's index in the
    /// model, or `None` for a crate outside it.
    dependencies: BTreeMap<String, Option<usize>>,
    pub modules: Vec<Module>,
    /// Its `#[macro_export]` macros, which its root holds whatever module defines them: the
    /// index of the module that defines each, and the macro's index among its items.
    macros: BTreeMap<String, (usize, usize)>,
}

/// The crates of the model; the first is `sqlx`.
pub struct Model {
    pub crates: Vec<Crate>,
}

/// One name a `use` item imports: the path it imports, and the name it binds, or `None` for a
/// glob.
struct Import {
    path: Vec<String>,
    binding: Option<String>,
}

/// Reads a file of a package.
pub type Read<'a> = &'a dyn Fn(&Path) -> io::Result<String>;

impl Model {
    pub fn krate(&self, krate: usize) -> Option<&Crate> {
        self.crates.get(krate)
    }

    /// Whether the crate is a procedural macro crate, whose functions are macros.
    pub fn is_proc_macro(&self, krate: usize) -> bool {
        self.krate(krate).is_some_and(|krate| krate.proc_macro)
    }

    pub fn module(&self, id: ModuleId) -> Option<&Module> {
        self.krate(id.krate)?.modules.get(id.index)
    }

    pub fn item(&self, id: ItemId) -> Option<&Item> {
        self.module(id.module)?.items.get(id.index)
    }
}

/// The name an identifier gives, without the `r#` of a raw identifier, as paths name it.
pub fn name(ident: &syn::Ident) -> String {
    syn::ext::IdentExt::unraw(ident).to_string()
}

/// An item's attributes.
fn attrs(item: &Item) -> &[syn::Attribute] {
    match item {
        Item::Const(inner) => &inner.attrs,
        Item::Enum(inner) => &inner.attrs,
        Item::ExternCrate(inner) => &inner.attrs,
        Item::Fn(inner) => &inner.attrs,
        Item::ForeignMod(inner) => &inner.attrs,
        Item::Impl(inner) => &inner.attrs,
        Item::Macro(inner) => &inner.attrs,
        Item::Mod(inner) => &inner.attrs,
        Item::Static(inner) => &inner.attrs,
        Item::Struct(inner) => &inner.attrs,
        Item::Trait(inner) => &inner.attrs,
        Item::TraitAlias(inner) => &inner.attrs,
        Item::Type(inner) => &inner.attrs,
        Item::Union(inner) => &inner.attrs,
        Item::Use(inner) => &inner.attrs,
        _ => &[],
    }
}

/// The names a `use` tree imports.
fn imports(tree: &UseTree) -> Vec<Import> {
    let mut found = Vec::new();
    collect(tree, &mut Vec::new(), &mut found);
    found
}

fn collect(tree: &UseTree, prefix: &mut Vec<String>, found: &mut Vec<Import>) {
    let mut bind = |name: &syn::Ident, binding: &syn::Ident| {
        let mut path = prefix.clone();
        if name != "self" {
            path.push(self::name(name));
        }
        let binding = if binding == "self" {
            prefix.last().cloned()
        } else {
            Some(self::name(binding))
        };
        found.push(Import { path, binding });
    };
    match tree {
        UseTree::Path(inner) => {
            prefix.push(name(&inner.ident));
            collect(&inner.tree, prefix, found);
            prefix.pop();
        }
        UseTree::Name(name) => bind(&name.ident, &name.ident),
        UseTree::Rename(rename) => bind(&rename.ident, &rename.rename),
        UseTree::Glob(_) => found.push(Import {
            path: prefix.clone(),
            binding: None,
        }),
        UseTree::Group(group) => {
            for inner in &group.items {
                collect(inner, prefix, found);
            }
        }
    }
}
