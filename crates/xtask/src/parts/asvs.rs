//! The rows of the security map that a removal keeps (ARCH-07): of each requirement whose every
//! row the removal would delete, it keeps the first, unmarked, with [`NOT_APPLICABLE`] as its last
//! cell, so the map keeps a row for each requirement.

use std::collections::HashMap;

use super::markers;
use crate::{Finding, catalog::Catalog, repo::Repo, text};

/// The security map, from the repository root.
pub(super) const MAP: &str = "docs/operations/asvs.md";

/// The last cell of a row that a removal keeps.
const NOT_APPLICABLE: &str = "Not applicable: the API has nothing that the requirement is about";

/// `text`, the map, with the first row of each requirement whose every row a part of `removed`
/// owns made a row of the core's, which reads [`NOT_APPLICABLE`], or `None` when there is no such
/// requirement, or when the markers of `text`, among the parts `known`, don't read.
pub(super) fn keeping(text: &str, removed: &[&str], known: &[&str]) -> Option<String> {
    let owners = markers::owners(text, known).ok()?;
    let lines: Vec<&str> = text.lines().collect();
    let mut rows: HashMap<&str, Vec<usize>> = HashMap::new();
    for (index, line) in lines.iter().enumerate() {
        if let Some(requirement) = requirement(line) {
            rows.entry(requirement).or_default().push(index);
        }
    }
    let deleted = |index: &usize| {
        owners
            .get(*index)
            .is_some_and(|parts| parts.iter().any(|part| removed.contains(part)))
    };
    let first: Vec<usize> = rows
        .values()
        .filter(|indices| indices.iter().all(deleted))
        .filter_map(|indices| indices.first().copied())
        .collect();
    if first.is_empty() {
        return None;
    }
    let mut kept = String::with_capacity(text.len());
    for (index, line) in lines.iter().enumerate() {
        let row = first.contains(&index).then(|| not_applicable(line));
        kept.push_str(row.flatten().as_deref().unwrap_or(line));
        kept.push('\n');
    }
    Some(kept)
}

/// The requirement that `line` is a row of, such as `5.2.1`, if its first cell names one.
fn requirement(line: &str) -> Option<&str> {
    let cell = line.strip_prefix("| ")?.split(" | ").next()?;
    let number = |part: &str| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
    (cell.split('.').count() == 3 && cell.split('.').all(number)).then_some(cell)
}

/// `row` with its last cell, and the marker in it, replaced by [`NOT_APPLICABLE`].
fn not_applicable(row: &str) -> Option<String> {
    Some(format!("{}{NOT_APPLICABLE} |", first_cells(row)?))
}

/// The first three cells of `row`, which [`keeping`] keeps, with the ` | ` after the third.
fn first_cells(row: &str) -> Option<&str> {
    let mut end = 0;
    for _ in 0..3 {
        end += row[end..].find(" | ")? + 3;
    }
    Some(&row[..end])
}

/// Each row of the map whose first three cells hold a link, an ID that a rule or a spec of `repo`
/// defines, or a decision's `D` and its number: of a row that [`keeping`] makes the core's, the
/// dry run reads the line's links and IDs as the part's, so it can't tell one there that the
/// removal deletes (ARCH-07).
pub(super) fn citing_cells(repo: &Repo) -> Vec<Finding> {
    let Some(map) = repo.file(MAP) else {
        return Vec::new();
    };
    let catalog = Catalog::read(repo);
    let decision = |word: &str| {
        word.len() > 1 && word.starts_with('D') && word[1..].bytes().all(|b| b.is_ascii_digit())
    };
    let mut findings = Vec::new();
    for (index, line) in map.text.lines().enumerate() {
        let Some(cells) = requirement(line).and(first_cells(line)) else {
            continue;
        };
        let cites = cells.contains("](")
            || text::id_tokens(cells)
                .iter()
                .any(|token| catalog.defines(&token.id()))
            || cells
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(decision);
        if cites {
            findings.push(Finding::new(
                MAP,
                index + 1,
                "a requirement's number, level and words cite nothing, since a removal keeps them \
                 where it deletes the rest of a part's row: cite in the last cell (ARCH-07)",
            ));
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The marker of `part` at the end of a table's row.
    fn mark(part: &str) -> String {
        format!("<!-- {} {part} -->", "part:")
    }

    #[test]
    fn arch_07_the_cells_that_a_removal_keeps_of_a_row_cite_nothing() {
        let map = "| Requirement | Level | What it asks | How it stands |\n|---|---|---|---|\n\
                   | 5.1.1 | 2 | Uploads documented, as SHA-256 sums | Met: API-01, D12 |\n\
                   | 5.1.2 | 2 | As [the guide](../guides/a.md) says | Met |\n\
                   | 5.1.3 | 2 | As API-01 says | Met |\n\
                   | 5.1.4 | 2 | As D12 says | Met |\n";
        let rules = "# API\n\n### API-01 Paths\nThe rule.\n- Why: one.\n- Check: `review`.\n";
        let repo = Repo::of(&[(MAP, map), ("docs/standards/api.md", rules)]);

        let lines: Vec<usize> = citing_cells(&repo)
            .iter()
            .map(|finding| finding.line)
            .collect();

        assert_eq!(lines, [4, 5, 6]);
    }

    #[test]
    fn arch_07_a_requirement_that_the_removal_leaves_no_row_keeps_its_first_as_not_applicable() {
        let gifts = format!(
            "| 5.1.1 | 2 | Gift uploads documented | Met: GIFTS-001 {} |",
            mark("gifts")
        );
        let map = format!(
            "## V5\n\n| Requirement | Level | What it asks | How it stands |\n|---|---|---|---|\n\
             | 5.1.1 | 2 | Uploads documented | Met: OFFERS-001 {} |\n{gifts}\n\
             | 5.4.1 | 2 | Names in headers | Met: API-01 |\n\
             | 5.4.1 | 2 | The same, of offers | Met: OFFERS-002 {} |\n",
            mark("offers"),
            mark("offers"),
        );
        let known = ["offers", "gifts"];

        let removing = |removed: &[&str]| {
            let text = keeping(&map, removed, &known);
            let text = text.as_deref().unwrap_or(&map);
            let kept = markers::strip(MAP, text, removed, &known).unwrap().unwrap();
            kept.lines().skip(4).map(str::to_owned).collect::<Vec<_>>()
        };

        assert_eq!(
            removing(&["offers", "gifts"]),
            [
                "| 5.1.1 | 2 | Uploads documented | Not applicable: the API has nothing that the \
                 requirement is about |",
                "| 5.4.1 | 2 | Names in headers | Met: API-01 |",
            ]
        );
        assert_eq!(
            removing(&["offers"]),
            [
                gifts.as_str(),
                "| 5.4.1 | 2 | Names in headers | Met: API-01 |"
            ]
        );
    }
}
