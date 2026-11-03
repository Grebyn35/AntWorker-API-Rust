//! `check-layout`, for CODE-14: an item of `platform`'s public API that no code outside the tests
//! names, and that the platform spec's Interface doesn't name either, is dead. The tests' own
//! items, which only `#[cfg(test)]` or the `test-support` feature compiles, are the tests'.
//!
//! An item is found by its name: it is used when its name occurs in the code of `platform` or
//! `app` outside the tests, its strings and macros included, more often than the code defines
//! that name. So an item whose name another item shares can pass unused, which review finds.

use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::{TokenStream, TokenTree};
use syn::{
    Attribute, ImplItem, Item, LitStr, Macro, Visibility,
    visit::{self, Visit},
};

use crate::{Finding, repo::Repo};

/// The code of `platform`, whose public items this reads.
const PLATFORM: &str = "crates/platform/src/";

/// The code that may use them: `platform`'s and `app`'s.
const CODE: [&str; 2] = [PLATFORM, "crates/app/src/"];

/// The platform spec, whose Interface names the items modules are built against.
const SPEC: &str = "docs/spec/platform.md";

/// A public item: its name, file and line.
struct Public {
    name: String,
    path: String,
    line: usize,
}

/// A finding for each public item of `platform` that no code outside the tests names, and the
/// Interface doesn't.
pub fn check(repo: &Repo) -> Vec<Finding> {
    let files: Vec<(&str, syn::File)> = repo
        .files()
        .filter(|file| {
            CODE.iter().any(|root| file.path.starts_with(root)) && file.path.ends_with(".rs")
        })
        // check-docs and the compiler report a file that doesn't parse.
        .filter_map(|file| Some((file.path.as_str(), syn::parse_file(&file.text).ok()?)))
        .collect();
    let gated_modules: BTreeSet<String> = files
        .iter()
        .flat_map(|(path, parsed)| gated_declarations(path, &parsed.items))
        .collect();
    let mut reader = Reader::default();
    for (path, parsed) in &files {
        let module = path.strip_suffix(".rs").unwrap_or(path);
        if gated_modules
            .iter()
            .any(|gated| module == gated || module.starts_with(&format!("{gated}/")))
        {
            continue;
        }
        reader.path = (*path).to_owned();
        reader.platform = path.starts_with(PLATFORM);
        reader.visit_file(parsed);
    }
    let interface = repo
        .file(SPEC)
        .map(|spec| interface_names(&spec.text))
        .unwrap_or_default();
    let Reader {
        public,
        defined,
        words,
        ..
    } = reader;
    public
        .into_iter()
        .filter(|item| {
            let uses = words.get(&item.name).copied().unwrap_or_default();
            uses <= defined.get(&item.name).copied().unwrap_or_default()
                && !interface.contains(&item.name)
        })
        .map(|item| {
            Finding::new(
                &item.path,
                item.line,
                format!(
                    "`{}` is public, but no code outside the tests names it, and the platform \
                     spec's Interface doesn't: delete it, or compile it only with the \
                     `test-support` feature if the tests need it (CODE-14)",
                    item.name
                ),
            )
        })
        .collect()
}

/// Reads the code outside the tests: the public items of `platform`, how often each name is
/// defined, and how often each name occurs, its own definitions included.
#[derive(Default)]
struct Reader {
    path: String,
    platform: bool,
    public: Vec<Public>,
    defined: BTreeMap<String, usize>,
    words: BTreeMap<String, usize>,
}

impl Reader {
    /// Counts a definition of `ident`, and records it as public when it is, in `platform`.
    fn define(&mut self, vis: &Visibility, ident: &syn::Ident) {
        let name = ident.to_string();
        *self.defined.entry(name.clone()).or_default() += 1;
        if self.platform && matches!(vis, Visibility::Public(_)) {
            self.public.push(Public {
                name,
                path: self.path.clone(),
                line: ident.span().start().line,
            });
        }
    }

    fn count(&mut self, word: &str) {
        *self.words.entry(word.to_owned()).or_default() += 1;
    }

    /// Counts the names in `tokens`, a macro's or an attribute's, and in their strings.
    fn tokens(&mut self, tokens: TokenStream) {
        for token in tokens {
            match token {
                TokenTree::Ident(ident) => self.count(&ident.to_string()),
                TokenTree::Literal(literal) => {
                    for word in identifiers(&literal.to_string()) {
                        self.count(word);
                    }
                }
                TokenTree::Group(group) => self.tokens(group.stream()),
                TokenTree::Punct(_) => {}
            }
        }
    }
}

impl<'ast> Visit<'ast> for Reader {
    fn visit_item(&mut self, item: &'ast Item) {
        if for_tests(attributes(item)) {
            return;
        }
        match item {
            Item::Fn(function) => self.define(&function.vis, &function.sig.ident),
            Item::Struct(item) => self.define(&item.vis, &item.ident),
            Item::Enum(item) => self.define(&item.vis, &item.ident),
            Item::Trait(item) => self.define(&item.vis, &item.ident),
            Item::Type(item) => self.define(&item.vis, &item.ident),
            Item::Const(item) => self.define(&item.vis, &item.ident),
            Item::Static(item) => self.define(&item.vis, &item.ident),
            // A re-export names an item without using it.
            Item::Use(_) => return,
            Item::Impl(block) => {
                // The type an impl block is of isn't a use of it, so read the trait it
                // implements, the bounds of its generics and its items, and not its type.
                if let Some((path, _)) = &block.trait_ {
                    self.visit_path(path);
                }
                self.visit_generics(&block.generics);
                for member in &block.items {
                    self.visit_impl_item(member);
                }
                for member in &block.items {
                    let (attrs, vis, ident) = match member {
                        ImplItem::Fn(function) => {
                            (&function.attrs, &function.vis, &function.sig.ident)
                        }
                        ImplItem::Const(constant) => {
                            (&constant.attrs, &constant.vis, &constant.ident)
                        }
                        _ => continue,
                    };
                    if for_tests(attrs) {
                        continue;
                    }
                    if block.trait_.is_none() {
                        self.define(vis, ident);
                    } else {
                        // A trait's method is the trait's: it is used where the trait is.
                        *self.defined.entry(ident.to_string()).or_default() += 1;
                    }
                }
                for attr in &block.attrs {
                    self.visit_attribute(attr);
                }
                return;
            }
            _ => {}
        }
        visit::visit_item(self, item);
    }

    fn visit_impl_item(&mut self, member: &'ast ImplItem) {
        let attrs = match member {
            ImplItem::Fn(function) => &function.attrs,
            ImplItem::Const(constant) => &constant.attrs,
            ImplItem::Type(alias) => &alias.attrs,
            _ => return visit::visit_impl_item(self, member),
        };
        if !for_tests(attrs) {
            visit::visit_impl_item(self, member);
        }
    }

    fn visit_attribute(&mut self, attr: &'ast Attribute) {
        // A doc comment names things in prose, which uses nothing.
        if attr.path().is_ident("doc") {
            return;
        }
        if let Ok(list) = attr.meta.require_list() {
            self.tokens(list.tokens.clone());
        }
        visit::visit_attribute(self, attr);
    }

    fn visit_ident(&mut self, ident: &'ast proc_macro2::Ident) {
        self.count(&ident.to_string());
    }

    fn visit_lit_str(&mut self, literal: &'ast LitStr) {
        for word in identifiers(&literal.value()) {
            self.count(word);
        }
    }

    fn visit_macro(&mut self, mac: &'ast Macro) {
        self.tokens(mac.tokens.clone());
        visit::visit_macro(self, mac);
    }
}

/// The modules, by the paths of their files without `.rs`, that the file at `path` declares and
/// only the tests compile.
fn gated_declarations(path: &str, items: &[Item]) -> Vec<String> {
    let directory = path
        .strip_suffix("/mod.rs")
        .or_else(|| path.strip_suffix("/lib.rs"))
        .or_else(|| path.strip_suffix("/main.rs"))
        .or_else(|| path.strip_suffix(".rs"))
        .unwrap_or(path);
    items
        .iter()
        .filter_map(|item| match item {
            Item::Mod(module) if module.content.is_none() && for_tests(&module.attrs) => {
                Some(format!("{directory}/{}", module.ident))
            }
            _ => None,
        })
        .collect()
}

fn attributes(item: &Item) -> &[Attribute] {
    match item {
        Item::Const(item) => &item.attrs,
        Item::Enum(item) => &item.attrs,
        Item::Fn(item) => &item.attrs,
        Item::Impl(item) => &item.attrs,
        Item::Mod(item) => &item.attrs,
        Item::Static(item) => &item.attrs,
        Item::Struct(item) => &item.attrs,
        Item::Trait(item) => &item.attrs,
        Item::Type(item) => &item.attrs,
        Item::Use(item) => &item.attrs,
        _ => &[],
    }
}

/// Whether `attrs` compile the item only for the tests: `#[cfg(test)]`, or a `cfg` that names
/// the `test-support` feature.
fn for_tests(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        attr.path().is_ident("cfg")
            && attr.meta.require_list().is_ok_and(|list| {
                let tokens = list.tokens.to_string();
                tokens == "test" || tokens.contains("\"test-support\"")
            })
    })
}

/// The identifiers of `code`, in its strings as well as outside them.
fn identifiers(code: &str) -> impl Iterator<Item = &str> {
    code.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|word| word.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_'))
}

/// The names in the code spans of the platform spec's Interface section.
fn interface_names(spec: &str) -> BTreeSet<String> {
    let start = spec.find("\n## Interface").unwrap_or(spec.len());
    let section = &spec[start..];
    let end = section[1..]
        .find("\n## ")
        .map_or(section.len(), |end| end + 1);
    section[..end]
        .split('`')
        .skip(1)
        .step_by(2)
        .flat_map(identifiers)
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn findings(files: &[(&str, &str)]) -> Vec<String> {
        check(&Repo::of(files))
            .into_iter()
            .map(|finding| format!("{}:{}: {}", finding.path, finding.line, finding.message))
            .map(|text| {
                text.split(" is public")
                    .next()
                    .unwrap_or_default()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn code_14_a_public_item_that_only_tests_name_is_dead() {
        let url = "pub struct Url;\npub struct Reexported;\n\
                   impl Url {\n\
                   \x20   pub fn database(&self) {}\n\
                   \x20   pub fn with_database(&self) {}\n\
                   \x20   pub fn named_by_the_interface(&self) {}\n\
                   \x20   #[cfg(feature = \"test-support\")]\n\
                   \x20   pub fn for_tests(&self) {}\n\
                   }\n\
                   #[cfg(test)]\n\
                   mod tests {\n\
                   \x20   fn uses() { super::Url.with_database(); }\n\
                   }\n";
        let app = "fn main() { platform::url::Url.database(); }\n";
        let lib = "pub mod url;\npub use url::Reexported;\n\
                   pub struct WithImpl;\nimpl WithImpl {\n    fn new() {}\n}\n\
                   pub trait Shape {}\nimpl Shape for Url {}\n";
        let spec = "# Platform\n\n## Interface\n\n- `Url::named_by_the_interface`\n\n\
                    ## Behaviors\n\n`with_database`\n";

        let found = findings(&[
            ("crates/platform/src/lib.rs", lib),
            ("crates/platform/src/url.rs", url),
            ("crates/app/src/main.rs", app),
            (SPEC, spec),
        ]);

        assert_eq!(
            found,
            [
                "crates/platform/src/lib.rs:3: `WithImpl`",
                "crates/platform/src/url.rs:2: `Reexported`",
                "crates/platform/src/url.rs:5: `with_database`",
            ]
        );
    }

    #[test]
    fn code_14_the_items_of_a_module_for_the_tests_are_the_tests() {
        let lib = "#[cfg(feature = \"test-support\")]\npub mod testing;\npub fn used() {}\n";
        let testing = "pub fn helper() { crate::used(); }\n";
        let inner = "pub fn nested() {}\n";
        let app = "fn main() { platform::used(); }\n";

        let found = findings(&[
            ("crates/platform/src/lib.rs", lib),
            ("crates/platform/src/testing.rs", testing),
            ("crates/platform/src/testing/inner.rs", inner),
            ("crates/app/src/main.rs", app),
        ]);

        assert!(found.is_empty(), "{found:?}");
    }
}
