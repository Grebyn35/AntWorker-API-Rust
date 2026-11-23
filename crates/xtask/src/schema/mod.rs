//! `check-schema`: the schema that the migrations build, as a database's catalog shows it,
//! follows the data rules: the names of tables, columns, constraints and indexes (DATA-01), the
//! standard columns (DATA-02), no check constraint that reads a single column (DATA-04), no
//! function but the one that maintains `updated_at` and `version` (DATA-07), foreign keys with an
//! explicit `ON DELETE` and an index (DATA-05), no soft-delete column (DATA-06), no constraint
//! that the catalog marks as not valid (DATA-10), and what the roles own, hold and have set
//! (DATA-12, DATA-19).
//!
//! It reads the catalog of a database that the API binary's own `bootstrap` and `migrate` build
//! (`catalog`), rather than parsing the migrations' SQL, so it sees what Postgres made of them,
//! and checks it against the rules (`rules`). A finding names the last migration that mentions
//! the object it concerns.

mod catalog;
mod roles;
mod rules;

use crate::{Finding, Outcome, markdown, repo::Repo, text};

/// The migrations.
const MIGRATIONS: &str = "migrations/";

/// The document that says which module owns each table.
const ARCHITECTURE: &str = "docs/architecture.md";

/// Checks the migrations' schema.
pub fn check(repo: &Repo) -> Outcome {
    let findings = match catalog::read() {
        Ok(catalog) => findings(repo, &catalog),
        Err(error) => vec![Finding::new(MIGRATIONS, 1, error)],
    };
    Outcome {
        report: Vec::new(),
        findings,
    }
}

/// The findings of `catalog`, each at the migration that [`locate`] names.
fn findings(repo: &Repo, catalog: &serde_json::Value) -> Vec<Finding> {
    rules::check(catalog, &bookkeeping(repo))
        .into_iter()
        .map(|(subject, message)| locate(repo, &subject, message))
        .collect()
}

/// The tables of `platform`'s bookkeeping: those that architecture.md §5 gives `platform`.
fn bookkeeping(repo: &Repo) -> Vec<String> {
    let text = repo
        .file(ARCHITECTURE)
        .map_or("", |file| file.text.as_str());
    markdown::tables(text)
        .into_iter()
        .filter(|table| table.header.first().is_some_and(|cell| cell == "Module"))
        .flat_map(|table| table.rows)
        .filter(|(_, cells)| cells.first().is_some_and(|cell| cell == "`platform`"))
        .filter_map(|(_, cells)| cells.get(1).cloned())
        .flat_map(|cell| {
            cell.split('`')
                .skip(1)
                .step_by(2)
                .map(str::to_owned)
                .collect::<Vec<String>>()
        })
        .collect()
}

/// The finding of `message` at the last migration that mentions `subject`, or at the
/// migrations' directory.
fn locate(repo: &Repo, subject: &str, message: String) -> Finding {
    let mut migrations: Vec<_> = repo
        .files()
        .filter(|file| file.path.starts_with(MIGRATIONS) && file.path.ends_with(".sql"))
        .collect();
    migrations.sort_by(|a, b| a.path.cmp(&b.path));
    migrations
        .iter()
        .rev()
        .find(|file| file.text.contains(subject))
        .map_or_else(
            || Finding::new(MIGRATIONS, 1, message.clone()),
            |file| {
                Finding::new(
                    &file.path,
                    text::line_of(&file.text, subject),
                    message.clone(),
                )
            },
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_02_the_bookkeeping_tables_are_those_architecture_md_gives_platform() {
        let architecture = "## 5. Modules\n\n| Module | Owns (tables) | Responsible for |\n|---|---|---|\n| `audit` | `audit_entries` | Entries. |\n| `platform` | `events`, `rate_limit_buckets` | The mechanisms. |\n";
        let repo = Repo::of(&[(ARCHITECTURE, architecture)]);
        assert_eq!(bookkeeping(&repo), ["events", "rate_limit_buckets"]);
    }

    #[test]
    fn data_01_a_misnamed_constraint_is_found_at_the_last_migration_that_mentions_it() {
        let repo = Repo::of(&[
            (
                "migrations/2_b.sql",
                "-- users_key\nALTER TABLE users ADD CONSTRAINT users_key PRIMARY KEY (id);\n",
            ),
            (
                "migrations/1_a.sql",
                "CREATE TABLE users (id uuid, CONSTRAINT users_key PRIMARY KEY (id));\n",
            ),
        ]);
        let constraint = |name: &str| {
            serde_json::json!({"constraints": [
                {"name": name, "table": "users", "type": "p", "columns": ["id"]}
            ]})
        };
        let named = |found: Vec<Finding>| -> Vec<(String, usize)> {
            found
                .into_iter()
                .filter(|finding| finding.message.ends_with("(DATA-01)"))
                .map(|finding| (finding.path, finding.line))
                .collect()
        };

        assert_eq!(
            named(findings(&repo, &constraint("users_key"))),
            [("migrations/2_b.sql".to_owned(), 1)]
        );
        assert_eq!(
            named(findings(&repo, &constraint("users_gone"))),
            [(MIGRATIONS.to_owned(), 1)]
        );
        assert_eq!(named(findings(&repo, &constraint("users_pkey"))), []);
    }
}
