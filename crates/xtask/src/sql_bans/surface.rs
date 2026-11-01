//! What code that depends on sqlx can call: every function and macro that a path from sqlx's
//! root reaches through public modules and re-exports, every public method of an impl block of a
//! type it reaches, wherever in the type's crate the block is, and every method of a trait it
//! reaches. A trait method is banned by the trait's path, which covers every implementation, so
//! the impls of traits add nothing.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use syn::Item;

use super::model::{Def, ItemId, Model, ModuleId, Ns, name};

/// A function, method or macro that code depending on sqlx can call.
pub struct Callable<'a> {
    pub def: Def,
    /// The shortest path from sqlx's root that reaches it, which clippy's bans name.
    pub path: String,
    /// Where it is defined: its file, from the directory that holds its package, and line.
    pub location: String,
    pub kind: Kind<'a>,
}

pub enum Kind<'a> {
    Function(Function<'a>),
    /// A `macro_rules!` macro, and the module it is defined in.
    Macro(&'a syn::ItemMacro, ModuleId),
}

/// A function or method, with what its signature's paths are resolved against.
pub struct Function<'a> {
    pub sig: &'a syn::Signature,
    /// The module whose names the signature's paths name.
    pub scope: ModuleId,
    /// The type or trait that holds a method.
    pub owner: Option<ItemId>,
    /// The generics of the impl block or trait that holds a method.
    pub outer: Option<&'a syn::Generics>,
}

/// Every function, method and macro that code depending on sqlx can call.
pub fn callables(model: &Model) -> Vec<Callable<'_>> {
    let paths = paths(model);
    let impls = inherent_impls(model);
    let mut found = Vec::new();
    for (def, path) in &paths {
        let Def::Item(id) = def else {
            continue;
        };
        let (Some(item), Some(module)) = (model.item(*id), model.module(id.module)) else {
            continue;
        };
        let at = |ident: &syn::Ident| format!("{}:{}", module.file, ident.span().start().line);
        match item {
            // A procedural macro crate's functions are macros, whose expansion its code
            // computes, so nothing in their signatures tells what they take.
            Item::Fn(function) if !model.is_proc_macro(id.module.krate) => {
                found.push(Callable {
                    def: *def,
                    path: path.join("::"),
                    location: at(&function.sig.ident),
                    kind: Kind::Function(Function {
                        sig: &function.sig,
                        scope: id.module,
                        owner: None,
                        outer: None,
                    }),
                });
            }
            Item::Macro(definition) => {
                if let Some(ident) = &definition.ident {
                    found.push(Callable {
                        def: *def,
                        path: path.join("::"),
                        location: at(ident),
                        kind: Kind::Macro(definition, id.module),
                    });
                }
            }
            Item::Trait(trait_) => found.extend(trait_methods(model, *id, trait_, path)),
            Item::Struct(_) | Item::Enum(_) | Item::Union(_) => {
                for block in impls.get(id).map_or(&[][..], Vec::as_slice) {
                    found.extend(methods(model, *id, *block, path));
                }
            }
            _ => {}
        }
    }
    found
}

/// Each item that a path from sqlx's root reaches, with the shortest such path, by breadth from
/// the root, so the first path found is a shortest.
pub fn paths(model: &Model) -> BTreeMap<Def, Vec<String>> {
    let root = ModuleId { krate: 0, index: 0 };
    let name = model
        .krate(0)
        .map_or_else(String::new, |krate| krate.name.clone());
    let mut paths = BTreeMap::from([(Def::Module(root), vec![name.clone()])]);
    let mut queue = VecDeque::from([(root, vec![name])]);
    let mut visited = BTreeSet::from([root]);
    while let Some((module, prefix)) = queue.pop_front() {
        for (name, ns) in model.names(module, true) {
            for def in model.lookup(module, &name, ns, true) {
                if def == Def::Outside {
                    continue;
                }
                let mut path = prefix.clone();
                path.push(name.clone());
                paths.entry(def).or_insert_with(|| path.clone());
                if let Def::Module(child) = def
                    && visited.insert(child)
                {
                    queue.push_back((child, path));
                }
            }
        }
    }
    paths
}

/// For each type of the model, its impl blocks without a trait, wherever in its crate they are.
pub fn inherent_impls(model: &Model) -> BTreeMap<ItemId, Vec<ItemId>> {
    let mut impls: BTreeMap<ItemId, Vec<ItemId>> = BTreeMap::new();
    for (krate, holder) in model.crates.iter().enumerate() {
        for (index, module) in holder.modules.iter().enumerate() {
            let module_id = ModuleId { krate, index };
            for (position, item) in module.items.iter().enumerate() {
                if let Item::Impl(block) = item
                    && block.trait_.is_none()
                {
                    let id = ItemId {
                        module: module_id,
                        index: position,
                    };
                    for target in type_defs(model, module_id, &block.self_ty, 0) {
                        impls.entry(target).or_default().push(id);
                    }
                }
            }
        }
    }
    impls
}

/// The structs, enums, unions and traits that `ty` names in `scope`, through type aliases.
pub fn type_defs(model: &Model, scope: ModuleId, ty: &syn::Type, depth: usize) -> Vec<ItemId> {
    let path = match ty {
        syn::Type::Path(path) if path.qself.is_none() => &path.path,
        syn::Type::Paren(inner) => return type_defs(model, scope, &inner.elem, depth),
        syn::Type::Group(inner) => return type_defs(model, scope, &inner.elem, depth),
        syn::Type::TraitObject(object) => match object.bounds.first() {
            Some(syn::TypeParamBound::Trait(bound)) => &bound.path,
            _ => return Vec::new(),
        },
        _ => return Vec::new(),
    };
    let segments: Vec<String> = path
        .segments
        .iter()
        .map(|segment| name(&segment.ident))
        .collect();
    let rooted = path.leading_colon.is_some();
    let mut found = Vec::new();
    for def in model.resolve(scope, &segments, rooted, Ns::Type) {
        let Def::Item(id) = def else {
            continue;
        };
        match model.item(id) {
            Some(Item::Struct(_) | Item::Enum(_) | Item::Union(_) | Item::Trait(_)) => {
                found.push(id);
            }
            // A chain of aliases ends; a cycle of them doesn't compile.
            Some(Item::Type(alias)) if depth < 16 => {
                found.extend(type_defs(model, id.module, &alias.ty, depth + 1));
            }
            _ => {}
        }
    }
    found
}

/// The public methods of the impl block `block` of the type `owner`, reached through `path`, the
/// type's path.
fn methods<'a>(
    model: &'a Model,
    owner: ItemId,
    block: ItemId,
    path: &[String],
) -> Vec<Callable<'a>> {
    let (Some(Item::Impl(items)), Some(module)) = (model.item(block), model.module(block.module))
    else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (index, inner) in items.items.iter().enumerate() {
        if let syn::ImplItem::Fn(function) = inner
            && matches!(function.vis, syn::Visibility::Public(_))
        {
            found.push(Callable {
                def: Def::Assoc(block, index),
                path: format!("{}::{}", path.join("::"), name(&function.sig.ident)),
                location: format!("{}:{}", module.file, function.sig.ident.span().start().line),
                kind: Kind::Function(Function {
                    sig: &function.sig,
                    scope: block.module,
                    owner: Some(owner),
                    outer: Some(&items.generics),
                }),
            });
        }
    }
    found
}

/// The methods of the trait `trait_`, reached through `path`, its own path.
fn trait_methods<'a>(
    model: &'a Model,
    id: ItemId,
    trait_: &'a syn::ItemTrait,
    path: &[String],
) -> Vec<Callable<'a>> {
    let Some(module) = model.module(id.module) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (index, inner) in trait_.items.iter().enumerate() {
        if let syn::TraitItem::Fn(function) = inner {
            found.push(Callable {
                def: Def::Assoc(id, index),
                path: format!("{}::{}", path.join("::"), name(&function.sig.ident)),
                location: format!("{}:{}", module.file, function.sig.ident.span().start().line),
                kind: Kind::Function(Function {
                    sig: &function.sig,
                    scope: id.module,
                    owner: Some(id),
                    outer: Some(&trait_.generics),
                }),
            });
        }
    }
    found
}
