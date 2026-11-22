//! A final release's section of `CHANGELOG.md`, which `just release` and the run that releases
//! read before the version releases (DOC-11, OPS-16).

use super::Version;

/// The release notes for forks.
pub const CHANGELOG: &str = "CHANGELOG.md";

/// The heading under which a section says what a fork does to take the release (DOC-11).
const UPGRADE_NOTES: &str = "### Upgrade notes";

/// The heading under which a section holds the lines that the load tests end with (OPS-16).
const CAPACITY: &str = "### Capacity";

/// How the two lines under [`CAPACITY`] start: `just load-test`'s, and `just load-test
/// streams`'s.
const MEASURED: [&str; 2] = ["Capacity: ", "Hub capacity: "];

/// What `changelog` lacks for the release of `requested`, or `None` if it lacks nothing. A
/// prerelease needs no section. A final release needs:
///
/// - a line that is `## [<version>] - ` and ten characters, digits but for a hyphen as the fifth
///   and the eighth, as a date is written with its year first. The section is the lines after
///   the first such line, up to the next line that starts with `## `;
/// - a line that isn't blank between the section's first line that is `### Upgrade notes` and
///   the next line that starts with `#`;
/// - between the section's first line that is `### Capacity` and the next line that starts
///   with `#`, a line that starts with `Capacity: ` and one that starts with `Hub capacity: `,
///   each after a list's `- ` or without one.
///
/// Whitespace at a line's end doesn't count.
pub fn lacks(changelog: &str, requested: Version) -> Option<String> {
    if requested.is_prerelease() {
        return None;
    }
    let heading = format!("## [{requested}] - ");
    let mut lines = changelog
        .lines()
        .map(str::trim_end)
        .skip_while(|line| !line.strip_prefix(&heading).is_some_and(is_date));
    if lines.next().is_none() {
        return Some(format!(
            "{CHANGELOG} has no section for {requested}: no line `{heading}` with the release's \
             date, as 2026-01-31."
        ));
    }
    let section: Vec<&str> = lines.take_while(|line| !line.starts_with("## ")).collect();
    let said = |heading: &str| under(&section, heading).is_some_and(|lines| !lines.is_empty());
    if !said(UPGRADE_NOTES) {
        return Some(format!(
            "The section of {requested} in {CHANGELOG} has no upgrade notes: no line \
             `{UPGRADE_NOTES}` with what a fork does to take the release under it, or that \
             there is nothing to do."
        ));
    }
    let measured = under(&section, CAPACITY).unwrap_or_default();
    let missing: Vec<String> = MEASURED
        .iter()
        .filter(|start| {
            !measured
                .iter()
                .any(|line| line.strip_prefix("- ").unwrap_or(line).starts_with(**start))
        })
        .map(|start| format!("`{}`", start.trim_end()))
        .collect();
    if missing.is_empty() {
        return None;
    }
    Some(format!(
        "The section of {requested} in {CHANGELOG} lacks what one replica serves: under a line \
         `{CAPACITY}`, the line that starts with {}, which `just load-test` and `just load-test \
         streams` end with.",
        missing.join(" and the one that starts with ")
    ))
}

/// Whether `text` has a date's form: ten characters, digits but for a hyphen as the fifth and
/// the eighth.
fn is_date(text: &str) -> bool {
    text.len() == 10
        && text.bytes().enumerate().all(|(index, byte)| match index {
            4 | 7 => byte == b'-',
            _ => byte.is_ascii_digit(),
        })
}

/// The lines of `section` that aren't blank, between its first line that is `heading` and the
/// next line that starts with `#`, or `None` if no line is `heading`.
fn under<'a>(section: &[&'a str], heading: &str) -> Option<Vec<&'a str>> {
    let start = section.iter().position(|line| *line == heading)?;
    Some(
        section
            .iter()
            .skip(start + 1)
            .take_while(|line| !line.starts_with('#'))
            .filter(|line| !line.is_empty())
            .copied()
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        text.parse().unwrap()
    }

    /// A changelog whose section of 1.4.0 lacks nothing, between the section of what no release
    /// holds yet and that of the release before.
    const WRITTEN: &str = "# Changelog\n\
        \n\
        ## [Unreleased]\n\
        \n\
        ## [1.4.0] - 2026-01-31\n\
        \n\
        ### Added\n\
        \n\
        - A route.\n\
        \n\
        ### Upgrade notes\n\
        \n\
        None.\n\
        \n\
        ### Capacity\n\
        \n\
        - Capacity: 900 requests a second.\n\
        - Hub capacity: 400 events a second.\n\
        \n\
        ## [1.3.0] - 2025-12-01\n\
        \n\
        ### Upgrade notes\n\
        \n\
        Apply the migrations.\n";

    /// [`WRITTEN`] with each line that `changes` names replaced, or left out where the
    /// replacement is empty.
    fn written(changes: &[(&str, &str)]) -> String {
        let mut text = String::new();
        for line in WRITTEN.lines() {
            match changes.iter().find(|(from, _)| *from == line) {
                Some((_, "")) => {}
                Some((_, to)) => text.extend([to, "\n"]),
                None => text.extend([line, "\n"]),
            }
        }
        assert_ne!(text, WRITTEN, "a change names a line the changelog holds");
        text
    }

    #[test]
    fn doc_11_a_final_release_with_its_section_written_lacks_nothing() {
        assert_eq!(lacks(WRITTEN, v("1.4.0")), None);
        // Spaces at a line's end, and the lines without a list's marks, change nothing.
        let plain = written(&[
            ("## [1.4.0] - 2026-01-31", "## [1.4.0] - 2026-01-31  "),
            (
                "- Capacity: 900 requests a second.",
                "Capacity: 900 requests a second.",
            ),
        ]);
        assert_eq!(lacks(&plain, v("1.4.0")), None);
    }

    #[test]
    fn doc_11_a_prerelease_needs_no_section() {
        assert_eq!(lacks("# Changelog\n", v("1.5.0-rc.1")), None);
    }

    #[test]
    fn doc_11_a_final_release_needs_a_section_under_its_version_and_a_date() {
        let missing = "CHANGELOG.md has no section for 1.4.0: no line `## [1.4.0] - ` with the \
                       release's date, as 2026-01-31.";
        for heading in [
            "",
            "## [1.4.0]",
            "## [1.4.0] - January 31",
            "## [1.4.0] - 2026-1-31",
            "## [1.4.0] - 2026-01-31 (final)",
            "## 1.4.0 - 2026-01-31",
            "### [1.4.0] - 2026-01-31",
            "## [1.4.0-rc.1] - 2026-01-31",
            "## [11.4.0] - 2026-01-31",
        ] {
            let changelog = written(&[("## [1.4.0] - 2026-01-31", heading)]);

            assert_eq!(
                lacks(&changelog, v("1.4.0")).as_deref(),
                Some(missing),
                "{heading}"
            );
        }
    }

    #[test]
    fn doc_11_a_final_release_needs_upgrade_notes_in_its_own_section() {
        let missing = "The section of 1.4.0 in CHANGELOG.md has no upgrade notes: no line `### \
                       Upgrade notes` with what a fork does to take the release under it, or \
                       that there is nothing to do.";
        // Without the heading, the notes of the release before aren't this one's; with it and
        // nothing under it, neither is what stands under the next heading.
        for changes in [
            &[("### Upgrade notes", "")][..],
            &[("### Upgrade notes", "#### Upgrade notes")],
            &[("None.", "")],
        ] {
            assert_eq!(
                lacks(&written(changes), v("1.4.0")).as_deref(),
                Some(missing),
                "{changes:?}"
            );
        }
        assert_eq!(
            lacks(WRITTEN, v("1.3.0")).map(|lack| lack.contains("lacks what one replica serves")),
            Some(true)
        );
    }

    #[test]
    fn ops_16_a_final_release_needs_both_load_tests_lines_under_capacity() {
        let lacks_line = |changes: &[(&str, &str)]| {
            let lack = lacks(&written(changes), v("1.4.0")).unwrap();
            let (_, named) = lack.split_once("the line that starts with ").unwrap();
            let (named, _) = named.split_once(", which").unwrap();
            named.to_owned()
        };

        assert_eq!(
            lacks_line(&[("- Capacity: 900 requests a second.", "")]),
            "`Capacity:`"
        );
        assert_eq!(
            lacks_line(&[("- Hub capacity: 400 events a second.", "- Capacity: 400.")]),
            "`Hub capacity:`"
        );
        assert_eq!(
            lacks_line(&[("### Capacity", "")]),
            "`Capacity:` and the one that starts with `Hub capacity:`"
        );
        // A line of another subsection isn't the capacity's.
        assert_eq!(
            lacks_line(&[("### Capacity", "### Capacity\n\n### Notes")]),
            "`Capacity:` and the one that starts with `Hub capacity:`"
        );
    }
}
