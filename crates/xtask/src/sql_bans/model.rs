//! The sqlx crates' modules and names: what a path reaches through modules, `use` imports and
//! re-exports (renames and globs among them), in the items that the features of the lock enable
//! ([`cfg`]).
//!
//! The model holds `sqlx` and the crates of the sqlx project it names in a `use` or an `extern
//! crate`: packages named `sqlx-...`, such as `sqlx-core` and `sqlx-postgres`. A path into any
//! other crate reaches [`Def::Outside`]. A procedural macro crate exports only its macros, so the
//! model reads only its root file.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
};

use syn::{Item, UseTree, Visibility};

use super::{
    cargo::{Package, Packages},
    cfg,
};

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

/// The lookups under way, so a cycle of imports ends.
type Guard = BTreeSet<(ModuleId, String, Ns, bool)>;

/// Reads a file of a package.
pub type Read<'a> = &'a dyn Fn(&Path) -> io::Result<String>;

/// Whether the package is one of the sqlx project's.
fn of_sqlx(package: &Package) -> bool {
    package.name == "sqlx" || package.name.starts_with("sqlx-")
}

impl Model {
    /// Reads `sqlx` among `packages`, and the crates of the sqlx project it names, with `read`.
    pub fn load(packages: &Packages, read: Read<'_>) -> Result<Self, String> {
        let mut crates = Vec::new();
        let mut ids: Vec<String> = Vec::new();
        let mut queue = vec![packages.sqlx.clone()];
        while let Some(id) = queue.pop() {
            if ids.contains(&id) {
                continue;
            }
            let package = packages
                .packages
                .get(&id)
                .ok_or_else(|| format!("cargo metadata doesn't describe the package {id}"))?;
            let krate = Crate::load(package, read)?;
            for name in krate.named_crates() {
                if let Some(dependency) = package.dependencies.get(&name)
                    && packages.packages.get(dependency).is_some_and(of_sqlx)
                {
                    queue.push(dependency.clone());
                }
            }
            ids.push(id);
            crates.push(krate);
        }
        for (krate, id) in crates.iter_mut().zip(&ids) {
            if let Some(package) = packages.packages.get(id) {
                krate.dependencies = package
                    .dependencies
                    .iter()
                    .map(|(name, dependency)| {
                        (name.clone(), ids.iter().position(|id| id == dependency))
                    })
                    .collect();
            }
        }
        Ok(Self { crates })
    }

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
    /// Reads the crate of `package`, from its root file.
    fn load(package: &Package, read: Read<'_>) -> Result<Self, String> {
        let mut krate = Self {
            name: package.name.replace('-', "_"),
            proc_macro: package.proc_macro,
            features: package.features.clone(),
            dependencies: BTreeMap::new(),
            modules: Vec::new(),
            macros: BTreeMap::new(),
        };
        let base = package.directory.parent().unwrap_or(&package.directory);
        let loader = Loader { base, read };
        let directory = package
            .root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        loader.file(&mut krate, &package.root, &directory, None)?;
        krate.macros = krate.exported_macros();
        Ok(krate)
    }

    fn dependency(&self, name: &str) -> Def {
        match self.dependencies.get(name) {
            Some(Some(krate)) => Def::Module(ModuleId {
                krate: *krate,
                index: 0,
            }),
            _ => Def::Outside,
        }
    }

    /// The crates its `use` and `extern crate` items name.
    fn named_crates(&self) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        if self.proc_macro {
            return names;
        }
        for item in self.modules.iter().flat_map(|module| &module.items) {
            match item {
                Item::Use(import) => {
                    names.extend(
                        imports(&import.tree)
                            .into_iter()
                            .filter_map(|one| one.path.into_iter().next()),
                    );
                }
                Item::ExternCrate(extern_crate) => {
                    names.insert(name(&extern_crate.ident));
                }
                _ => {}
            }
        }
        names
    }

    fn exported_macros(&self) -> BTreeMap<String, (usize, usize)> {
        let mut macros = BTreeMap::new();
        for (index, module) in self.modules.iter().enumerate() {
            for (position, item) in module.items.iter().enumerate() {
                if let Item::Macro(definition) = item
                    && let Some(name) = &definition.ident
                    && definition.mac.path.is_ident("macro_rules")
                    && definition
                        .attrs
                        .iter()
                        .any(|attr| attr.path().is_ident("macro_export"))
                {
                    macros.insert(self::name(name), (index, position));
                }
            }
        }
        macros
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

/// Reads a crate's modules from its files.
struct Loader<'a> {
    /// The directory that holds the package's directory, which locations are given from.
    base: &'a Path,
    read: Read<'a>,
}

impl Loader<'_> {
    /// Reads the module in `file`, whose `mod` items without a path name files in `directory`.
    fn file(
        &self,
        krate: &mut Crate,
        file: &Path,
        directory: &Path,
        parent: Option<usize>,
    ) -> Result<usize, String> {
        let shown = self.shown(file);
        let text = (self.read)(file).map_err(|error| format!("can't read {shown}: {error}"))?;
        let parsed =
            syn::parse_file(&text).map_err(|error| format!("can't parse {shown}: {error}"))?;
        let here = file.parent().map(Path::to_path_buf).unwrap_or_default();
        let place = Place {
            file,
            directory,
            here: &here,
        };
        self.module(krate, parsed.items, &place, parent)
    }

    /// Adds the module of `items`, with the modules it declares.
    fn module(
        &self,
        krate: &mut Crate,
        mut items: Vec<Item>,
        place: &Place<'_>,
        parent: Option<usize>,
    ) -> Result<usize, String> {
        items.retain(|item| cfg::active(attrs(item), &krate.features));
        for item in &mut items {
            prune(item, &krate.features);
        }
        let index = krate.modules.len();
        krate.modules.push(Module {
            parent,
            file: self.shown(place.file),
            items: Vec::new(),
            children: BTreeMap::new(),
        });
        let mut children = BTreeMap::new();
        for (position, item) in items.iter_mut().enumerate() {
            let Item::Mod(declaration) = item else {
                continue;
            };
            let child_name = self::name(&declaration.ident);
            let child = if let Some((brace, inner)) = declaration.content.take() {
                declaration.content = Some((brace, Vec::new()));
                let directory = place.directory.join(&child_name);
                let inner_place = Place {
                    directory: &directory,
                    here: &directory,
                    ..*place
                };
                self.module(krate, inner, &inner_place, Some(index))?
            } else if krate.proc_macro {
                continue;
            } else {
                let (file, directory) = self.declared(declaration, place)?;
                self.file(krate, &file, &directory, Some(index))?
            };
            children.insert(position, child);
        }
        if let Some(module) = krate.modules.get_mut(index) {
            module.items = items;
            module.children = children;
        }
        Ok(index)
    }

    /// The file that the `mod` item `declaration` names, and the directory of its own `mod`
    /// items' files.
    fn declared(
        &self,
        declaration: &syn::ItemMod,
        place: &Place<'_>,
    ) -> Result<(PathBuf, PathBuf), String> {
        let name = self::name(&declaration.ident);
        let explicit = declaration.attrs.iter().find_map(|attr| match &attr.meta {
            syn::Meta::NameValue(value) if value.path.is_ident("path") => match &value.value {
                syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(path),
                    ..
                }) => Some(path.value()),
                _ => None,
            },
            _ => None,
        });
        if let Some(path) = explicit {
            let file = place.here.join(path);
            let directory = match file.file_stem() {
                Some(stem) if file.file_name().is_some_and(|name| name != "mod.rs") => {
                    file.with_file_name(stem)
                }
                _ => file.parent().map(Path::to_path_buf).unwrap_or_default(),
            };
            return Ok((file, directory));
        }
        let directory = place.directory.join(&name);
        let flat = place.directory.join(format!("{name}.rs"));
        let nested = directory.join("mod.rs");
        match (self.read)(&flat) {
            Ok(_) => Ok((flat, directory)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok((nested, directory)),
            Err(error) => Err(format!("can't read {}: {error}", self.shown(&flat))),
        }
    }

    /// `file` as a path from the directory that holds its package, with `/` between parts.
    fn shown(&self, file: &Path) -> String {
        let relative = file.strip_prefix(self.base).unwrap_or(file);
        relative
            .components()
            .map(|part| part.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    }
}

/// Where a module's items are: the file, the directory of the files its `mod` items name, and
/// the directory a `#[path]` is relative to.
struct Place<'a> {
    file: &'a Path,
    directory: &'a Path,
    here: &'a Path,
}

/// Drops the items of an impl block or a trait that the features don't enable.
fn prune(item: &mut Item, features: &BTreeSet<String>) {
    match item {
        Item::Impl(block) => block.items.retain(|inner| match inner {
            syn::ImplItem::Fn(function) => cfg::active(&function.attrs, features),
            syn::ImplItem::Const(constant) => cfg::active(&constant.attrs, features),
            syn::ImplItem::Type(alias) => cfg::active(&alias.attrs, features),
            syn::ImplItem::Macro(invocation) => cfg::active(&invocation.attrs, features),
            _ => true,
        }),
        Item::Trait(trait_) => trait_.items.retain(|inner| match inner {
            syn::TraitItem::Fn(function) => cfg::active(&function.attrs, features),
            syn::TraitItem::Const(constant) => cfg::active(&constant.attrs, features),
            syn::TraitItem::Type(alias) => cfg::active(&alias.attrs, features),
            syn::TraitItem::Macro(invocation) => cfg::active(&invocation.attrs, features),
            _ => true,
        }),
        _ => {}
    }
}

/// The name an identifier gives, without the `r#` of a raw identifier, as paths name it.
pub fn name(ident: &syn::Ident) -> String {
    syn::ext::IdentExt::unraw(ident).to_string()
}

/// An item's attributes.
pub fn attrs(item: &Item) -> &[syn::Attribute] {
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
