//! `check-deps`: the feature modules depend on each other as architecture.md §5 allows.
//! - A module's `mod.rs` declares its submodules private, and no code outside a module names
//!   one of its submodules, so others use only what `mod.rs` re-exports (ARCH-04).
//! - A module names another only along an edge of the graph in architecture.md §5, which has no
//!   cycle (ARCH-05).
//! - No public signature of a module, and no field its public types expose, names another
//!   module's error type: the names that module's `mod.rs` re-exports from its `error.rs`
//!   (ERR-06).
//!
//! It reads each file of `app`'s library as a token stream, with the paths its `use`
//! declarations import (`rust`), so it finds a module's name inside macros and attributes too.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    Finding, Outcome,
    repo::Repo,
    rust::{self, Named, Source},
};

/// The document that holds the graph.
const ARCHITECTURE: &str = "docs/architecture.md";

/// The heading of the section whose Mermaid block is the graph.
const GRAPH_HEADING: &str = "### Dependencies";

/// A feature module, as its files show it.
#[derive(Default)]
struct Module {
    /// Its submodules, which its `mod.rs` declares.
    submodules: BTreeSet<String>,
    /// The names its `mod.rs` re-exports from its `error.rs`.
    errors: BTreeSet<String>,
    /// Every name its `mod.rs` imports, which includes what it re-exports.
    exports: BTreeSet<String>,
}

/// Checks `app`'s library against the graph.
pub fn check(repo: &Repo) -> Outcome {
    let mut findings = Vec::new();
    let graph = match graph(repo) {
        Ok(graph) => graph,
        Err(finding) => {
            return Outcome {
                report: Vec::new(),
                findings: vec![finding],
            };
        }
    };
    findings.extend(cycles(&graph));
    let mut sources = Vec::new();
    for file in repo.files() {
        if !file.path.starts_with(rust::APP)
            || !file.path.ends_with(".rs")
            || file.path == "crates/app/src/main.rs"
        {
            continue;
        }
        match Source::read(file) {
            Ok(source) => sources.push(source),
            Err(finding) => findings.push(finding),
        }
    }
    let modules = modules(&sources, &mut findings);
    for source in &sources {
        let own = rust::feature_module(source.path).map(|(module, _)| module);
        findings.extend(references(source, own, &modules, &graph.edges));
        if let Some(own) = own {
            findings.extend(foreign_errors(source, own, &modules));
        }
    }
    let mut seen = BTreeSet::new();
    findings.retain(|f| seen.insert((f.path.clone(), f.line, f.message.clone())));
    Outcome {
        report: Vec::new(),
        findings,
    }
}

/// The allowed edges, from the Mermaid block of architecture.md §5, and the line of the block.
struct Graph {
    edges: BTreeSet<(String, String)>,
    line: usize,
}

/// Reads the graph: each line `a --> b & c` of the block allows `a` to use `b` and `c`.
fn graph(repo: &Repo) -> Result<Graph, Finding> {
    let missing = |message: &str| Finding::new(ARCHITECTURE, 1, message.to_owned());
    let text = repo
        .file(ARCHITECTURE)
        .map(|file| file.text.as_str())
        .ok_or_else(|| {
            missing("the architecture document, which holds the graph, is missing (ARCH-05)")
        })?;
    let lines: Vec<&str> = text.lines().collect();
    let heading = lines
        .iter()
        .position(|line| line.trim() == GRAPH_HEADING)
        .ok_or_else(|| {
            missing("no section `Dependencies` holds the graph of the modules (ARCH-05)")
        })?;
    let start = lines[heading..]
        .iter()
        .position(|line| line.trim() == "```mermaid")
        .map(|at| heading + at)
        .ok_or_else(|| missing("the section `Dependencies` has no Mermaid block (ARCH-05)"))?;
    let mut edges = BTreeSet::new();
    for line in lines[start + 1..]
        .iter()
        .take_while(|line| line.trim() != "```")
    {
        let Some((from, to)) = line.split_once("-->") else {
            continue;
        };
        for from in from.split('&').map(str::trim) {
            for to in to.split('&').map(str::trim) {
                edges.insert((from.to_owned(), to.to_owned()));
            }
        }
    }
    Ok(Graph {
        edges,
        line: start + 1,
    })
}

/// ARCH-05: a cycle in the graph, if there is one.
fn cycles(graph: &Graph) -> Vec<Finding> {
    let mut next: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (from, to) in &graph.edges {
        next.entry(from).or_default().push(to);
    }
    for start in next.keys() {
        let mut stack = vec![(*start, vec![*start])];
        while let Some((at, path)) = stack.pop() {
            for to in next.get(at).into_iter().flatten() {
                if to == start {
                    return vec![Finding::new(
                        ARCHITECTURE,
                        graph.line,
                        format!(
                            "the graph of the modules has a cycle: {} -> {start} (ARCH-05)",
                            path.join(" -> ")
                        ),
                    )];
                }
                if !path.contains(to) {
                    let mut longer = path.clone();
                    longer.push(to);
                    stack.push((to, longer));
                }
            }
        }
    }
    Vec::new()
}

/// Each feature module, from its `mod.rs`. ARCH-04: a submodule it declares is private.
fn modules(sources: &[Source], findings: &mut Vec<Finding>) -> BTreeMap<String, Module> {
    let mut modules: BTreeMap<String, Module> = BTreeMap::new();
    for source in sources {
        let Some((name, parts)) = rust::feature_module(source.path) else {
            continue;
        };
        let module = modules.entry(name.to_owned()).or_default();
        if !parts.is_empty() {
            continue;
        }
        for (submodule, public, line) in rust::declared(&source.tokens) {
            if public {
                findings.push(Finding::new(
                    source.path,
                    line,
                    format!("`{submodule}` is public: a module's submodules are private, and others use what its `mod.rs` re-exports (ARCH-04)"),
                ));
            }
            module.submodules.insert(submodule);
        }
        let error = ["crate", "modules", name, "error"];
        for (alias, path) in &source.imports {
            module.exports.insert(alias.clone());
            let from_error =
                path.len() > error.len() && path.iter().zip(error).all(|(a, b)| a == b);
            if from_error && alias != "*" {
                module.errors.insert(alias.clone());
            }
        }
    }
    modules
}

/// ARCH-04 and ARCH-05: the other modules a file names, and how.
fn references(
    source: &Source,
    own: Option<&str>,
    modules: &BTreeMap<String, Module>,
    edges: &BTreeSet<(String, String)>,
) -> Vec<Finding> {
    let mut findings = Vec::new();
    for Named { segments, line } in source.named() {
        let [first, second, other, rest @ ..] = segments.as_slice() else {
            continue;
        };
        if first != "crate" || second != "modules" || Some(other.as_str()) == own {
            continue;
        }
        let Some(module) = modules.get(other) else {
            continue;
        };
        // A path that ends at a submodule's name may name an item that `mod.rs` re-exports under
        // the same name, such as the function `routes` of the submodule `routes`.
        let reaches_in = |name: &&String| {
            module.submodules.contains(*name) && (rest.len() > 1 || !module.exports.contains(*name))
        };
        if let Some(submodule) = rest.first().filter(reaches_in) {
            findings.push(Finding::new(
                source.path,
                line,
                format!("`{}` reaches into `{other}::{submodule}`: other code uses only what `{other}`'s `mod.rs` re-exports (ARCH-04)", segments.join("::")),
            ));
        }
        if let Some(own) = own
            && !edges.contains(&(own.to_owned(), other.clone()))
        {
            findings.push(Finding::new(
                source.path,
                line,
                format!("`{own}` uses `{other}`, which the graph in architecture.md §5 doesn't allow (ARCH-05)"),
            ));
        }
    }
    findings
}

/// ERR-06: another module's error type in a public signature or a public field.
fn foreign_errors(source: &Source, own: &str, modules: &BTreeMap<String, Module>) -> Vec<Finding> {
    let mut exposed: Vec<(usize, Vec<Named>)> = rust::signatures(&source.tokens)
        .into_iter()
        .filter(|signature| signature.public)
        .map(|signature| (signature.line, rust::names(&signature.tokens)))
        .collect();
    exposed.extend(
        rust::public_fields(&source.tokens)
            .into_iter()
            .map(|(line, tokens)| (line, rust::names(&tokens))),
    );
    let mut findings = Vec::new();
    for (line, names) in exposed {
        for Named { segments, .. } in source.resolved(names) {
            let [first, second, other, .., last] = segments.as_slice() else {
                continue;
            };
            let foreign = first == "crate"
                && second == "modules"
                && other != own
                && modules
                    .get(other)
                    .is_some_and(|module| module.errors.contains(last));
            if foreign {
                findings.push(Finding::new(
                    source.path,
                    line,
                    format!("a public signature of `{own}` names `{other}`'s error `{last}`: convert it into `{own}`'s own (ERR-06)"),
                ));
            }
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    const GRAPH: &str = "## 5. Modules\n\n### Dependencies\n\n```mermaid\nflowchart TD\n  auth --> accounts & audit\n  accounts --> audit\n```\n";

    const ACCOUNTS: &str = "mod error; mod repo; mod service; pub(crate) use error::AccountsError; pub(crate) use service::create;";

    fn findings(files: &[(&str, &str)]) -> Vec<String> {
        let mut all = vec![
            (ARCHITECTURE, GRAPH),
            ("crates/app/src/modules/accounts/mod.rs", ACCOUNTS),
        ];
        all.extend_from_slice(files);
        check(&Repo::of(&all))
            .findings
            .into_iter()
            .map(|finding| finding.message)
            .collect()
    }

    #[test]
    fn arch_05_modules_that_follow_the_graph_pass() {
        let auth = r#"
            use crate::modules::{accounts::{self, AccountsError}, audit};
            impl From<AccountsError> for AuthError { fn from(error: AccountsError) -> Self { todo() } }
            #[utoipa::path(post, path = "/v1/users", responses((status = 201, body = crate::modules::accounts::Profile)))]
            pub(crate) async fn register() -> Result<(), AuthError> { accounts::create().await }
        "#;
        assert_eq!(
            findings(&[("crates/app/src/modules/auth/error.rs", auth)]),
            Vec::<String>::new()
        );
    }

    #[test]
    fn arch_05_a_module_uses_another_only_along_an_edge() {
        let audit = "fn f() { crate::modules::accounts::create(); super::super::auth::x(); }";
        assert_eq!(
            findings(&[("crates/app/src/modules/audit/service.rs", audit)]),
            [
                "`audit` uses `accounts`, which the graph in architecture.md §5 doesn't allow (ARCH-05)"
            ],
            "auth, which has no mod.rs here, isn't a module of the code"
        );
    }

    #[test]
    fn arch_05_the_graph_has_no_cycle() {
        let cyclic = GRAPH.replace("accounts --> audit", "accounts --> audit\n  audit --> auth");
        let repo = Repo::of(&[(ARCHITECTURE, cyclic.as_str())]);
        let messages: Vec<String> = check(&repo)
            .findings
            .into_iter()
            .map(|f| f.message)
            .collect();
        assert_eq!(messages.len(), 1, "{messages:#?}");
        assert!(messages[0].starts_with("the graph of the modules has a cycle: "));
        assert!(messages[0].ends_with("(ARCH-05)"));
    }

    #[test]
    fn arch_04_a_module_s_submodules_are_private_and_others_use_its_re_exports() {
        let leaky = "pub(crate) mod repo; mod routes; mod service; pub(crate) use service::create; pub(crate) use routes::routes;";
        let repo = Repo::of(&[
            (ARCHITECTURE, GRAPH),
            ("crates/app/src/modules/accounts/mod.rs", leaky),
            (
                "crates/app/src/composition.rs",
                "use crate::modules::accounts::repo::insert; fn f() { crate::modules::accounts::create(); crate::modules::accounts::routes(); }",
            ),
        ]);
        let messages: Vec<String> = check(&repo)
            .findings
            .into_iter()
            .map(|f| f.message)
            .collect();
        assert_eq!(
            messages,
            [
                "`repo` is public: a module's submodules are private, and others use what its `mod.rs` re-exports (ARCH-04)",
                "`crate::modules::accounts::repo::insert` reaches into `accounts::repo`: other code uses only what `accounts`'s `mod.rs` re-exports (ARCH-04)",
            ]
        );
    }

    #[test]
    fn err_06_no_public_signature_names_another_module_s_error() {
        let service = r#"
            use crate::modules::accounts::{self, AccountsError};
            pub(crate) async fn register() -> Result<(), AccountsError> {}
            pub(crate) fn other() -> accounts::AccountsError {}
            fn private() -> Result<(), AccountsError> {}
        "#;
        let error = "pub(crate) enum AuthError { Accounts(#[from] crate::modules::accounts::AccountsError) }";
        let message = "a public signature of `auth` names `accounts`'s error `AccountsError`: convert it into `auth`'s own (ERR-06)";
        assert_eq!(
            findings(&[
                ("crates/app/src/modules/auth/service.rs", service),
                ("crates/app/src/modules/auth/error.rs", error),
            ]),
            [message, message, message]
        );
    }
}
