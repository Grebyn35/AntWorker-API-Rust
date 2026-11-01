//! Following names: what a name or a path reaches in a module, through its items, its imports
//! and its globs, as rustc resolves them, and which names a module holds.

use std::collections::BTreeSet;

use syn::{Item, Visibility};

use super::{Crate, Def, ItemId, Model, ModuleId, NAMESPACES, Ns, imports, name};

/// The lookups under way, so a cycle of imports ends.
type Guard = BTreeSet<(ModuleId, String, Ns, bool)>;

impl Model {
    /// What `name` reaches in `ns` in `module`: with `public`, as code outside its crate sees
    /// it, which reaches only its public items and re-exports.
    pub fn lookup(&self, module: ModuleId, name: &str, ns: Ns, public: bool) -> Vec<Def> {
        self.lookup_in(module, name, ns, public, &mut Guard::new())
    }

    /// What `path` reaches in `ns`, resolved in `scope` as a `use` path or a path in code is,
    /// `::` before it when `rooted`.
    pub fn resolve(&self, scope: ModuleId, path: &[String], rooted: bool, ns: Ns) -> Vec<Def> {
        self.resolve_in(scope, path, rooted, ns, &mut Guard::new())
    }

    /// Every name `module` holds, with its namespace: with `public`, only those code outside its
    /// crate sees.
    pub fn names(&self, module: ModuleId, public: bool) -> BTreeSet<(String, Ns)> {
        self.names_in(module, public, &mut BTreeSet::new())
    }

    fn lookup_in(
        &self,
        module: ModuleId,
        name: &str,
        ns: Ns,
        public: bool,
        guard: &mut Guard,
    ) -> Vec<Def> {
        let key = (module, name.to_owned(), ns, public);
        if !guard.insert(key.clone()) {
            return Vec::new();
        }
        let mut found = self.explicit(module, name, ns, public, guard);
        // A glob imports only the names the module doesn't hold itself.
        if found.is_empty() {
            for target in self.globs(module, public, guard) {
                let public = public || target.krate != module.krate;
                found.extend(self.lookup_in(target, name, ns, public, guard));
            }
        }
        guard.remove(&key);
        found.sort();
        found.dedup();
        found
    }

    /// What `name` reaches through `module`'s own items and its imports that aren't globs.
    fn explicit(
        &self,
        module: ModuleId,
        name: &str,
        ns: Ns,
        public: bool,
        guard: &mut Guard,
    ) -> Vec<Def> {
        let (Some(krate), Some(holder)) = (self.krate(module.krate), self.module(module)) else {
            return Vec::new();
        };
        let mut found = Vec::new();
        if ns == Ns::Macro
            && module.index == 0
            && let Some((index, position)) = krate.macros.get(name)
        {
            let defining = ModuleId {
                index: *index,
                ..module
            };
            found.push(Def::Item(ItemId {
                module: defining,
                index: *position,
            }));
        }
        for (index, item) in holder.items.iter().enumerate() {
            if public && !is_public(item) {
                continue;
            }
            if let Item::Use(import) = item {
                for one in imports(&import.tree) {
                    if one.binding.as_deref() == Some(name) {
                        let rooted = import.leading_colon.is_some();
                        found.extend(self.resolve_in(module, &one.path, rooted, ns, guard));
                    }
                }
            } else if krate
                .defines(item)
                .iter()
                .any(|(defined, space)| defined == name && *space == ns)
            {
                found.extend(self.def_of(module, index, item));
            }
        }
        found
    }

    /// The modules that `module`'s glob imports read.
    fn globs(&self, module: ModuleId, public: bool, guard: &mut Guard) -> Vec<ModuleId> {
        let Some(holder) = self.module(module) else {
            return Vec::new();
        };
        let mut targets = Vec::new();
        for item in &holder.items {
            let Item::Use(import) = item else {
                continue;
            };
            if public && !is_public(item) {
                continue;
            }
            for one in imports(&import.tree) {
                if one.binding.is_none() {
                    let rooted = import.leading_colon.is_some();
                    for target in self.resolve_in(module, &one.path, rooted, Ns::Type, guard) {
                        if let Def::Module(target) = target {
                            targets.push(target);
                        }
                    }
                }
            }
        }
        targets
    }

    fn def_of(&self, module: ModuleId, index: usize, item: &Item) -> Option<Def> {
        let krate = self.krate(module.krate)?;
        match item {
            Item::Mod(_) => {
                let child = *self.module(module)?.children.get(&index)?;
                Some(Def::Module(ModuleId {
                    krate: module.krate,
                    index: child,
                }))
            }
            Item::ExternCrate(extern_crate) if extern_crate.ident == "self" => {
                Some(Def::Module(ModuleId {
                    krate: module.krate,
                    index: 0,
                }))
            }
            Item::ExternCrate(extern_crate) => Some(krate.dependency(&name(&extern_crate.ident))),
            _ => Some(Def::Item(ItemId { module, index })),
        }
    }

    fn resolve_in(
        &self,
        scope: ModuleId,
        path: &[String],
        rooted: bool,
        ns: Ns,
        guard: &mut Guard,
    ) -> Vec<Def> {
        let Some((first, rest)) = path.split_first() else {
            return Vec::new();
        };
        let root = ModuleId {
            krate: scope.krate,
            index: 0,
        };
        let mut current = match first.as_str() {
            _ if rooted => self.extern_crate(scope.krate, first),
            "crate" | "$crate" => vec![Def::Module(root)],
            "self" => vec![Def::Module(scope)],
            "super" => self.parent(scope).into_iter().collect(),
            _ => {
                let here = if rest.is_empty() { ns } else { Ns::Type };
                let local = self.lookup_in(scope, first, here, false, guard);
                if local.is_empty() {
                    self.extern_crate(scope.krate, first)
                } else {
                    local
                }
            }
        };
        for (position, segment) in rest.iter().enumerate() {
            let here = if position + 1 == rest.len() {
                ns
            } else {
                Ns::Type
            };
            let mut next = Vec::new();
            for def in &current {
                match def {
                    Def::Module(module) if segment == "super" => next.extend(self.parent(*module)),
                    Def::Module(module) => next.extend(self.lookup_in(
                        *module,
                        segment,
                        here,
                        module.krate != scope.krate,
                        guard,
                    )),
                    Def::Outside => next.push(Def::Outside),
                    // An enum's variants and the items of types and traits: none of them a
                    // function these crates import.
                    Def::Item(_) | Def::Assoc(..) => {}
                }
            }
            current = next;
        }
        current.sort();
        current.dedup();
        current
    }

    fn parent(&self, module: ModuleId) -> Option<Def> {
        let parent = self.module(module)?.parent?;
        Some(Def::Module(ModuleId {
            krate: module.krate,
            index: parent,
        }))
    }

    /// The root of the crate that `krate` calls `name`.
    fn extern_crate(&self, krate: usize, name: &str) -> Vec<Def> {
        match self.krate(krate) {
            Some(krate) if krate.dependencies.contains_key(name) => vec![krate.dependency(name)],
            _ if ["std", "core", "alloc"].contains(&name) => vec![Def::Outside],
            _ => Vec::new(),
        }
    }

    fn names_in(
        &self,
        module: ModuleId,
        public: bool,
        visiting: &mut BTreeSet<(ModuleId, bool)>,
    ) -> BTreeSet<(String, Ns)> {
        let mut names = BTreeSet::new();
        let (Some(krate), Some(holder)) = (self.krate(module.krate), self.module(module)) else {
            return names;
        };
        if !visiting.insert((module, public)) {
            return names;
        }
        if module.index == 0 {
            names.extend(krate.macros.keys().map(|name| (name.clone(), Ns::Macro)));
        }
        for item in &holder.items {
            if public && !is_public(item) {
                continue;
            }
            let Item::Use(import) = item else {
                names.extend(krate.defines(item));
                continue;
            };
            for one in imports(&import.tree) {
                match one.binding {
                    Some(binding) if binding != "_" => {
                        names.extend(NAMESPACES.iter().map(|ns| (binding.clone(), *ns)));
                    }
                    Some(_) => {}
                    None => {
                        let rooted = import.leading_colon.is_some();
                        let targets = self.resolve(module, &one.path, rooted, Ns::Type);
                        for target in targets {
                            if let Def::Module(target) = target {
                                let public = public || target.krate != module.krate;
                                names.extend(self.names_in(target, public, visiting));
                            }
                        }
                    }
                }
            }
        }
        visiting.remove(&(module, public));
        names
    }
}

impl Crate {
    fn dependency(&self, name: &str) -> Def {
        match self.dependencies.get(name) {
            Some(Some(krate)) => Def::Module(ModuleId {
                krate: *krate,
                index: 0,
            }),
            _ => Def::Outside,
        }
    }

    /// The names `item` defines, with their namespaces. A procedural macro crate exports only
    /// its macros.
    fn defines(&self, item: &Item) -> Vec<(String, Ns)> {
        let named = |ident: &syn::Ident, ns| vec![(name(ident), ns)];
        match item {
            Item::Fn(function) if self.proc_macro => proc_macro_names(function)
                .into_iter()
                .map(|name| (name, Ns::Macro))
                .collect(),
            _ if self.proc_macro => Vec::new(),
            Item::Fn(function) => named(&function.sig.ident, Ns::Value),
            Item::Const(constant) => named(&constant.ident, Ns::Value),
            Item::Static(statik) => named(&statik.ident, Ns::Value),
            Item::Struct(structure) => {
                let mut names = named(&structure.ident, Ns::Type);
                if !matches!(structure.fields, syn::Fields::Named(_)) {
                    names.extend(named(&structure.ident, Ns::Value));
                }
                names
            }
            Item::Enum(enumeration) => named(&enumeration.ident, Ns::Type),
            Item::Union(union) => named(&union.ident, Ns::Type),
            Item::Trait(trait_) => named(&trait_.ident, Ns::Type),
            Item::TraitAlias(alias) => named(&alias.ident, Ns::Type),
            Item::Type(alias) => named(&alias.ident, Ns::Type),
            Item::Mod(module) => named(&module.ident, Ns::Type),
            Item::ExternCrate(extern_crate) => named(
                extern_crate
                    .rename
                    .as_ref()
                    .map_or(&extern_crate.ident, |(_, rename)| rename),
                Ns::Type,
            ),
            _ => Vec::new(),
        }
    }
}

/// The names a procedural macro crate's function exports: its own for a function-like or
/// attribute macro, the derive's for a derive.
fn proc_macro_names(function: &syn::ItemFn) -> Vec<String> {
    let mut names = Vec::new();
    for attr in &function.attrs {
        if attr.path().is_ident("proc_macro") || attr.path().is_ident("proc_macro_attribute") {
            names.push(name(&function.sig.ident));
        } else if attr.path().is_ident("proc_macro_derive")
            && let Ok(derived) = attr.parse_args_with(|input: syn::parse::ParseStream<'_>| {
                let name: syn::Ident = input.parse()?;
                input.parse::<proc_macro2::TokenStream>()?;
                Ok(name)
            })
        {
            names.push(name(&derived));
        }
    }
    names
}

/// Whether code outside the item's crate can name it: `pub`, not `pub(crate)` or narrower.
fn is_public(item: &Item) -> bool {
    let visibility = match item {
        Item::Const(inner) => &inner.vis,
        Item::Enum(inner) => &inner.vis,
        Item::ExternCrate(inner) => &inner.vis,
        Item::Fn(inner) => &inner.vis,
        Item::Mod(inner) => &inner.vis,
        Item::Static(inner) => &inner.vis,
        Item::Struct(inner) => &inner.vis,
        Item::Trait(inner) => &inner.vis,
        Item::TraitAlias(inner) => &inner.vis,
        Item::Type(inner) => &inner.vis,
        Item::Union(inner) => &inner.vis,
        Item::Use(inner) => &inner.vis,
        _ => return false,
    };
    matches!(visibility, Visibility::Public(_))
}
