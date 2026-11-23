//! `rename`, which gives the product a new name in every file that holds the old one, and
//! `check-rename`, which renames a copy of the repository and runs `just checks` there (ARCH-15,
//! docs/guides/starting-a-product.md).
//!
//! The product's name is the value of `PRODUCT` in [`NAMED`]: words of ASCII letters and digits,
//! with one space between two, the first starting with a capital letter, such as `Order Desk`.
//! The files hold it in two forms: the name itself, as the emails show it, and its slug, its words
//! in lowercase with a hyphen between two, such as `order-desk`, which names the binary,
//! `order-desk-api`, and so the service in telemetry. Since the name starts with a capital letter,
//! the two forms never read the same.
//!
//! The task replaces each form with the same form of the new name wherever no ASCII letter or
//! digit stands right before or after it. It changes nothing while any file it reads, or the path
//! of one, would still hold an occurrence of the old name, in any case, or already holds one of
//! the new name: an occurrence is the name's words, compared without regard to case, with nothing,
//! a space, a hyphen or an underscore between two, and no ASCII letter or digit right before or
//! after them. The character right after a backslash that no backslash escapes is an escape's,
//! such as the `t` that follows the backslash of a tab in a string, and counts as no letter or
//! digit before a form or an occurrence. The task reads every text file of the repository and
//! `.env`, but the files of the migration workspace (DOC-13), which the migration's close
//! deletes, and this file, which reads the product's name from [`NAMED`], and whose names are
//! examples, those that `check-rename` chooses from among them. In the files it reads, it leaves
//! the names of the codebases this one replaces as they are, which the close deletes too. A
//! refusal names each line that keeps it from renaming, by its file and its number, and shows
//! the line's text, but none of a line of `.env`, whose values may be secrets.

use std::{io, ops::Range, process::ExitCode};

use crate::{
    copy::{self, Check, Copy},
    references::OLD_NAMES,
    repo::{self, Repo},
};

/// The file that holds the product's name.
const NAMED: &str = "crates/app/src/modules/mod.rs";

/// What stands before the product's name in [`NAMED`], on the line that declares it.
const DECLARATION: &str = "pub(crate) const PRODUCT: &str = \"";

/// The migration workspace, which records the codebase this one replaces (DOC-13).
const MIGRATION: [&str; 2] = ["docs/inventory/", "docs/MIGRATION.md"];

/// This file, whose names are examples.
const OWN: &str = "crates/xtask/src/rename.rs";

/// The settings of the developer's machine, which `just setup` wrote from `.env.example`.
const ENV: &str = ".env";

/// The names that `check-rename` chooses from, of which it gives a copy the first that the
/// product can be renamed to, so that a fork whose name or files hold one passes the check with
/// the next: each has two words, so the copy holds a name with a space and a slug with a hyphen.
const CHECKED: [&str; 3] = ["Order Desk", "Paper Lantern", "Copper Kettle"];

/// Characters that may stand between two of a name's words in an occurrence of it.
const SEPARATORS: [u8; 3] = *b" -_";

/// The product's name.
#[derive(Debug, PartialEq)]
struct Name {
    /// Its words, as the name writes them.
    words: Vec<String>,
}

impl Name {
    /// The name that `text` writes, or why it is none.
    fn parse(text: &str) -> Result<Self, String> {
        let words: Vec<String> = text.split(' ').map(str::to_owned).collect();
        let valid = words
            .iter()
            .all(|word| !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_alphanumeric()));
        if !valid || !text.starts_with(|first: char| first.is_ascii_uppercase()) {
            return Err(format!(
                "`{text}` isn't a product's name: a name is words of ASCII letters and digits, \
                 with one space between two, the first starting with a capital letter, such as \
                 `{}`",
                CHECKED[0]
            ));
        }
        Ok(Self { words })
    }

    /// The name as people read it, such as `Order Desk`.
    fn display(&self) -> String {
        self.words.join(" ")
    }

    /// Its words in lowercase, with a hyphen between two, such as `order-desk`.
    fn slug(&self) -> String {
        self.words.join("-").to_ascii_lowercase()
    }

    /// Where `text` holds an occurrence of the name, as the module's documentation defines it.
    fn occurrences(&self, text: &str) -> Vec<Range<usize>> {
        let lowered = text.to_ascii_lowercase();
        let words: Vec<String> = self
            .words
            .iter()
            .map(|word| word.to_ascii_lowercase())
            .collect();
        let mut found: Vec<Range<usize>> = Vec::new();
        for (start, _) in lowered.match_indices(words[0].as_str()) {
            if found.last().is_some_and(|last| start < last.end) || !starts(text, start) {
                continue;
            }
            let mut end = start + words[0].len();
            for word in &words[1..] {
                let after = &lowered[end..];
                if after.starts_with(word.as_str()) {
                    end += word.len();
                } else if after
                    .as_bytes()
                    .first()
                    .is_some_and(|b| SEPARATORS.contains(b))
                    && after[1..].starts_with(word.as_str())
                {
                    end += 1 + word.len();
                } else {
                    end = 0;
                    break;
                }
            }
            if end > start && ends(text, end) {
                found.push(start..end);
            }
        }
        found
    }
}

/// Whether no ASCII letter or digit stands right before the byte `at` of `text`, or the one that
/// does is an escape's: the character right after a backslash that no backslash escapes, as an
/// odd number of backslashes before it shows.
fn starts(text: &str, at: usize) -> bool {
    let bytes = text.as_bytes();
    let Some(before) = at.checked_sub(1) else {
        return true;
    };
    if !bytes.get(before).is_some_and(u8::is_ascii_alphanumeric) {
        return true;
    }
    let backslashes = bytes[..before]
        .iter()
        .rev()
        .take_while(|byte| **byte == b'\\')
        .count();
    backslashes % 2 == 1
}

/// Whether the byte `at` of `text`, which follows what ends there, is no ASCII letter or digit.
fn ends(text: &str, at: usize) -> bool {
    text.as_bytes()
        .get(at)
        .is_none_or(|byte| !byte.is_ascii_alphanumeric())
}

/// The parts of `text` that name a codebase this one replaces, which the task leaves as they are.
fn old_codebases(text: &str) -> Vec<Range<usize>> {
    let mut spans: Vec<Range<usize>> = OLD_NAMES
        .iter()
        .flat_map(|name| {
            text.match_indices(name)
                .map(|(start, found)| start..start + found.len())
        })
        .collect();
    spans.sort_by_key(|span| span.start);
    spans
}

/// The occurrences of `name` in `text` outside what names a codebase this one replaces.
fn occurrences_outside_old(name: &Name, text: &str) -> Vec<Range<usize>> {
    let old = old_codebases(text);
    name.occurrences(text)
        .into_iter()
        .filter(|found| !old.iter().any(|span| span.contains(&found.start)))
        .collect()
}

/// `text` with each form of `old` replaced by the same form of `new`, where no ASCII letter or
/// digit stands right before or after it, outside what names a codebase this one replaces.
fn replaced(text: &str, old: &Name, new: &Name) -> String {
    let forms = [(old.display(), new.display()), (old.slug(), new.slug())];
    let spans = old_codebases(text);
    let mut out = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        if let Some(span) = spans.iter().find(|span| span.start == at) {
            out.push_str(&text[span.clone()]);
            at = span.end;
            continue;
        }
        let rest = &text[at..];
        let form = forms.iter().find(|(from, _)| {
            rest.starts_with(from.as_str()) && starts(text, at) && ends(text, at + from.len())
        });
        if let Some((from, to)) = form {
            out.push_str(to);
            at += from.len();
        } else {
            let next = rest.chars().next().map_or(1, char::len_utf8);
            out.push_str(&rest[..next]);
            at += next;
        }
    }
    out
}

/// The product's name, from the line of `text`, [`NAMED`]'s contents, that declares it.
fn current(text: &str) -> Result<Name, String> {
    let value = text
        .lines()
        .find_map(|line| line.strip_prefix(DECLARATION))
        .and_then(|rest| rest.strip_suffix("\";"))
        .ok_or_else(|| format!("{NAMED} holds no line `{DECLARATION}<name>\";`"))?;
    Name::parse(value).map_err(|error| format!("{NAMED}: {error}"))
}

/// Whether the task reads the file at `path`.
fn read(path: &str) -> bool {
    path != OWN && !MIGRATION.iter().any(|kept| path.starts_with(kept))
}

/// Each line of `text`, the file at `path`, that holds an occurrence of `name`, as
/// `<path>:<line>: <the line>`, or, for a line of `.env`, whose values may be secrets, as
/// `<path>:<line>`, with none of its text.
fn lines_naming(name: &Name, path: &str, text: &str) -> Vec<String> {
    let mut lines: Vec<String> = occurrences_outside_old(name, text)
        .iter()
        .map(|found| {
            let number = text[..found.start].matches('\n').count() + 1;
            if path == ENV {
                return format!("{path}:{number}");
            }
            let line = text.lines().nth(number - 1).unwrap_or_default().trim();
            format!("{path}:{number}: {line}")
        })
        .collect();
    lines.dedup();
    lines
}

/// The files that renaming the product `old` to `new` changes, each with its new contents, from
/// `files`, each a path and its contents, and `paths`, the path of every file, text or not; or
/// why it changes none.
fn renamed(
    old: &Name,
    new: &Name,
    files: &[(&str, &str)],
    paths: &[&str],
) -> Result<Vec<(String, String)>, String> {
    free(old, new, files, paths)?;
    let read: Vec<&(&str, &str)> = files.iter().filter(|(path, _)| read(path)).collect();
    let named_paths = paths_naming(old, paths);
    if !named_paths.is_empty() {
        return Err(format!(
            "these paths hold `{}`, which the task renames in no path: rename them first:\n  {}",
            old.display(),
            named_paths.join("\n  ")
        ));
    }
    let after: Vec<(&str, &str, String)> = read
        .iter()
        .map(|(path, text)| (*path, *text, replaced(text, old, new)))
        .collect();
    let left: Vec<String> = after
        .iter()
        .flat_map(|(path, _, text)| lines_naming(old, path, text))
        .collect();
    if !left.is_empty() {
        return Err(format!(
            "these lines hold `{}` in a form that is neither `{}` nor `{}`, so the task changed \
             nothing: write each in one of those forms, or without the name:\n  {}",
            old.display(),
            old.display(),
            old.slug(),
            left.join("\n  ")
        ));
    }
    Ok(after
        .into_iter()
        .filter(|(_, before, text)| before != text)
        .map(|(path, _, text)| (path.to_owned(), text))
        .collect())
}

/// Fails unless the product `old` can take the name `new`: neither name holds the other, and none
/// of `files` that the task reads, and none of `paths`, holds `new` already.
fn free(old: &Name, new: &Name, files: &[(&str, &str)], paths: &[&str]) -> Result<(), String> {
    if !old.occurrences(&new.display()).is_empty() || !new.occurrences(&old.display()).is_empty() {
        return Err(format!(
            "one of `{}` and `{}` holds the other, so no check could tell them apart: choose \
             another name",
            new.display(),
            old.display()
        ));
    }
    let taken: Vec<String> = files
        .iter()
        .filter(|(path, _)| read(path))
        .flat_map(|(path, text)| lines_naming(new, path, text))
        .chain(paths_naming(new, paths))
        .collect();
    if taken.is_empty() {
        return Ok(());
    }
    Err(format!(
        "these lines and paths already hold `{}`, so it couldn't be told from the text they hold \
         once it is the product's name: choose another name, or rename them first:\n  {}",
        new.display(),
        taken.join("\n  ")
    ))
}

/// The first of [`CHECKED`] that the product `old` can take as its name, as [`free`] says.
fn first_free(old: &Name, files: &[(&str, &str)], paths: &[&str]) -> Option<&'static str> {
    CHECKED.iter().copied().find(|candidate| {
        Name::parse(candidate).is_ok_and(|new| free(old, &new, files, paths).is_ok())
    })
}

/// Each of `paths` that the task reads and that holds an occurrence of `name`.
fn paths_naming(name: &Name, paths: &[&str]) -> Vec<String> {
    paths
        .iter()
        .filter(|path| read(path) && !name.occurrences(path).is_empty())
        .map(|path| (*path).to_owned())
        .collect()
}

/// Renames the product to `name`, in every file that holds a form of its name.
pub fn run(name: &str) -> ExitCode {
    match rename(name) {
        Ok((old, new, changed)) => {
            report(&format!(
                "rename: renamed `{}` to `{}`, whose slug is `{}`, in {} files:",
                old.display(),
                new.display(),
                new.slug(),
                changed.len()
            ));
            for path in &changed {
                report(&format!("  {path}"));
            }
            report(
                "Next: commit what it changed once `just checks` passes, as \
                 docs/guides/starting-a-product.md says.",
            );
            ExitCode::SUCCESS
        }
        Err(error) => fail(&format!("rename: {error}")),
    }
}

/// What the task reads: the product's name, the repository's files, and `.env`'s text, where the
/// machine has one.
struct Read {
    old: Name,
    repo: Repo,
    env: Option<String>,
}

impl Read {
    fn load() -> Result<Self, String> {
        let repo = Repo::load().map_err(|error| format!("can't read the repository: {error}"))?;
        let old = current(
            &repo
                .file(NAMED)
                .ok_or_else(|| format!("{NAMED} doesn't exist"))?
                .text,
        )?;
        let env = match repo::read(&copy::root().join(ENV)) {
            Ok(bytes) => Some(String::from_utf8(bytes).map_err(|_| format!("{ENV} isn't UTF-8"))?),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("can't read {ENV}: {error}")),
        };
        Ok(Self { old, repo, env })
    }

    /// Each text file's path and contents, `.env`'s among them.
    fn files(&self) -> Vec<(&str, &str)> {
        self.repo
            .files()
            .map(|file| (file.path.as_str(), file.text.as_str()))
            .chain(self.env.as_deref().map(|text| (ENV, text)))
            .collect()
    }

    /// The path of every file of the repository, text or not.
    fn paths(&self) -> Vec<&str> {
        self.repo.paths().collect()
    }
}

/// Renames the product to `name`, and gives its old name, its new name and the paths it changed.
fn rename(name: &str) -> Result<(Name, Name, Vec<String>), String> {
    let new = Name::parse(name)?;
    let read = Read::load()?;
    if read.old == new {
        return Err(format!("the product is named `{}` already", new.display()));
    }
    let changed = renamed(&read.old, &new, &read.files(), &read.paths())?;
    for (path, text) in &changed {
        repo::write(path, text).map_err(|error| format!("can't write {path}: {error}"))?;
    }
    copy::run(&copy::root(), "cargo", &["fmt", "--all"], &[])?;
    Ok((
        read.old,
        new,
        changed.into_iter().map(|(path, _)| path).collect(),
    ))
}

/// Runs `check-rename` (ARCH-15): on a fresh copy of the repository, renames the product with
/// `just rename`, to the first of [`CHECKED`] that it can take, then runs `just checks` there.
pub fn check() -> ExitCode {
    let name = match Read::load().and_then(|read| {
        first_free(&read.old, &read.files(), &read.paths()).ok_or_else(|| {
            format!(
                "the product can take none of the names that the check gives a copy, since its \
                 name or its files hold each: {}",
                CHECKED.join(", ")
            )
        })
    }) {
        Ok(name) => name,
        Err(error) => return fail(&format!("check-rename: {error}")),
    };
    let checked = Copy::fresh(Check::Rename, "rename").and_then(|copy| {
        copy.just(&["rename", name])?;
        copy.just(&["checks"])
    });
    match checked {
        Ok(()) => {
            report(&format!(
                "check-rename: the copy named `{name}` passed the checks"
            ));
            ExitCode::SUCCESS
        }
        Err(error) => fail(&format!(
            "check-rename: the copy named `{name}` failed: {error}"
        )),
    }
}

#[expect(clippy::print_stdout, reason = "the report is the task's output")]
fn report(message: &str) {
    println!("{message}");
}

#[expect(clippy::print_stderr, reason = "the error is the task's output")]
fn fail(message: &str) -> ExitCode {
    eprintln!("{message}");
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(text: &str) -> Name {
        Name::parse(text).unwrap()
    }

    #[test]
    fn arch_15_a_name_is_words_of_letters_and_digits_the_first_starting_with_a_capital() {
        for valid in ["Ledger", "Order Desk", "Acme 2"] {
            assert!(Name::parse(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "",
            "order desk",
            "Order  Desk",
            " Order",
            "Order ",
            "Order-Desk",
            "Ordér",
            "2Order",
        ] {
            assert!(Name::parse(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn arch_15_the_slug_is_the_words_in_lowercase_with_a_hyphen_between_two() {
        assert_eq!(name("Ledger").slug(), "ledger");
        assert_eq!(name("Order Desk 2").slug(), "order-desk-2");
        assert_eq!(name("Order Desk").display(), "Order Desk");
    }

    #[test]
    fn arch_15_an_occurrence_is_the_words_in_any_case_with_a_separator_or_none_between_two() {
        let order = name("Order Desk");
        for text in [
            "Order Desk",
            "order-desk-api",
            "ORDER_DESK",
            "OrderDesk",
            "(order desk)",
        ] {
            assert_eq!(order.occurrences(text).len(), 1, "{text}");
        }
        for text in [
            "Order  Desk",
            "ordered desk",
            "Order Desktop",
            "xorder-desk",
            "Order",
        ] {
            assert!(order.occurrences(text).is_empty(), "{text}");
        }
    }

    #[test]
    fn arch_15_each_form_becomes_the_same_form_where_no_letter_or_digit_adjoins_it() {
        let (old, new) = (name("Ledger"), name("Order Desk"));
        assert_eq!(
            replaced(
                "Your Ledger account\n--bin ledger-api\nmy-Ledger-pass\n",
                &old,
                &new
            ),
            "Your Order Desk account\n--bin order-desk-api\nmy-Order Desk-pass\n"
        );
        assert_eq!(
            replaced("Ledgers ledgerish xLedger", &old, &new),
            "Ledgers ledgerish xLedger"
        );
        let (old, new) = (name("Order Desk"), name("Acme"));
        assert_eq!(
            replaced("Order Desk order-desk-api", &old, &new),
            "Acme acme-api"
        );
    }

    #[test]
    fn arch_15_a_letter_that_an_escape_holds_adjoins_no_form() {
        let (old, new) = (name("Ledger"), name("Order Desk"));
        // A tab's escape, then the slug, as a string of a source file holds them.
        assert_eq!(
            replaced("\"Name:\\tledger-api\\nLedger\\n\"", &old, &new),
            "\"Name:\\torder-desk-api\\nOrder Desk\\n\""
        );
        assert_eq!(old.occurrences("\\nLEDGER").len(), 1);
        // A backslash that another escapes holds no letter: `tledger` is a word of its own.
        assert_eq!(
            replaced("dir\\\\tledger xledger", &old, &new),
            "dir\\\\tledger xledger"
        );
        assert!(old.occurrences("\\\\nledger").is_empty());
    }

    #[test]
    fn arch_15_what_names_a_codebase_this_one_replaces_stays_as_it_is() {
        for codebase in OLD_NAMES {
            // A product named as the codebase's name begins, such as `Name` for `Name-API`.
            let first = codebase
                .split(|c: char| !c.is_ascii_alphanumeric())
                .next()
                .unwrap();
            let (old, new) = (name(first), name("Order Desk"));
            let text = format!("As {codebase} did, and {codebase}-Rust does. {first} API");
            assert_eq!(
                replaced(&text, &old, &new),
                format!("As {codebase} did, and {codebase}-Rust does. Order Desk API")
            );
            assert!(occurrences_outside_old(&old, codebase).is_empty());
            assert_eq!(occurrences_outside_old(&old, &text).len(), 1);
        }
    }

    #[test]
    fn arch_15_the_current_name_is_the_value_of_product() {
        let text = "/// The name.\npub(crate) const PRODUCT: &str = \"Order Desk\";\n";
        assert_eq!(current(text).unwrap(), name("Order Desk"));
        assert!(current("const NAME: &str = \"Ledger\";\n").is_err());
        assert!(current("pub(crate) const PRODUCT: &str = \"order desk\";\n").is_err());
    }

    #[test]
    fn arch_15_renaming_changes_the_files_that_hold_a_form_and_leaves_the_workspace() {
        let (old, new) = (name("Ledger"), name("Order Desk"));
        let files = [
            ("README.md", "# Ledger API\n"),
            ("Cargo.toml", "name = \"ledger-api\"\n"),
            ("docs/MIGRATION.md", "From Ledger to here.\n"),
            ("docs/inventory/emails.md", "Your Ledger account\n"),
            ("src/lib.rs", "fn main() {}\n"),
        ];
        let paths: Vec<&str> = files.iter().map(|(path, _)| *path).collect();
        assert_eq!(
            renamed(&old, &new, &files, &paths).unwrap(),
            [
                ("README.md".to_owned(), "# Order Desk API\n".to_owned()),
                (
                    "Cargo.toml".to_owned(),
                    "name = \"order-desk-api\"\n".to_owned()
                )
            ]
        );
    }

    #[test]
    fn arch_15_renaming_changes_nothing_while_another_form_of_the_old_name_would_stay() {
        let (old, new) = (name("Ledger"), name("Order Desk"));
        let files = [
            ("README.md", "# Ledger API\n"),
            ("src/a.rs", "const NAME: &str = \"LEDGER\";\n"),
            ("src/b.rs", "// Ant_Worker? No: ant worker is two words.\n"),
        ];
        let paths: Vec<&str> = files.iter().map(|(path, _)| *path).collect();
        let error = renamed(&old, &new, &files, &paths).unwrap_err();
        assert!(
            error.ends_with(":\n  src/a.rs:1: const NAME: &str = \"LEDGER\";"),
            "{error}"
        );
    }

    #[test]
    fn arch_15_renaming_refuses_a_name_that_a_file_or_a_path_holds_already() {
        let (old, new) = (name("Ledger"), name("Order Desk"));
        let files = [
            ("README.md", "# Ledger API\n"),
            ("docs/x.md", "An order-desk app.\n"),
            ("docs/MIGRATION.md", "Order Desk\n"),
        ];
        let paths = [
            "README.md",
            "docs/x.md",
            "docs/MIGRATION.md",
            "art/orderdesk.png",
        ];
        let error = renamed(&old, &new, &files, &paths).unwrap_err();
        assert!(
            error.ends_with(":\n  docs/x.md:1: An order-desk app.\n  art/orderdesk.png"),
            "{error}"
        );
    }

    #[test]
    fn arch_15_a_refusal_names_a_line_of_env_by_its_number_and_shows_none_of_its_text() {
        let (old, new) = (name("Ledger"), name("Order Desk"));
        let files = [
            ("README.md", "# Ledger API\n"),
            (".env", "A=1\nMAIL_PASSWORD=my_LEDGER_secret\n# LEDGER\n"),
        ];
        let error = renamed(&old, &new, &files, &["README.md"]).unwrap_err();
        assert!(error.ends_with(":\n  .env:2\n  .env:3"), "{error}");

        let files = [(".env", "SMTP_PASSWORD=order-desk-9\n")];
        let error = renamed(&old, &new, &files, &[]).unwrap_err();
        assert!(error.ends_with(":\n  .env:1"), "{error}");
    }

    #[test]
    fn arch_15_the_check_gives_a_copy_the_first_name_that_the_product_can_take() {
        assert_eq!(CHECKED, ["Order Desk", "Paper Lantern", "Copper Kettle"]);
        let files = [("README.md", "# Ledger API\n")];
        let free_of = |old: &str, files: &[(&str, &str)], paths: &[&str]| {
            first_free(&name(old), files, paths)
        };

        assert_eq!(
            free_of("Ledger", &files, &["README.md"]),
            Some("Order Desk")
        );
        // A product of that name, or of one that holds it or that it holds, takes the next.
        for old in ["Order Desk", "Order Desk Pro", "Desk"] {
            assert_eq!(free_of(old, &files, &[]), Some("Paper Lantern"), "{old}");
        }
        // So does one whose files or paths hold it, in any form.
        let module = [("src/lib.rs", "mod order_desk;\n")];
        assert_eq!(free_of("Ledger", &module, &[]), Some("Paper Lantern"));
        let paths = ["src/order_desk/mod.rs", "art/paper-lantern.png"];
        assert_eq!(free_of("Ledger", &files, &paths), Some("Copper Kettle"));
        let all = [("a.md", "Order Desk, Paper Lantern and Copper Kettle.\n")];
        assert_eq!(free_of("Ledger", &all, &[]), None);
    }

    #[test]
    fn arch_15_renaming_refuses_a_path_that_holds_the_old_name() {
        let (old, new) = (name("Ledger"), name("Order Desk"));
        let files = [("README.md", "# Ledger API\n")];
        let paths = ["README.md", "art/ledger.svg"];
        let error = renamed(&old, &new, &files, &paths).unwrap_err();
        assert!(error.ends_with(":\n  art/ledger.svg"), "{error}");
    }

    #[test]
    fn arch_15_renaming_refuses_a_name_that_holds_the_old_one_or_that_it_holds() {
        let old = name("Ledger");
        for new in ["Ledger Pro", "My Ledger"] {
            let error = renamed(&old, &name(new), &[], &[]).unwrap_err();
            assert!(error.contains("holds the other"), "{new}: {error}");
        }
        let error = renamed(&name("Order Desk"), &name("Desk"), &[], &[]).unwrap_err();
        assert!(error.contains("holds the other"), "{error}");
    }
}
