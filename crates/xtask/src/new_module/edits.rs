//! Where a new module registers itself in the files that every module shares: each insertion
//! finds its place by what the file holds there, such as the `const MATRIX` of the permission
//! matrix, so it needs no marker of its own. When a file doesn't hold its place any more, as a
//! fork may have rewritten it, `edits` gives what to add to each such file instead, and the task
//! writes nothing.

use super::names::Names;

/// A file that a new module changes, with the text it holds after the change.
pub struct Edit {
    /// The file's path.
    pub path: &'static str,
    /// What it holds after the change.
    pub text: String,
}

/// The text of each insertion that isn't one line, from the templates' `insert/` directory,
/// with the module's names in place of the template's.
pub struct Inserts {
    /// The permission types (`access/permissions.rs`).
    pub permissions: String,
    /// The permission matrix's rows (`access/permissions.rs`).
    pub matrix: String,
    /// The re-export of the permission types (`access/mod.rs`).
    pub access_exports: String,
    /// TEST-08's fixtures of the module's routes (`tests/integration/access.rs`).
    pub test_08_fixtures: String,
    /// The module's row of what each module owns (architecture.md §5).
    pub architecture_owns: String,
    /// The module's edges in the graph of dependencies (architecture.md §5).
    pub architecture_graph: String,
    /// The module's row of the spec index.
    pub spec_index: String,
    /// The permissions' rows of the access spec's matrix.
    pub access_matrix: String,
}

/// Every edit that registers the module `names`, given each shared file's text by `read`, or,
/// for each file that doesn't hold its insertion's place, what to add there by hand.
pub fn edits(
    names: &Names,
    inserts: &Inserts,
    read: impl Fn(&str) -> Option<String>,
) -> Result<Vec<Edit>, Vec<String>> {
    let mut plan = Plan {
        read: &read,
        edits: Vec::new(),
        missing: Vec::new(),
    };
    code(&mut plan, &names.module, inserts);
    documents(&mut plan, inserts);
    if plan.missing.is_empty() {
        Ok(plan.edits)
    } else {
        Err(plan.missing)
    }
}

/// The edits found so far, and what to add by hand where a file doesn't hold its place.
struct Plan<'a> {
    /// Each shared file's text.
    read: &'a dyn Fn(&str) -> Option<String>,
    /// The edits found.
    edits: Vec<Edit>,
    /// What to add by hand, with the file to add it to.
    missing: Vec<String>,
}

impl Plan<'_> {
    /// The file at `path` changed by `edit`, or `what` to add to it by hand when `edit` finds no
    /// place in it.
    fn place(&mut self, path: &'static str, what: &str, edit: &dyn Fn(&str) -> Option<String>) {
        match (self.read)(path).as_deref().and_then(edit) {
            Some(text) => self.edits.push(Edit { path, text }),
            None => self.missing.push(format!("{path}: add {what}")),
        }
    }
}

/// The code's edits: the module's declaration, its composition, its permissions, and its test
/// module and TEST-08's fixtures.
fn code(plan: &mut Plan<'_>, module: &str, inserts: &Inserts) {
    let declaration = format!("pub(crate) mod {module};");
    plan.place(
        "crates/app/src/modules/mod.rs",
        &format!("`{declaration}` among the modules"),
        &|text| {
            sorted(text, module, std::slice::from_ref(&declaration), |line| {
                entry(line, "pub(crate) mod ", ";")
            })
        },
    );
    plan.place(
        "crates/app/src/composition/mod.rs",
        &format!(
            "`{module},` among the imports of `crate::modules`, `.merge({module}::routes())` \
             among the routes that `routes` mounts, `{module}::routes().openapi(service),` among the \
             documents of `openapi`, and `Arc::new({module}::Contract),` among the contracts of \
             `contracts`, before the contract of `accounts`"
        ),
        &|text| composition(text, module),
    );
    plan.place(
        "crates/app/src/modules/access/permissions.rs",
        "the permission types before the matrix, and their rows in it",
        &|text| permissions(text, &inserts.permissions, &inserts.matrix),
    );
    plan.place(
        "crates/app/src/modules/access/mod.rs",
        "the re-export of the permission types",
        &|text| {
            after_statement(
                text,
                "pub(crate) use permissions::",
                &inserts.access_exports,
            )
        },
    );
    let test_lines = ["#[cfg(test)]".to_owned(), format!("mod {module};")];
    plan.place(
        "crates/app/tests/integration/main.rs",
        &format!("`#[cfg(test)] mod {module};` among the test modules"),
        &|text| sorted(text, module, &test_lines, |line| entry(line, "mod ", ";")),
    );
    plan.place(
        "crates/app/tests/integration/support/mod.rs",
        &format!("`pub mod {module};` among the test support's modules"),
        &|text| {
            sorted(text, module, &[format!("pub mod {module};")], |line| {
                entry(line, "pub mod ", ";")
            })
        },
    );
    plan.place(
        "crates/app/tests/integration/access.rs",
        "the fixtures of the module's routes at the end of TEST-08's `FIXTURES`",
        &|text| test_08(text, &inserts.test_08_fixtures),
    );
}

/// The documents' edits: architecture.md's table and graph, the spec index, and the access spec's
/// matrix.
fn documents(plan: &mut Plan<'_>, inserts: &Inserts) {
    plan.place(
        "docs/architecture.md",
        "the module's row of what each module owns, and its edges in the graph of dependencies (§5)",
        &|text| architecture(text, &inserts.architecture_owns, &inserts.architecture_graph),
    );
    plan.place(
        "docs/spec/README.md",
        "the module's row of the index of specs",
        &|text| after_table(text, "## Specs", &inserts.spec_index),
    );
    plan.place(
        "docs/spec/access.md",
        "the permissions' rows of the matrix under \"Roles and permissions\"",
        &|text| after_table(text, "### Roles and permissions", &inserts.access_matrix),
    );
}

/// The name that `line` declares between `start` and `end`, such as `access` in
/// `pub(crate) mod access;`.
fn entry(line: &str, start: &str, end: &str) -> Option<String> {
    line.strip_prefix(start)
        .and_then(|rest| rest.strip_suffix(end))
        .filter(|name| {
            name.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        })
        .map(str::to_owned)
}

/// Whether `line` is a marker that opens a part's block (ARCH-07), or an attribute, which the
/// declaration below it carries.
fn carried(line: &str) -> bool {
    let line = line.trim_start();
    line.starts_with("#[") || line.starts_with("// part: ")
}

/// `text` with `lines` among the declarations that `declared` reads a name from, in the order of
/// their names, before the first whose name comes after `name`, and above what that one carries,
/// or after the last, and below the end of its part's block. `None` when nothing is declared.
fn sorted(
    text: &str,
    name: &str,
    lines: &[String],
    declared: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let mut all: Vec<&str> = text.lines().collect();
    let entries: Vec<(usize, String)> = all
        .iter()
        .enumerate()
        .filter_map(|(index, line)| declared(line).map(|found| (index, found)))
        .collect();
    let (last, _) = entries.last()?;
    let at = match entries.iter().find(|(_, found)| found.as_str() > name) {
        Some((index, _)) => {
            let mut at = *index;
            while at > 0 && carried(all[at - 1]) {
                at -= 1;
            }
            at
        }
        None => {
            let mut at = last + 1;
            while at < all.len() && all[at].trim_start().starts_with("// end part: ") {
                at += 1;
            }
            at
        }
    };
    let inserted: Vec<&str> = lines.iter().map(String::as_str).collect();
    all.splice(at..at, inserted);
    Some(joined(&all, text))
}

/// `text` from `lines`, with the final line feed `text` had.
fn joined(lines: &[&str], text: &str) -> String {
    let mut joined = lines.join("\n");
    if text.ends_with('\n') {
        joined.push('\n');
    }
    joined
}

/// The index of the first line of `lines`, from `from` on, that `matches`.
fn find(lines: &[&str], from: usize, matches: impl Fn(&str) -> bool) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .skip(from)
        .find(|(_, line)| matches(line))
        .map(|(index, _)| index)
}

/// `text` with `insert` before line `at`.
fn before(text: &str, at: usize, insert: &str) -> String {
    let mut all: Vec<&str> = text.lines().collect();
    all.splice(at..at, insert.lines());
    joined(&all, text)
}

/// The composition root with the module's import, its routes merged into those that `routes`
/// mounts, before the last link of its chain, and its document among those `openapi` merges.
fn composition(text: &str, module: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let imports = find(&lines, 0, |line| line.starts_with("use crate::modules::{"))?;
    let imports_end = find(&lines, imports, |line| line.starts_with("};"))?;
    let block = lines[imports + 1..imports_end].join("\n");
    let block = sorted(&block, module, &[format!("    {module},")], |line| {
        let name = line.trim_start();
        let end = name.find([',', ':'])?;
        line.starts_with("    ")
            .then(|| name[..end].to_owned())
            .filter(|found| !found.is_empty() && !found.starts_with("//"))
    })?;
    let mut all: Vec<String> = lines[..=imports]
        .iter()
        .map(|line| (*line).to_owned())
        .collect();
    all.extend(block.lines().map(str::to_owned));
    all.extend(lines[imports_end..].iter().map(|line| (*line).to_owned()));
    let text = joined(&all.iter().map(String::as_str).collect::<Vec<_>>(), text);
    let lines: Vec<&str> = text.lines().collect();
    let routes = find(&lines, 0, |line| line.starts_with("fn routes("))?;
    let routes_end = find(&lines, routes, |line| line == "}")?;
    // The chain's last link, which ends the function, outside any part's markers.
    let last = routes_end.checked_sub(1).filter(|last| *last > routes)?;
    let indent = indentation(lines[last]);
    let text = before(&text, last, &format!("{indent}.merge({module}::routes())"));
    let lines: Vec<&str> = text.lines().collect();
    let document = find(&lines, 0, |line| {
        line.contains("platform::http::document([")
    })?;
    let document_end = find(&lines, document, |line| line.trim() == "])")?;
    let indent = format!("{}    ", indentation(lines[document_end]));
    let text = before(
        &text,
        document_end,
        &format!("{indent}{module}::routes().openapi(service),"),
    );
    // The module's personal data contract, which a deletion runs before the contract of
    // `accounts`, which stays last (DATA-23).
    let lines: Vec<&str> = text.lines().collect();
    let last = find(&lines, 0, |line| {
        line.trim_start()
            .starts_with("Arc::new(accounts::Contract::new(")
    })?;
    let indent = indentation(lines[last]);
    Some(before(
        &text,
        last,
        &format!("{indent}Arc::new({module}::Contract),"),
    ))
}

/// The leading whitespace of `line`.
fn indentation(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// The permission matrix's file with the module's types above the matrix's documentation, and
/// its rows at the matrix's end.
fn permissions(text: &str, types: &str, rows: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let matrix = find(&lines, 0, |line| line.starts_with("const MATRIX"))?;
    let matrix_end = find(&lines, matrix, |line| line.trim() == "];")?;
    let text = before(text, matrix_end, rows);
    let mut documented = matrix;
    while documented > 0 && lines[documented - 1].starts_with("///") {
        documented -= 1;
    }
    Some(before(
        &text,
        documented,
        &format!("{}\n\n", types.trim_end()),
    ))
}

/// `text` with `insert` after the last statement that starts with `start`, which may span lines.
fn after_statement(text: &str, start: &str, insert: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let last = lines.iter().rposition(|line| line.starts_with(start))?;
    let end = find(&lines, last, |line| line.trim_end().ends_with(';'))?;
    Some(before(text, end + 1, insert))
}

/// TEST-08's test with the module's fixtures at the end of its `FIXTURES`.
fn test_08(text: &str, fixtures: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let table = find(&lines, 0, |line| line.starts_with("const FIXTURES"))?;
    let table_end = find(&lines, table, |line| line == "];")?;
    Some(before(text, table_end, fixtures))
}

/// architecture.md with the module's row of what each module owns, before `platform`'s, which
/// isn't a module of `app`, and its edges at the end of the graph of dependencies. A module that
/// the document plans already, such as one a later step builds, keeps its planned row and edges,
/// and gains a line with the edges its code needs that the plan lacks.
fn architecture(text: &str, owns: &str, graph: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let heading = find(&lines, 0, |line| line == "### What each module owns")?;
    let table = find(&lines, heading, |line| line.starts_with('|'))?;
    let table_end = find(&lines, table, |line| !line.starts_with('|')).unwrap_or(lines.len());
    let cell = owns.split(" | ").next().unwrap_or(owns);
    let planned = lines[table..table_end]
        .iter()
        .any(|line| line.starts_with(&format!("{cell} |")));
    let at = find(&lines, table, |line| line.starts_with("| `platform` |"))
        .filter(|at| *at < table_end)
        .unwrap_or(table_end);
    let text = if planned {
        text.to_owned()
    } else {
        before(text, at, owns)
    };
    let lines: Vec<&str> = text.lines().collect();
    let heading = find(&lines, 0, |line| line == "### Dependencies")?;
    let diagram = find(&lines, heading, |line| line == "```mermaid")?;
    let diagram_end = find(&lines, diagram + 1, |line| line == "```")?;
    let (source, targets) = graph.trim_end().split_once(" --> ")?;
    let planned: Vec<&str> = lines[diagram..diagram_end]
        .iter()
        .filter_map(|line| line.strip_prefix(&format!("{source} --> ")))
        .flat_map(|planned| planned.split(" & "))
        .map(str::trim)
        .collect();
    let missing: Vec<&str> = targets
        .split(" & ")
        .map(str::trim)
        .filter(|target| !planned.contains(target))
        .collect();
    Some(if missing.is_empty() {
        text
    } else {
        before(
            &text,
            diagram_end,
            &format!("{source} --> {}", missing.join(" & ")),
        )
    })
}

/// `text` with `rows` after the last row of the first table below the heading `heading`.
fn after_table(text: &str, heading: &str, rows: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    let at = find(&lines, 0, |line| line == heading)?;
    let table = find(&lines, at, |line| line.starts_with('|'))?;
    let table_end = find(&lines, table, |line| !line.starts_with('|')).unwrap_or(lines.len());
    Some(before(text, table_end, rows))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arch_03_a_module_s_line_goes_in_order_above_what_the_next_carries() {
        let modules = "//! Modules.\n\npub(crate) mod access;\n// part: offers\n\
                       pub(crate) mod offers;\n// end part: offers\n\
                       pub(crate) mod realtime;\n";
        let declared = |line: &str| entry(line, "pub(crate) mod ", ";");

        let between = sorted(
            modules,
            "items",
            &["pub(crate) mod items;".to_owned()],
            declared,
        );
        let last = sorted(
            modules,
            "widgets",
            &["pub(crate) mod widgets;".to_owned()],
            declared,
        );

        assert_eq!(
            between.unwrap(),
            "//! Modules.\n\npub(crate) mod access;\npub(crate) mod items;\n\
             // part: offers\npub(crate) mod offers;\n// end part: offers\n\
             pub(crate) mod realtime;\n"
        );
        assert!(
            last.unwrap()
                .ends_with("pub(crate) mod realtime;\npub(crate) mod widgets;\n")
        );
        let tests = "#[cfg(test)]\nmod access;\n// part: offers\n#[cfg(test)]\nmod offers;\n\
                     // end part: offers\n";
        let lines = ["#[cfg(test)]".to_owned(), "mod items;".to_owned()];
        assert_eq!(
            sorted(tests, "items", &lines, |line| entry(line, "mod ", ";")).unwrap(),
            "#[cfg(test)]\nmod access;\n#[cfg(test)]\nmod items;\n// part: offers\n\
             #[cfg(test)]\nmod offers;\n// end part: offers\n"
        );
    }

    #[test]
    fn arch_03_the_composition_root_imports_mounts_and_documents_the_module() {
        let root = "use crate::modules::{\n    access,\n    realtime::{self, Realtime},\n    \
                    system,\n};\n\nfn routes() -> Routes {\n    system::routes()\n        \
                    .merge(access::routes())\n        .merge(realtime::routes())\n}\n\n\
                    fn openapi() {\n    platform::http::document([\n        \
                    system::routes().openapi(service),\n    ])\n}\n\nfn contracts() {\n    \
                    vec![\n        Arc::new(access::Contract),\n        \
                    Arc::new(accounts::Contract::new(mail.clone())),\n    ]\n}\n";

        assert_eq!(
            composition(root, "items").unwrap(),
            "use crate::modules::{\n    access,\n    items,\n    realtime::{self, Realtime},\n    \
             system,\n};\n\nfn routes() -> Routes {\n    system::routes()\n        \
             .merge(access::routes())\n        .merge(items::routes())\n        \
             .merge(realtime::routes())\n}\n\nfn openapi() {\n    platform::http::document([\n        \
             system::routes().openapi(service),\n        items::routes().openapi(service),\n    \
             ])\n}\n\nfn contracts() {\n    vec![\n        Arc::new(access::Contract),\n        \
             Arc::new(items::Contract),\n        \
             Arc::new(accounts::Contract::new(mail.clone())),\n    ]\n}\n"
        );
        assert_eq!(composition("fn compose() {}\n", "items"), None);
        assert_eq!(
            composition(
                "use crate::modules::{\n    system,\n};\n\nfn routes() {\n}\n",
                "items"
            ),
            None
        );
    }

    #[test]
    fn arch_03_a_shared_file_without_its_place_gets_no_edit_and_is_named() {
        let inserts = Inserts {
            permissions: String::new(),
            matrix: String::new(),
            access_exports: String::new(),
            test_08_fixtures: String::new(),
            architecture_owns: String::new(),
            architecture_graph: String::new(),
            spec_index: String::new(),
            access_matrix: String::new(),
        };
        let names = Names::new("items", None).unwrap();

        let missing = match edits(&names, &inserts, |path| {
            (path != "docs/spec/README.md").then(String::new)
        }) {
            Ok(_) => Vec::new(),
            Err(missing) => missing,
        };

        assert_eq!(missing.len(), 10);
        assert!(
            missing[0].starts_with("crates/app/src/modules/mod.rs: add `pub(crate) mod items;`")
        );
    }

    #[test]
    fn arch_03_rows_go_after_a_table_and_before_platform_s_row() {
        let document = "### What each module owns\n\n| Module | Owns |\n|---|---|\n\
                            | `access` | x |\n| `platform` | y |\n\nText.\n\n### Dependencies\n\n\
                            ```mermaid\nflowchart TD\n  audit --> access\n```\n";

        assert_eq!(
            architecture(document, "| `files` | z |", "  files --> access").unwrap(),
            "### What each module owns\n\n| Module | Owns |\n|---|---|\n\
             | `access` | x |\n| `files` | z |\n| `platform` | y |\n\nText.\n\n### Dependencies\n\n\
             ```mermaid\nflowchart TD\n  audit --> access\n  files --> access\n```\n"
        );
        let planned = "### What each module owns\n\n| Module | Owns |\n|---|---|\n\
                       | `files` | planned |\n| `platform` | y |\n\n### Dependencies\n\n\
                       ```mermaid\nflowchart TD\n  files --> access & audit\n```\n";
        assert_eq!(
            architecture(planned, "| `files` | z |", "  files --> access").unwrap(),
            planned
        );
        // A plan whose edges lack some that the module's code needs gains a line with those.
        let lacking = planned.replace("  files --> access & audit", "  files --> accounts");
        assert_eq!(
            architecture(&lacking, "| `files` | z |", "  files --> access & audit").unwrap(),
            lacking.replace(
                "  files --> accounts\n",
                "  files --> accounts\n  files --> access & audit\n"
            )
        );
        assert_eq!(
            after_table(
                "## Specs\n\n| A | B |\n|---|---|\n| a | b |\n",
                "## Specs",
                "| c | d |"
            )
            .unwrap(),
            "## Specs\n\n| A | B |\n|---|---|\n| a | b |\n| c | d |\n"
        );
    }
}
