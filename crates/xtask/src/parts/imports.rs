//! The imports of each Rust file that a removal edits, which `check-parts` reads in the file as it
//! was and in the text that the removal keeps of it (ARCH-07), for two faults that a reading of
//! names finds without a build: a `use` that goes with a part's lines while kept code names what
//! it imported, and one that stays while only the lines that went named what it imports.
//!
//! What the check reads of a file's Rust, per file rather than per scope:
//!
//! - The names that a `use` item imports, wherever it stands, in a module, a nested module or a
//!   function's body: in the item's tree, a name imports itself, except `self`, which imports the
//!   name before it; a rename imports the name after its `as`, except `_`, which imports none; a
//!   glob imports none.
//! - The names that the code names, outside `use` items: the first name of each path that doesn't
//!   start with `::`, unless it is `self`, `super`, `crate` or `Self`; and each identifier among
//!   the tokens of a macro's invocation or of an attribute, through their groups, unless it comes
//!   right after a `.` that is no second `.` of a `..`, or is one of the standard library's
//!   derives in a `derive`, which names the prelude's macro whatever trait of that name a `use`
//!   imports. A method's name and a field's are no path, and the text of a literal no
//!   identifier, so neither names anything.
//! - The names that it defines, each the name of an item, wherever it stands: a function, a
//!   struct, an enum, a union, a trait, a type alias, a const, a static, a module, or a macro of
//!   `macro_rules!`.
//! - The names that it binds, each the identifier of a pattern, as in a `let`, a function's or a
//!   closure's parameter, or a match's arm.
//!
//! The check finds, in a file that the removal edits whose kept text parses:
//!
//! 1. A name that a `use` item of the file as it was imports and no `use` item of the kept text
//!    does, which the kept code names, and which the kept text neither defines nor binds, unless
//!    the kept text holds a glob, other than `use super::*` in a module whose body the file
//!    holds: the check reads no name that a glob imports. It is reported at the first kept line
//!    that names it.
//! 2. A name that a kept `use` item imports, where the item's visibility is inherited, with no
//!    `pub`, which the code of the file as it was names and the kept code doesn't. It is reported
//!    at the line of the `use` that holds the name. A trait that a `use` imports for its methods
//!    alone is named by neither, so it is never found.

use std::collections::{BTreeMap, HashSet};

use proc_macro2::{TokenStream, TokenTree};
use syn::{
    Ident, Item, ItemUse, Macro, MetaList, PatIdent, Path, UseTree, Visibility,
    visit::{self, Visit},
};

use crate::Finding;

/// The first names of a path that name no item of their own.
const KEYWORDS: [&str; 4] = ["self", "super", "crate", "Self"];

/// The standard library's derives, whose macros the prelude holds.
const DERIVES: [&str; 9] = [
    "Clone",
    "Copy",
    "Debug",
    "Default",
    "Eq",
    "Hash",
    "Ord",
    "PartialEq",
    "PartialOrd",
];

/// What the check reads of a file's Rust, as the module's documentation says.
#[derive(Default)]
pub(super) struct Uses {
    imports: Vec<Import>,
    /// Each name that the code names, with the first line that names it, counted from 1.
    named: BTreeMap<String, usize>,
    defined: HashSet<String>,
    bound: HashSet<String>,
    /// Whether a `use` item holds a glob, other than the one of `use super::*` in a module whose
    /// body the file holds.
    globbed: bool,
    /// How many modules whose body the file holds hold what is being read.
    depth: usize,
}

/// A name that a `use` item imports.
struct Import {
    name: String,
    /// The line of the `use` item that holds the name, counted from 1.
    line: usize,
    /// Whether the item's visibility is inherited, with no `pub`.
    inherited: bool,
}

impl Uses {
    /// What the check reads of `file`.
    pub(super) fn of(file: &syn::File) -> Self {
        let mut uses = Self::default();
        uses.visit_file(file);
        uses
    }

    fn name(&mut self, ident: &Ident) {
        let line = ident.span().start().line;
        self.named
            .entry(ident.to_string())
            .and_modify(|first| *first = (*first).min(line))
            .or_insert(line);
    }

    /// Reads the names that `tree` imports, where `before` is the name before it, if any.
    fn import(
        &mut self,
        tree: &UseTree,
        before: Option<&Ident>,
        inherited: bool,
        super_glob: bool,
    ) {
        let mut imports = |name: &Ident, at: &Ident| {
            self.imports.push(Import {
                name: name.to_string(),
                line: at.span().start().line,
                inherited,
            });
        };
        match tree {
            UseTree::Path(path) => {
                self.import(&path.tree, Some(&path.ident), inherited, super_glob);
            }
            UseTree::Name(name) if name.ident == "self" => {
                if let Some(before) = before {
                    imports(before, &name.ident);
                }
            }
            UseTree::Name(name) => imports(&name.ident, &name.ident),
            UseTree::Rename(rename) if rename.rename == "_" => {}
            UseTree::Rename(rename) => imports(&rename.rename, &rename.rename),
            UseTree::Glob(_) => self.globbed |= !super_glob,
            UseTree::Group(group) => {
                for tree in &group.items {
                    self.import(tree, before, inherited, super_glob);
                }
            }
        }
    }

    /// Reads the names among `tokens`, a macro's or an attribute's, through their groups, but
    /// `passed`, the names that name nothing there.
    fn tokens(&mut self, tokens: TokenStream, passed: &[&str]) {
        // Whether the token before is a `.`, and whether that `.` is the second of a `..`.
        let (mut dot, mut range) = (false, false);
        for token in tokens {
            let is_dot = matches!(&token, TokenTree::Punct(punct) if punct.as_char() == '.');
            match token {
                TokenTree::Ident(ident) if passed.iter().any(|name| ident == name) => {}
                TokenTree::Ident(ident) if !dot || range => self.name(&ident),
                TokenTree::Group(group) => self.tokens(group.stream(), passed),
                _ => {}
            }
            range = is_dot && dot && !range;
            dot = is_dot;
        }
    }
}

impl<'ast> Visit<'ast> for Uses {
    fn visit_item_use(&mut self, item: &'ast ItemUse) {
        let super_glob = self.depth > 0
            && matches!(
                &item.tree,
                UseTree::Path(path) if path.ident == "super" && matches!(*path.tree, UseTree::Glob(_))
            );
        let inherited = matches!(item.vis, Visibility::Inherited);
        // A `use` item names nothing, so nothing in it is visited.
        self.import(&item.tree, None, inherited, super_glob);
    }

    fn visit_item(&mut self, item: &'ast Item) {
        let defined = match item {
            Item::Fn(item) => Some(&item.sig.ident),
            Item::Struct(item) => Some(&item.ident),
            Item::Enum(item) => Some(&item.ident),
            Item::Union(item) => Some(&item.ident),
            Item::Trait(item) => Some(&item.ident),
            Item::Type(item) => Some(&item.ident),
            Item::Const(item) => Some(&item.ident),
            Item::Static(item) => Some(&item.ident),
            Item::Mod(item) => Some(&item.ident),
            Item::Macro(item) if item.mac.path.is_ident("macro_rules") => item.ident.as_ref(),
            _ => None,
        };
        if let Some(ident) = defined {
            self.defined.insert(ident.to_string());
        }
        let body = matches!(item, Item::Mod(module) if module.content.is_some());
        self.depth += usize::from(body);
        visit::visit_item(self, item);
        self.depth -= usize::from(body);
    }

    fn visit_path(&mut self, path: &'ast Path) {
        if path.leading_colon.is_none()
            && let Some(first) = path.segments.first()
            && !KEYWORDS.iter().any(|keyword| first.ident == keyword)
        {
            self.name(&first.ident);
        }
        visit::visit_path(self, path);
    }

    fn visit_macro(&mut self, mac: &'ast Macro) {
        visit::visit_macro(self, mac);
        self.tokens(mac.tokens.clone(), &[]);
    }

    fn visit_meta_list(&mut self, list: &'ast MetaList) {
        visit::visit_meta_list(self, list);
        let passed: &[&str] = if list.path.is_ident("derive") {
            &DERIVES
        } else {
            &[]
        };
        self.tokens(list.tokens.clone(), passed);
    }

    fn visit_pat_ident(&mut self, pat: &'ast PatIdent) {
        self.bound.insert(pat.ident.to_string());
        visit::visit_pat_ident(self, pat);
    }
}

/// The findings of the file at `path`, which a removal edits, as the module's documentation says:
/// `original` read of the file as it was, and `kept` of the text that the removal keeps of it,
/// whose line `n`, counted from 1, is `lines[n - 1]`, with its number in the file.
pub(super) fn lost(
    path: &str,
    original: &Uses,
    kept: &Uses,
    lines: &[(usize, &str)],
) -> Vec<Finding> {
    let at = |line: usize| {
        lines
            .get(line.saturating_sub(1))
            .map_or(1, |(number, _)| *number)
    };
    let imported: HashSet<&str> = kept
        .imports
        .iter()
        .map(|import| import.name.as_str())
        .collect();
    let mut found = Vec::new();
    if !kept.globbed {
        let gone: HashSet<&str> = original
            .imports
            .iter()
            .map(|import| import.name.as_str())
            .filter(|name| !imported.contains(name))
            .collect();
        for (name, line) in &kept.named {
            if gone.contains(name.as_str())
                && !kept.defined.contains(name)
                && !kept.bound.contains(name)
            {
                found.push(Finding::new(
                    path,
                    at(*line),
                    format!(
                        "`{name}` is named here, and each `use` that imports it goes with the \
                         removal (ARCH-07)"
                    ),
                ));
            }
        }
    }
    for import in kept.imports.iter().filter(|import| import.inherited) {
        if original.named.contains_key(&import.name) && !kept.named.contains_key(&import.name) {
            found.push(Finding::new(
                path,
                at(import.line),
                format!(
                    "this `use` imports `{}`, which only the lines that the removal deletes name \
                     (ARCH-07)",
                    import.name
                ),
            ));
        }
    }
    found.sort_by_key(|finding| finding.line);
    found
}

#[cfg(test)]
mod tests {
    use super::super::{LIST, check};
    use super::*;
    use crate::repo::Repo;

    /// What [`lost`] finds in `original` once a removal deletes its lines `removed`, counted from
    /// 1: each finding's line and message.
    fn found(original: &str, removed: &[usize]) -> Vec<(usize, String)> {
        let lines: Vec<(usize, &str)> = original
            .lines()
            .enumerate()
            .map(|(index, line)| (index + 1, line))
            .filter(|(number, _)| !removed.contains(number))
            .collect();
        let text: Vec<&str> = lines.iter().map(|(_, line)| *line).collect();
        let kept = Uses::of(&syn::parse_file(&text.join("\n")).unwrap());
        let original = Uses::of(&syn::parse_file(original).unwrap());
        lost("src/a.rs", &original, &kept, &lines)
            .into_iter()
            .map(|finding| (finding.line, finding.message))
            .collect()
    }

    fn gone(line: usize, name: &str) -> (usize, String) {
        (
            line,
            format!(
                "`{name}` is named here, and each `use` that imports it goes with the removal \
                 (ARCH-07)"
            ),
        )
    }

    fn idle(line: usize, name: &str) -> (usize, String) {
        (
            line,
            format!(
                "this `use` imports `{name}`, which only the lines that the removal deletes name \
                 (ARCH-07)"
            ),
        )
    }

    /// The names that the `use` items of `code` import, in order.
    fn imported(code: &str) -> Vec<String> {
        let uses = Uses::of(&syn::parse_file(code).unwrap());
        uses.imports.into_iter().map(|import| import.name).collect()
    }

    /// The names that `code` names, in order.
    fn named(code: &str) -> Vec<String> {
        let uses = Uses::of(&syn::parse_file(code).unwrap());
        uses.named.into_keys().collect()
    }

    #[test]
    fn arch_07_a_use_imports_each_name_of_its_tree_self_as_the_name_before_and_a_rename_as_its_new_name()
     {
        assert_eq!(
            imported(
                "use a::Gauge;\nuse b::{c::Dial, Knob as Lever, d::{self, Spring}};\n\
                 use e::Turn as _;\nuse f::*;\nfn g() { use h::Coil; }\nmod i { use j::Pin; }\n"
            ),
            ["Gauge", "Dial", "Lever", "d", "Spring", "Coil", "Pin"]
        );
    }

    #[test]
    fn arch_07_a_glob_counts_unless_it_is_super_s_in_a_module_whose_body_the_file_holds() {
        let globbed = |code: &str| Uses::of(&syn::parse_file(code).unwrap()).globbed;

        assert!(globbed("use a::*;\n"));
        assert!(globbed("fn b() { use c::Kind::*; }\n"));
        assert!(globbed("use super::*;\n"));
        assert!(globbed("mod d { use super::e::*; }\n"));
        assert!(!globbed("mod d { use super::*; }\n"));
        assert!(!globbed("mod d { mod e { use super::*; } }\n"));
    }

    #[test]
    fn arch_07_code_names_a_path_s_first_name_and_the_identifiers_of_macros_and_attributes() {
        assert_eq!(
            named(
                "#[derive(Dial)]\nstruct A { b: Gauge<Knob> }\n\
                 fn c() { d::e(); ::f::g(); self::h(); super::i(); crate::j(); Self::q(); x.method(); \
                 x.field; m!(k.l, 0..Lever, n::o, \"{Spring}\", (p)); }\n"
            ),
            [
                "Dial", "Gauge", "Knob", "Lever", "d", "derive", "k", "m", "n", "o", "p", "x"
            ]
        );
    }

    #[test]
    fn arch_07_items_at_any_depth_are_defined_and_the_identifiers_of_patterns_bound() {
        let uses = Uses::of(
            &syn::parse_file(
                "fn a(b: u8) { struct C; let d = |e| e; match b { f => {} } }\n\
                 mod g { enum H {} union I { j: u8 } trait K {} type L = u8; const M: u8 = 0; \
                 static N: u8 = 0; macro_rules! o { () => {} } mod p; }\n",
            )
            .unwrap(),
        );

        let mut defined: Vec<&str> = uses.defined.iter().map(String::as_str).collect();
        defined.sort_unstable();
        let mut bound: Vec<&str> = uses.bound.iter().map(String::as_str).collect();
        bound.sort_unstable();
        assert_eq!(
            defined,
            ["C", "H", "I", "K", "L", "M", "N", "a", "g", "o", "p"]
        );
        assert_eq!(bound, ["b", "d", "e", "f"]);
    }

    #[test]
    fn arch_07_a_name_that_kept_code_names_whose_only_use_goes_is_found_at_its_first_kept_line() {
        // In code, in a macro and in an attribute; through `self` and a rename; in a nested
        // module and in a function's body; and with the glob of the tests' module.
        let code = "use a::Gauge;\nuse b::dials::{self};\nuse c::Knob as Lever;\n\
                    fn d() -> Gauge { use e::Spring; m!(Spring) }\nmod f { use g::Pin; }\n\
                    fn h() { dials::turn(); Lever::pull(); }\nmod i { fn j() -> Pin {} }\n\
                    #[cfg(test)]\nmod tests { use super::*; #[derive(Gauge)] struct K; }\n";

        assert_eq!(
            found(code, &[1, 2, 3, 5]),
            [
                gone(4, "Gauge"),
                gone(6, "Lever"),
                gone(6, "dials"),
                gone(7, "Pin"),
            ]
        );
        let spring = "fn a() { use b::Spring; }\nfn c() { m!(Spring); }\n";
        assert_eq!(found(spring, &[1]), [gone(2, "Spring")]);
    }

    #[test]
    fn arch_07_a_name_whose_only_use_goes_is_not_found_where_the_kept_text_binds_defines_or_imports_it()
     {
        // Bound, defined, imported again, named after a `.` or in a literal's text alone, named
        // only as what an import renamed or a path that starts with `::`, and the `_` of a
        // rename, which `_` among a macro's tokens doesn't name.
        let code = "use a::gauge;\nuse b::Dial;\nuse c::Knob;\nuse d::turn;\nuse e::Lever;\n\
                    use f::Spring as Coil;\nuse g::pin;\nuse h::Latch as _;\n\
                    fn i(gauge: u8) -> u8 { gauge }\nmod j { struct Dial; fn q() -> Dial {} }\n\
                    mod tests { use k::Knob; fn l() -> Knob {} }\n\
                    fn m() { x.turn(); n!(x.Lever, \"{Lever}\"); Spring::new(); ::pin::o(); n!(_); }\n";

        assert_eq!(found(code, &[1, 2, 3, 4, 5, 6, 7, 8]), []);
    }

    #[test]
    fn arch_07_a_glob_other_than_super_s_in_a_module_keeps_a_name_whose_only_use_goes_from_being_found()
     {
        let code = "use a::Gauge;\nuse b::*;\nfn c() -> Gauge {}\n";

        assert_eq!(found(code, &[1]), []);
        assert_eq!(found(code, &[1, 2]), [gone(3, "Gauge")]);
    }

    #[test]
    fn arch_07_a_kept_use_whose_name_only_deleted_lines_named_is_found_at_the_line_of_the_name() {
        // In a group of lines, through `self` and a rename, and named only after a `.` in what
        // the removal keeps.
        let code = "use a::{\n    Gauge,\n    Dial,\n};\nuse b::knobs::{self};\nuse c::Spring as Coil;\n\
                    fn d() -> Gauge {}\nfn e() -> Dial {}\nfn f() { knobs::turn(); Coil::new(); }\n\
                    fn g() { x.Dial; m!(x.Coil) }\n";

        assert_eq!(
            found(code, &[7, 9]),
            [idle(2, "Gauge"), idle(5, "knobs"), idle(6, "Coil")]
        );
    }

    #[test]
    fn arch_07_a_kept_use_is_not_found_while_kept_code_names_its_name_or_where_it_is_pub_or_a_trait()
     {
        // Named in a kept macro, attribute or path; a visibility other than the inherited one;
        // a trait whose methods alone the code calls, which no code names; and a rename to `_`,
        // which imports no name, though `_` among a macro's tokens is one.
        let code = "use a::Gauge;\nuse b::Dial;\nuse c::Knob;\npub use d::Lever;\n\
                    pub(crate) use e::Spring;\nuse f::Turn;\nuse g::Latch as _;\n\
                    fn h() -> (Gauge, Dial, Knob, Lever, Spring) { x.turn(); m!(_) }\n\
                    fn i() { m!(Gauge); y.turn() }\n#[derive(Dial)]\nstruct J(Knob);\n";

        assert_eq!(found(code, &[8]), []);
    }

    /// A marker of `part` in Rust, which the source of these tests builds rather than holds, so
    /// `just remove` never reads one here.
    fn mark(part: &str) -> String {
        format!("// {} {part}", "part:")
    }

    fn end(part: &str) -> String {
        format!("// end {} {part}", "part:")
    }

    /// The list of two parts, `gadgets` and `widgets`, each of which owns a file, and of the run
    /// `bare`, which removes both, with the parts' markers, as `parts.yaml` carries them.
    fn two_parts() -> String {
        let hash = |marker: String| format!("#{}", &marker[2..]);
        let part = |name: &str, word: &str| {
            format!(
                "  {}\n  - name: {name}\n    paths:\n      - src/{name}.rs\n    words:\n      - \
                 {word}\n  {}\n",
                hash(mark(name)),
                hash(end(name))
            )
        };
        format!(
            "parts:\n{}{}runs:\n  - name: bare\n    parts:\n      - gadgets {}\n      - widgets \
             {}\n",
            part("gadgets", "gadget"),
            part("widgets", "widget"),
            hash(mark("gadgets")),
            hash(mark("widgets")),
        )
    }

    /// A file whose import of `Dial` stands in `gadgets`' block, which only `widgets`' line
    /// names, and whose core's import of `Gauge` only `gadgets`' line names.
    fn dial_in_one_part_named_in_another() -> String {
        [
            mark("gadgets"),
            "use crate::dials::Dial;".to_owned(),
            end("gadgets"),
            "use crate::gauges::Gauge;".to_owned(),
            "pub fn core() {}".to_owned(),
            mark("widgets"),
            "pub fn widget(dial: Dial) -> Dial {".to_owned(),
            "    dial".to_owned(),
            "}".to_owned(),
            end("widgets"),
            mark("gadgets"),
            "pub fn gadget(gauge: Gauge) -> Gauge {".to_owned(),
            "    gauge".to_owned(),
            "}".to_owned(),
            end("gadgets"),
            String::new(),
        ]
        .join("\n")
    }

    #[test]
    fn arch_07_an_import_in_one_part_s_block_that_another_part_s_line_names_fails_each_removal_alone()
     {
        // Removing `gadgets` alone leaves `widgets`' line without the import of `Dial`, and
        // `widgets` alone leaves that import unused, as every removal of `gadgets` leaves the
        // core's import of `Gauge`.
        let list = two_parts();
        let code = dial_in_one_part_named_in_another();
        let repo = Repo::of(&[
            (LIST, &list),
            ("src/a.rs", &code),
            ("src/gadgets.rs", ""),
            ("src/widgets.rs", ""),
        ]);

        let outcome = check(&repo);

        let found: Vec<(&str, usize, &str)> = outcome
            .findings
            .iter()
            .map(|finding| {
                (
                    finding.path.as_str(),
                    finding.line,
                    finding.message.as_str(),
                )
            })
            .collect();
        assert_eq!(
            found,
            [
                (
                    "src/a.rs",
                    4,
                    "removing gadgets, widgets: this `use` imports `Gauge`, which only the lines \
                     that the removal deletes name (ARCH-07)"
                ),
                (
                    "src/a.rs",
                    4,
                    "removing gadgets: this `use` imports `Gauge`, which only the lines that the \
                     removal deletes name (ARCH-07)"
                ),
                (
                    "src/a.rs",
                    7,
                    "removing gadgets: `Dial` is named here, and each `use` that imports it goes \
                     with the removal (ARCH-07)"
                ),
                (
                    "src/a.rs",
                    2,
                    "removing widgets: this `use` imports `Dial`, which only the lines that the \
                     removal deletes name (ARCH-07)"
                ),
            ]
        );
        assert_eq!(
            outcome.report,
            [
                "bare: removing gadgets, widgets deletes 2 files and edits 2",
                "gadgets alone: removing gadgets deletes 1 files and edits 2",
                "widgets alone: removing widgets deletes 1 files and edits 2",
            ]
        );
    }

    #[test]
    fn arch_07_a_standard_derive_names_no_trait_that_a_use_imports() {
        let code = "use std::fmt::Debug;\nuse serde::Serialize;\n\
                    #[derive(Debug, Serialize)]\nstruct Gauge;\n\
                    fn shown(dial: impl Debug) {}\n";

        // The derive's `Debug` is the prelude's macro, so only the bound names the trait.
        assert_eq!(found(code, &[5]), [idle(1, "Debug")]);
        assert_eq!(found(code, &[1, 5]), []);
        // Another derive is the name that a `use` imports.
        assert_eq!(found(code, &[2]), [gone(3, "Serialize")]);
    }
}
