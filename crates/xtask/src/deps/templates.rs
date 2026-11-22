//! ARCH-03 and ARCH-04 for the templates of `just new-module` (`new_module`), which only the copy
//! of `check-new-module` builds: each path `crate::modules::<m>::<name>` of a template that the
//! task writes into `app`'s library, for a module `<m>` other than the template's own, names what
//! `<m>`'s `mod.rs` re-exports: a name that a `pub use` of it imports, any name after a glob, or
//! a name that it declares `pub`.
//!
//! It reads each such template as the file that the task writes it to, for the template's own
//! module, `sample_items`, with the paths its `use` declarations import (`rust`), so a path through
//! one of them, such as `log::record` after `audit::{self as log}`, is read in full; a template
//! that isn't Rust's tokens is a finding. It reads the names that the template the task inserts
//! into `access`'s `mod.rs` re-exports as `access`'s. The templates hold the template's names,
//! which the task replaces with the module's alike where a template's path names one and where
//! the inserted re-export does, so a path through one, such as `access::SampleItemsRead`, is read
//! as the templates write it.

use std::collections::{BTreeMap, BTreeSet};

use proc_macro2::{TokenStream, TokenTree};

use super::Module;
use crate::{
    Finding, new_module,
    repo::{File, Repo},
    rust::{self, Named, Source},
};

/// Each path into another module that a Rust template of `files/` names, which that module's
/// `mod.rs`, with the re-export the task inserts into it, doesn't re-export.
pub(super) fn imports(repo: &Repo, modules: &BTreeMap<String, Module>) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut public: BTreeMap<&str, BTreeSet<String>> = modules
        .iter()
        .map(|(name, module)| (name.as_str(), module.public.clone()))
        .collect();
    if let Some((path, text, root)) = new_module::inserted_exports(repo)
        && let Some((module, _)) = rust::feature_module(root)
    {
        match text.parse::<TokenStream>() {
            Ok(tokens) => {
                let tokens: Vec<TokenTree> = tokens.into_iter().collect();
                public
                    .entry(module)
                    .or_default()
                    .extend(rust::reexported(&tokens));
            }
            Err(error) => findings.push(Finding::new(
                path,
                error.span().start().line,
                error.to_string(),
            )),
        }
    }
    for file in repo.files() {
        let Some(written) = new_module::written(&file.path)
            .filter(|path| path.starts_with(rust::APP) && path.ends_with(".rs"))
        else {
            continue;
        };
        let template = File {
            path: written.to_owned(),
            text: file.text.clone(),
        };
        let source = match Source::read(&template) {
            Ok(source) => source,
            Err(finding) => {
                findings.push(Finding::new(&file.path, finding.line, finding.message));
                continue;
            }
        };
        let own = rust::feature_module(written).map(|(module, _)| module);
        for Named { segments, line } in source.named() {
            let [first, second, other, name, ..] = segments.as_slice() else {
                continue;
            };
            if first != "crate" || second != "modules" || Some(other.as_str()) == own {
                continue;
            }
            let exported = public
                .get(other.as_str())
                .is_some_and(|names| names.contains(name) || names.contains("*"));
            if !exported {
                findings.push(Finding::new(
                    &file.path,
                    line,
                    format!("`{}` names an item of `{other}` that its `mod.rs` doesn't re-export, `{name}`, so the module that `just new-module` writes from this template wouldn't build (ARCH-03, ARCH-04)", segments.join("::")),
                ));
            }
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deps::check;

    const TEMPLATES: &str = "crates/xtask/templates/new-module";

    /// The findings of ARCH-03 of a check whose templates of `files/` are `templates`, each by
    /// its path under `files/crates/app/src/modules/sample_items/`, and whose re-export inserted
    /// into `access` is `inserted`.
    fn refused(templates: &[(&str, &str)], inserted: &str) -> Vec<String> {
        let graph = "### Dependencies\n\n```mermaid\nflowchart TD\n  sample_items --> access & audit\n```\n";
        let mut files: Vec<(String, String)> = vec![
            ("docs/architecture.md".to_owned(), graph.to_owned()),
            (
                "crates/app/src/modules/access/mod.rs".to_owned(),
                "mod error; mod permissions; mod service;\n\
                 use service::Private;\n\
                 pub(crate) use error::AccessError;\n\
                 pub(crate) use service::{self as checks, held};\n\
                 pub(crate) fn sole_workspaces() {}"
                    .to_owned(),
            ),
            (
                "crates/app/src/modules/audit/mod.rs".to_owned(),
                "mod model; mod service; pub(crate) use model::*;".to_owned(),
            ),
            (
                format!("{TEMPLATES}/insert/access-exports.rs.tmpl"),
                inserted.to_owned(),
            ),
        ];
        for (path, text) in templates {
            let path = format!("{TEMPLATES}/files/crates/app/src/modules/sample_items/{path}");
            files.push((path, (*text).to_owned()));
        }
        let files: Vec<(&str, &str)> = files
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        let mut found: Vec<Finding> = check(&Repo::of(&files))
            .findings
            .into_iter()
            .filter(|finding| finding.message.contains("(ARCH-03, ARCH-04)"))
            .collect();
        found.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
        found
            .iter()
            .map(|finding| {
                let path = finding.path.rsplit('/').next().unwrap_or_default();
                let named = finding.message.split('`').nth(1).unwrap_or_default();
                format!("{path}:{}: {named}", finding.line)
            })
            .collect()
    }

    #[test]
    fn arch_03_a_template_names_only_what_another_module_s_mod_rs_re_exports() {
        let service = "use super::model::SampleItem;
use crate::modules::{access::{self, AccessError, Missing, SampleItemsRead}, audit::{self as log, Entry}};
use crate::modules::access::service::held as reached;
use crate::modules::access::Private as Renamed;
fn f() {
    access::sole_workspaces();
    log::record();
    crate::modules::access::removed(SampleItemsRead);
    crate::modules::sample_items::model::own();
    crate::modules::orders::Order::new();
    let x = \"crate::modules::access::InAString\";
    access::checks::held();
}";
        let events = "use crate::modules::access::{SampleItemsRead, SampleItemsWrite};";
        let inserted = "\npub(crate) use permissions::{SampleItemsCreate, SampleItemsRead};\n";
        assert_eq!(
            refused(
                &[("service.rs.tmpl", service), ("events.rs.tmpl", events)],
                inserted
            ),
            [
                "events.rs.tmpl:1: crate::modules::access::SampleItemsWrite",
                "service.rs.tmpl:2: crate::modules::access::Missing",
                "service.rs.tmpl:3: crate::modules::access::service::held",
                "service.rs.tmpl:4: crate::modules::access::Private",
                "service.rs.tmpl:8: crate::modules::access::removed",
                "service.rs.tmpl:10: crate::modules::orders::Order::new",
            ],
            "a glob re-exports `audit`'s every name; a template's own module and a string are no path into another"
        );
    }

    #[test]
    fn arch_03_a_template_that_isn_t_rust_is_refused() {
        let found = check(&Repo::of(&[
            (
                "docs/architecture.md",
                "### Dependencies\n\n```mermaid\n```\n",
            ),
            (
                "crates/xtask/templates/new-module/files/crates/app/src/modules/sample_items/repo.rs.tmpl",
                "fn f() { (",
            ),
            (
                "crates/xtask/templates/new-module/files/docs/spec/sample_items.md.tmpl",
                "# Sample items (",
            ),
        ]));
        let paths: Vec<&str> = found.findings.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "crates/xtask/templates/new-module/files/crates/app/src/modules/sample_items/repo.rs.tmpl"
            ]
        );
    }
}
