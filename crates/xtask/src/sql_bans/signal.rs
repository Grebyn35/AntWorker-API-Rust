//! Which functions and macros take SQL text.
//!
//! A function takes SQL text when a parameter other than its receiver does:
//! - one typed with sqlx's SQL types: `SqlStr` or `AssertSqlSafe`, also inside another type
//!   such as `Option<SqlStr>`, or `impl SqlSafeStr`, or a type parameter of the function, its
//!   impl block or its trait bounded by `SqlSafeStr` or `Execute` (every `SqlSafeStr` is an
//!   `Execute`, so an executor's method takes a raw string through it);
//! - or one typed as text, such as `&str`, `impl Into<String>` or `impl Display`, whose name is
//!   one sqlx gives SQL text: `sql`, `query` and `statement`, which its SQL-typed parameters
//!   are named too, and `init` and `separator`, the opening fragment and the separator that
//!   `QueryBuilder` writes into its SQL. It counts in a function that can carry the text to the
//!   database: one with a receiver, or with another parameter or a result of a type of the sqlx
//!   crates. A function with neither, such as one that returns a checksum of SQL text, only
//!   reads it.
//!
//! The functions of the SQL types themselves, such as `SqlStr::from_static`, only make or read
//! a SQL value, which runs only through a function with a SQL-typed parameter, so they don't
//! count. A return type is never SQL text.
//!
//! A `macro_rules!` macro takes SQL text outside the checked macros when its body passes
//! `checked = false` to the macro that expands queries, as the `_unchecked` macros do, or names a
//! function that takes SQL text.

use std::collections::BTreeSet;

use proc_macro2::{TokenStream, TokenTree};

use super::{
    model::{Def, ItemId, Model, ModuleId, Ns, name},
    surface::{Callable, Function, Kind},
};

/// The names sqlx gives a parameter of SQL text.
const SQL_NAMES: [&str; 5] = ["sql", "query", "statement", "init", "separator"];

/// The traits whose bound makes a parameter text.
const TEXT_TRAITS: [&str; 2] = ["Display", "ToString"];

/// sqlx's SQL types, which the signal starts from, by their paths from sqlx's root.
const SQL_TYPES: [&str; 2] = ["SqlStr", "AssertSqlSafe"];
const SQL_TRAITS: [&str; 2] = ["SqlSafeStr", "Execute"];

/// sqlx's SQL types and the traits of SQL text, as the model resolves them.
pub struct Anchors {
    types: BTreeSet<ItemId>,
    traits: BTreeSet<ItemId>,
}

impl Anchors {
    /// Finds sqlx's SQL types and traits, which sqlx's root exports.
    pub fn find(model: &Model) -> Result<Self, String> {
        let find = |name: &str| {
            let root = ModuleId { krate: 0, index: 0 };
            let found: Vec<ItemId> = model
                .lookup(root, name, Ns::Type, true)
                .into_iter()
                .filter_map(|def| match def {
                    Def::Item(id) => Some(id),
                    _ => None,
                })
                .collect();
            if found.is_empty() {
                Err(format!(
                    "sqlx doesn't export `{name}`, from which the check tells which functions take SQL text"
                ))
            } else {
                Ok(found)
            }
        };
        let mut types = BTreeSet::new();
        for name in SQL_TYPES {
            types.extend(find(name)?);
        }
        let mut traits = BTreeSet::new();
        for name in SQL_TRAITS {
            traits.extend(find(name)?);
        }
        Ok(Self { types, traits })
    }

    /// Whether the type or trait is one of sqlx's SQL types or traits.
    pub fn holds(&self, id: ItemId) -> bool {
        self.types.contains(&id) || self.traits.contains(&id)
    }
}

/// The names of `function`'s parameters that take SQL text.
pub fn sql_parameters(model: &Model, anchors: &Anchors, function: &Function<'_>) -> Vec<String> {
    if function.owner.is_some_and(|owner| anchors.holds(owner)) {
        return Vec::new();
    }
    let context = Context {
        model,
        anchors,
        function,
    };
    let mut found = Vec::new();
    for (position, input) in function.sig.inputs.iter().enumerate() {
        let syn::FnArg::Typed(typed) = input else {
            continue;
        };
        let parameter = match &*typed.pat {
            syn::Pat::Ident(ident) => name(&ident.ident),
            _ => String::new(),
        };
        let named = SQL_NAMES.contains(&parameter.trim_start_matches('_'));
        if context.sql_typed(&typed.ty, 0)
            || (named && context.text(&typed.ty, 0) && context.carries(position))
        {
            found.push(parameter);
        }
    }
    found
}

/// What a signature's types are read against.
struct Context<'a> {
    model: &'a Model,
    anchors: &'a Anchors,
    function: &'a Function<'a>,
}

impl Context<'_> {
    /// Whether text in the parameter at `position` can reach the database: the function has a
    /// receiver, or another parameter or its result has a type of the sqlx crates, which can
    /// keep the text or run it. A function with neither, such as one that returns a checksum of
    /// SQL text, only reads it.
    fn carries(&self, position: usize) -> bool {
        let signature = self.function.sig;
        let others = signature
            .inputs
            .iter()
            .enumerate()
            .filter(|(index, _)| *index != position)
            .any(|(_, input)| match input {
                syn::FnArg::Receiver(_) => true,
                syn::FnArg::Typed(typed) => self.of_sqlx(&typed.ty, 0),
            });
        let result = match &signature.output {
            syn::ReturnType::Type(_, ty) => self.of_sqlx(ty, 0),
            syn::ReturnType::Default => false,
        };
        others || result
    }

    /// Whether `ty` names a type or trait of the sqlx crates, `Self` of a method among them.
    fn of_sqlx(&self, ty: &syn::Type, depth: usize) -> bool {
        if depth > 8 {
            return false;
        }
        let bounded = |bounds: &syn::punctuated::Punctuated<syn::TypeParamBound, _>| {
            bounds.iter().any(|bound| match bound {
                syn::TypeParamBound::Trait(bound) => self.path_of_sqlx(&bound.path, depth),
                _ => false,
            })
        };
        match ty {
            syn::Type::Path(path) if path.qself.is_none() => self.path_of_sqlx(&path.path, depth),
            syn::Type::ImplTrait(inner) => bounded(&inner.bounds),
            syn::Type::TraitObject(inner) => bounded(&inner.bounds),
            _ => inner_types(ty)
                .into_iter()
                .any(|inner| self.of_sqlx(inner, depth + 1)),
        }
    }

    fn path_of_sqlx(&self, path: &syn::Path, depth: usize) -> bool {
        (path.is_ident("Self") && self.function.owner.is_some())
            || !self.resolve(path).is_empty()
            || arguments(path)
                .into_iter()
                .any(|inner| self.of_sqlx(inner, depth + 1))
    }

    /// Whether `ty` names a SQL type, or a trait of SQL text as a bound.
    fn sql_typed(&self, ty: &syn::Type, depth: usize) -> bool {
        if depth > 8 {
            return false;
        }
        match ty {
            syn::Type::Path(path) if path.qself.is_none() => {
                self.sql_path(&path.path, depth)
                    || self
                        .generic_bounds(&path.path)
                        .iter()
                        .any(|bound| self.sql_bound(bound, depth))
            }
            syn::Type::ImplTrait(bounds) => bounds
                .bounds
                .iter()
                .any(|bound| self.sql_bound(bound, depth)),
            syn::Type::TraitObject(bounds) => bounds
                .bounds
                .iter()
                .any(|bound| self.sql_bound(bound, depth)),
            _ => inner_types(ty)
                .into_iter()
                .any(|inner| self.sql_typed(inner, depth + 1)),
        }
    }

    /// Whether `path` names a SQL type, or has one among its type arguments.
    fn sql_path(&self, path: &syn::Path, depth: usize) -> bool {
        self.resolve(path)
            .iter()
            .any(|id| self.anchors.types.contains(id))
            || arguments(path)
                .into_iter()
                .any(|inner| self.sql_typed(inner, depth + 1))
    }

    fn sql_bound(&self, bound: &syn::TypeParamBound, depth: usize) -> bool {
        let syn::TypeParamBound::Trait(bound) = bound else {
            return false;
        };
        self.resolve(&bound.path)
            .iter()
            .any(|id| self.anchors.holds(*id))
            || arguments(&bound.path)
                .into_iter()
                .any(|inner| self.sql_typed(inner, depth + 1))
    }

    /// Whether `ty` is text: `str` or `String` behind a reference or a smart pointer, or
    /// bounded by `Display`, `ToString`, or a trait whose arguments are text, such as
    /// `Into<String>` or `AsRef<str>`.
    fn text(&self, ty: &syn::Type, depth: usize) -> bool {
        if depth > 8 {
            return false;
        }
        match ty {
            syn::Type::Path(path) if path.qself.is_none() => {
                let last = path.path.segments.last().map(|last| name(&last.ident));
                matches!(last.as_deref(), Some("str" | "String"))
                    || (matches!(last.as_deref(), Some("Cow" | "Box" | "Arc" | "Rc"))
                        && arguments(&path.path)
                            .into_iter()
                            .any(|inner| self.text(inner, depth + 1)))
                    || self
                        .generic_bounds(&path.path)
                        .iter()
                        .any(|bound| self.text_bound(bound, depth))
            }
            syn::Type::ImplTrait(bounds) => bounds
                .bounds
                .iter()
                .any(|bound| self.text_bound(bound, depth)),
            syn::Type::Reference(reference) => self.text(&reference.elem, depth + 1),
            syn::Type::Paren(inner) => self.text(&inner.elem, depth + 1),
            syn::Type::Group(inner) => self.text(&inner.elem, depth + 1),
            _ => false,
        }
    }

    fn text_bound(&self, bound: &syn::TypeParamBound, depth: usize) -> bool {
        let syn::TypeParamBound::Trait(bound) = bound else {
            return false;
        };
        let last = bound.path.segments.last().map(|last| name(&last.ident));
        last.is_some_and(|last| TEXT_TRAITS.contains(&last.as_str()))
            || arguments(&bound.path)
                .into_iter()
                .any(|inner| self.text(inner, depth + 1))
    }

    /// The bounds of the type parameter that `path` names, if it names one of the function's,
    /// its impl block's or its trait's: inline and in `where` clauses.
    fn generic_bounds(&self, path: &syn::Path) -> Vec<&syn::TypeParamBound> {
        let Some(ident) = path.get_ident() else {
            return Vec::new();
        };
        let mut bounds = Vec::new();
        let all = [Some(&self.function.sig.generics), self.function.outer];
        for generics in all.into_iter().flatten() {
            for parameter in generics.type_params() {
                if parameter.ident == *ident {
                    bounds.extend(parameter.bounds.iter());
                }
            }
            let predicates = generics
                .where_clause
                .iter()
                .flat_map(|clause| clause.predicates.iter());
            for predicate in predicates {
                if let syn::WherePredicate::Type(predicate) = predicate
                    && let syn::Type::Path(bounded) = &predicate.bounded_ty
                    && bounded.path.get_ident() == Some(ident)
                {
                    bounds.extend(predicate.bounds.iter());
                }
            }
        }
        bounds
    }

    /// The types and traits `path` names in the function's module.
    fn resolve(&self, path: &syn::Path) -> Vec<ItemId> {
        let segments: Vec<String> = path
            .segments
            .iter()
            .map(|segment| name(&segment.ident))
            .collect();
        self.model
            .resolve(
                self.function.scope,
                &segments,
                path.leading_colon.is_some(),
                Ns::Type,
            )
            .into_iter()
            .filter_map(|def| match def {
                Def::Item(id) => Some(id),
                _ => None,
            })
            .collect()
    }
}

/// The types among a path's generic arguments, its associated types' among them.
fn arguments(path: &syn::Path) -> Vec<&syn::Type> {
    let mut found = Vec::new();
    for segment in &path.segments {
        if let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments {
            for argument in &arguments.args {
                match argument {
                    syn::GenericArgument::Type(ty) => found.push(ty),
                    syn::GenericArgument::AssocType(assoc) => found.push(&assoc.ty),
                    _ => {}
                }
            }
        }
    }
    found
}

/// The types directly inside `ty`: what a reference, slice, array, tuple or group holds.
fn inner_types(ty: &syn::Type) -> Vec<&syn::Type> {
    match ty {
        syn::Type::Reference(inner) => vec![&inner.elem],
        syn::Type::Slice(inner) => vec![&inner.elem],
        syn::Type::Array(inner) => vec![&inner.elem],
        syn::Type::Paren(inner) => vec![&inner.elem],
        syn::Type::Group(inner) => vec![&inner.elem],
        syn::Type::Ptr(inner) => vec![&inner.elem],
        syn::Type::Tuple(tuple) => tuple.elems.iter().collect(),
        _ => Vec::new(),
    }
}

/// The macros that take SQL text outside the checked macros, among `callables`, given the
/// functions that take SQL text.
pub fn sql_macros(
    model: &Model,
    callables: &[Callable<'_>],
    functions: &BTreeSet<Def>,
) -> BTreeSet<Def> {
    let mut found = BTreeSet::new();
    for callable in callables {
        let Kind::Macro(definition, module) = &callable.kind else {
            continue;
        };
        let scan = Scan::of(model, *module, definition.mac.tokens.clone());
        if scan.unchecked || !scan.paths.is_disjoint(functions) {
            found.insert(callable.def);
        }
    }
    found
}

/// What a macro's body names.
struct Scan {
    /// Whether it invokes a macro with `checked = false`.
    unchecked: bool,
    /// The items its paths reach, outside macro invocations' names.
    paths: BTreeSet<Def>,
}

impl Scan {
    fn of(model: &Model, scope: ModuleId, tokens: TokenStream) -> Self {
        let mut scan = Self {
            unchecked: false,
            paths: BTreeSet::new(),
        };
        scan.tokens(model, scope, tokens);
        scan
    }

    fn tokens(&mut self, model: &Model, scope: ModuleId, tokens: TokenStream) {
        let tokens: Vec<TokenTree> = tokens.into_iter().collect();
        let mut at = 0;
        while at < tokens.len() {
            // A metavariable, such as `$query`, names no item.
            if let (Some(TokenTree::Punct(dollar)), Some(TokenTree::Ident(variable))) =
                (tokens.get(at), tokens.get(at + 1))
                && dollar.as_char() == '$'
                && variable != "crate"
            {
                at += 2;
                continue;
            }
            let (path, end) = path_at(&tokens, at);
            if path.is_empty() {
                if let Some(TokenTree::Group(group)) = tokens.get(at) {
                    self.tokens(model, scope, group.stream());
                }
                at += 1;
                continue;
            }
            let invoked =
                matches!(tokens.get(end), Some(TokenTree::Punct(bang)) if bang.as_char() == '!');
            if invoked {
                if let Some(TokenTree::Group(arguments)) = tokens.get(end + 1) {
                    self.unchecked |= unchecked(arguments.stream());
                }
            } else {
                self.paths
                    .extend(model.resolve(scope, &path, false, Ns::Value));
            }
            at = end;
        }
    }
}

/// The path that starts at `tokens[at]`, `$crate` included, and the index after it; an empty
/// path when none starts there.
fn path_at(tokens: &[TokenTree], at: usize) -> (Vec<String>, usize) {
    let mut path = Vec::new();
    let mut index = at;
    if let (Some(TokenTree::Punct(dollar)), Some(TokenTree::Ident(krate))) =
        (tokens.get(index), tokens.get(index + 1))
        && dollar.as_char() == '$'
        && krate == "crate"
    {
        path.push("$crate".to_owned());
        index += 2;
    } else if let Some(TokenTree::Ident(first)) = tokens.get(index) {
        path.push(name(first));
        index += 1;
    } else {
        return (path, at);
    }
    while let (
        Some(TokenTree::Punct(first)),
        Some(TokenTree::Punct(second)),
        Some(TokenTree::Ident(next)),
    ) = (
        tokens.get(index),
        tokens.get(index + 1),
        tokens.get(index + 2),
    ) {
        if first.as_char() != ':' || second.as_char() != ':' {
            break;
        }
        path.push(name(next));
        index += 3;
    }
    (path, index)
}

/// Whether a macro invocation's arguments hold `checked = false`.
fn unchecked(arguments: TokenStream) -> bool {
    let tokens: Vec<TokenTree> = arguments.into_iter().collect();
    tokens.windows(3).any(|window| {
        matches!(
            window,
            [TokenTree::Ident(key), TokenTree::Punct(equals), TokenTree::Ident(value)]
                if key == "checked" && equals.as_char() == '=' && value == "false"
        )
    })
}
