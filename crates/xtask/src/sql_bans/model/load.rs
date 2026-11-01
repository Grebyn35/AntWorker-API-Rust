//! Reading the crates of the model: each module's file, with the items its features enable.

use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
};

use syn::Item;

use super::{Crate, Model, Module, Read, attrs, imports, name};
use crate::sql_bans::{
    cargo::{Package, Packages},
    cfg,
};

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
                    && let Some(ident) = &definition.ident
                    && definition.mac.path.is_ident("macro_rules")
                    && definition
                        .attrs
                        .iter()
                        .any(|attr| attr.path().is_ident("macro_export"))
                {
                    macros.insert(name(ident), (index, position));
                }
            }
        }
        macros
    }
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
            let child_name = name(&declaration.ident);
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
        let module = name(&declaration.ident);
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
        let directory = place.directory.join(&module);
        let flat = place.directory.join(format!("{module}.rs"));
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
