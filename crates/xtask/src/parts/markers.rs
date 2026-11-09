//! The markers that give a line, or a block of lines, to an optional part (ARCH-07). A marker is
//! a comment that reads `part: <part>`, or `end part: <part>`, in one of the syntaxes below.

/// The comment syntaxes a marker is written in, each with what ends its comment: Rust's, the one
/// of the files whose comments start with `#`, Markdown's, and Mermaid's.
const SYNTAXES: [(&str, &str); 4] = [("//", ""), ("#", ""), ("<!--", "-->"), ("%%", "")];

/// What a marker does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// It follows other text on its line, and marks the line.
    Line,
    /// It stands alone on its line, and opens a block.
    Open,
    /// It stands alone on its line, and closes the block that its part's open marker opened.
    Close,
}

/// A marker: its part, what it does, the comment syntax it is written in, and whether it stands
/// alone on its line.
#[derive(Debug, PartialEq, Eq)]
struct Marker<'a> {
    part: &'a str,
    kind: Kind,
    syntax: &'static str,
    alone: bool,
    /// Where its comment starts in its line.
    at: usize,
    /// The line's text after its comment.
    after: &'a str,
}

/// The marker that `line` holds, if any. A marker in a syntax whose comment runs to the end of
/// the line ends its line; only Markdown's may come before more text, such as a table row's last
/// pipe.
fn marker(line: &str) -> Option<Marker<'_>> {
    SYNTAXES.into_iter().find_map(|(opener, closer)| {
        line.match_indices(opener).find_map(|(at, _)| {
            let rest = line[at + opener.len()..].trim_start();
            let (close, rest) = match rest.strip_prefix("end ") {
                Some(rest) => (true, rest),
                None => (false, rest),
            };
            let rest = rest.strip_prefix("part: ")?;
            let length = rest
                .find(|c: char| !(c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
                .unwrap_or(rest.len());
            let part = &rest[..length];
            if !part.starts_with(|c: char| c.is_ascii_lowercase()) {
                return None;
            }
            let after = rest[length..].trim_start().strip_prefix(closer)?;
            if closer.is_empty() && !after.trim().is_empty() {
                return None;
            }
            let alone = line[..at].trim().is_empty() && after.trim().is_empty();
            let kind = if close {
                Kind::Close
            } else if alone {
                Kind::Open
            } else {
                Kind::Line
            };
            Some(Marker {
                part,
                kind,
                syntax: opener,
                alone,
                at,
                after,
            })
        })
    })
}

/// `line` without its marker: none if it holds only a marker, or the text before a marker that
/// ends it, such as a comment's sentence that the deployment's setup shows.
pub fn unmarked(line: &str) -> Option<&str> {
    match marker(line) {
        Some(marker) if marker.alone => None,
        Some(marker) => Some(line[..marker.at].trim_end()),
        None => Some(line),
    }
}

/// The parts that own each line of `text`: those whose block holds it, or whose marker it holds.
/// Fails, with the line counted from 1, on a marker of a part that `known` doesn't name, an end
/// marker that doesn't stand alone or closes no block of its part, and a block that no end marker
/// closes.
pub fn owners<'a>(text: &'a str, known: &[&str]) -> Result<Vec<Vec<&'a str>>, (usize, String)> {
    let mut open: Vec<(&str, &str, usize)> = Vec::new();
    let mut owners = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let mut owning: Vec<&str> = open.iter().map(|(part, _, _)| *part).collect();
        if let Some(marker) = marker(line) {
            if !known.contains(&marker.part) {
                return Err((
                    number,
                    format!("the marker names `{}`, which is no part", marker.part),
                ));
            }
            match marker.kind {
                Kind::Line => owning.push(marker.part),
                Kind::Open => {
                    open.push((marker.part, marker.syntax, number));
                    owning.push(marker.part);
                }
                Kind::Close => match open.last() {
                    Some((part, syntax, _))
                        if *part == marker.part && *syntax == marker.syntax && marker.alone =>
                    {
                        open.pop();
                    }
                    _ => {
                        return Err((
                            number,
                            format!(
                                "the end marker of `{}` closes no block of it that this line's \
                                 syntax opened, or doesn't stand alone",
                                marker.part
                            ),
                        ));
                    }
                },
            }
        }
        owners.push(owning);
    }
    match open.last() {
        Some((part, _, line)) => {
            Err((*line, format!("no end marker closes the block of `{part}`")))
        }
        None => Ok(owners),
    }
}

/// The lines of `text` that removing the parts `removed` keeps, each with its number, counted from
/// 1. Fails as [`owners`] does.
pub fn kept<'a>(
    text: &'a str,
    removed: &[&str],
    known: &[&str],
) -> Result<Vec<(usize, &'a str)>, (usize, String)> {
    let owners = owners(text, known)?;
    Ok(text
        .lines()
        .zip(owners)
        .enumerate()
        .filter(|(_, (_, parts))| !parts.iter().any(|part| removed.contains(part)))
        .map(|(index, (line, _))| (index + 1, line))
        .collect())
}

/// The lines of Rust `text`, counted from 1, whose marker isn't Rust's `//` comment alone on its
/// line: one after code, where rustfmt can join the line to another or move the marker onto the
/// line before, or one in another file's syntax (ARCH-07).
pub fn misplaced_in_rust(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| {
            marker(line).is_some_and(|marker| marker.syntax != "//" || !marker.alone)
        })
        .map(|(index, _)| index + 1)
        .collect()
}

/// The lines of `text`, counted from 1, whose marker marks its line but comes before more of its
/// text, outside a table row: removing the part would delete that text with the line, though it
/// isn't the part's (ARCH-07). A row is the part's whole, whichever cell holds its marker, and
/// only Markdown's comment can come before more text.
pub fn text_after_marker(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| {
            !line.trim_start().starts_with('|')
                && marker(line).is_some_and(|marker| {
                    marker.kind == Kind::Line && !marker.after.trim().is_empty()
                })
        })
        .map(|(index, _)| index + 1)
        .collect()
}

/// The lines of Rust `text`, counted from 1, that hold an attribute, or the end of one, or an
/// outer doc comment, right above a marker that opens a block holding an item or a statement:
/// the attribute or the comment belongs to that item, so removing the part would leave it on the
/// one after the block (ARCH-07). A block that holds only doc comments and attributes goes on the
/// item after it, as does what stands above it.
pub fn attribute_above_block(text: &str) -> Vec<usize> {
    let lines: Vec<&str> = text.lines().collect();
    let mut found = Vec::new();
    for (index, pair) in lines.windows(2).enumerate() {
        let above = pair[0].trim();
        if !(above.starts_with("#[") || above.starts_with("///") || above.ends_with(")]")) {
            continue;
        }
        let Some(open) = marker(pair[1]).filter(|marker| marker.kind == Kind::Open) else {
            continue;
        };
        let mut in_attribute = false;
        let holds_item = lines[index + 2..]
            .iter()
            .take_while(|line| {
                !marker(line)
                    .is_some_and(|marker| marker.kind == Kind::Close && marker.part == open.part)
            })
            .any(|line| {
                let line = line.trim();
                if in_attribute || line.starts_with("#[") {
                    in_attribute = !line.ends_with(']');
                    return false;
                }
                !(line.is_empty() || line.starts_with("//"))
            });
        if holds_item {
            found.push(index + 1);
        }
    }
    found
}

/// `text` without the lines that the parts in `removed` own, or `None` if it has none of them.
pub fn strip(
    text: &str,
    removed: &[&str],
    known: &[&str],
) -> Result<Option<String>, (usize, String)> {
    let owners = owners(text, known)?;
    if !owners
        .iter()
        .any(|parts| parts.iter().any(|part| removed.contains(part)))
    {
        return Ok(None);
    }
    let mut kept = String::with_capacity(text.len());
    for (line, parts) in text.split_inclusive('\n').zip(owners) {
        if !parts.iter().any(|part| removed.contains(part)) {
            kept.push_str(line);
        }
    }
    Ok(Some(kept))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A marker of `part`, in the syntax that `opener` and `closer` give: the source of these
    /// tests holds none, so `just remove` never reads one here.
    fn mark(opener: &str, part: &str, closer: &str) -> String {
        format!("{opener} {} {part}{closer}", "part:")
    }

    fn end(opener: &str, part: &str, closer: &str) -> String {
        format!("{opener} end {} {part}{closer}", "part:")
    }

    #[test]
    fn arch_07_a_marker_after_text_marks_its_line_and_one_alone_opens_or_closes_a_block() {
        let lines = [
            format!("mod a; {}", mark("//", "one", "")),
            format!("| a | b {} |", mark("<!--", "one", " -->")),
            mark("    //", "one", ""),
            mark("  %%", "two", ""),
            end("#", "one", ""),
        ];

        let found: Vec<Option<(&str, Kind)>> = lines
            .iter()
            .map(|line| marker(line).map(|marker| (marker.part, marker.kind)))
            .collect();

        assert_eq!(
            found,
            [
                Some(("one", Kind::Line)),
                Some(("one", Kind::Line)),
                Some(("one", Kind::Open)),
                Some(("two", Kind::Open)),
                Some(("one", Kind::Close)),
            ]
        );
    }

    #[test]
    fn arch_07_text_that_only_looks_like_a_marker_isn_t_one() {
        let lines = [
            format!("{} more", mark("//", "one", "")),
            mark("//", "<part>", ""),
            mark("<!--", "one", ""),
            format!("the first {} jobs", "part:"),
            "#[derive(Debug)]".to_owned(),
        ];

        let found: Vec<Option<Marker<'_>>> = lines.iter().map(|line| marker(line)).collect();

        assert_eq!(found, [None, None, None, None, None]);
    }

    /// Lines of the parts `one` and `two`: a line of `two` inside a block of `one`, and one after
    /// it.
    fn two_parts() -> String {
        [
            "a".to_owned(),
            format!("b {}", mark("//", "one", "")),
            mark("//", "one", ""),
            "c".to_owned(),
            format!("d {}", mark("//", "two", "")),
            end("//", "one", ""),
            format!("e {}", mark("//", "two", "")),
            String::new(),
        ]
        .join("\n")
    }

    #[test]
    fn arch_07_removing_a_part_deletes_its_lines_and_its_blocks_with_all_they_hold() {
        let text = two_parts();

        let kept = strip(&text, &["one"], &["one", "two"]).unwrap();

        assert_eq!(kept, Some(format!("a\ne {}\n", mark("//", "two", ""))));
    }

    #[test]
    fn arch_07_removing_a_part_keeps_the_lines_and_blocks_of_another() {
        let text = two_parts();

        let kept = strip(&text, &["two"], &["one", "two"]).unwrap().unwrap();

        let starts: Vec<&str> = kept.lines().map(|line| &line[..1]).collect();
        assert_eq!(starts, ["a", "b", "/", "c", "/"], "{kept}");
    }

    #[test]
    fn arch_07_a_file_that_holds_none_of_a_part_s_lines_is_left_as_it_is() {
        let text = "a\nb\n";

        let kept = strip(text, &["one"], &["one"]).unwrap();

        assert_eq!(kept, None);
    }

    #[test]
    fn arch_07_a_removal_keeps_the_other_lines_with_their_numbers() {
        let text = format!(
            "a\n{}\nb\n{}\nc\n",
            mark("//", "one", ""),
            end("//", "one", "")
        );

        let lines = kept(&text, &["one"], &["one"]).unwrap();

        assert_eq!(lines, [(1, "a"), (5, "c")]);
    }

    #[test]
    fn arch_07_a_rust_marker_after_code_or_in_another_syntax_fails_and_one_alone_doesn_t() {
        let text = [
            format!("    1, {}", mark("//", "one", "")),
            mark("    //", "one", ""),
            "    2,".to_owned(),
            end("    //", "one", ""),
            format!("//! A note. {}", mark("<!--", "one", " -->")),
            mark("#", "one", ""),
        ]
        .join("\n");

        let lines = misplaced_in_rust(&text);

        assert_eq!(lines, [1, 5, 6]);
    }

    #[test]
    fn arch_07_a_marker_before_more_of_its_line_fails_outside_a_table_row() {
        let text = [
            format!("A sentence. {} Another.", mark("<!--", "one", " -->")),
            format!("| a | b {} |", mark("<!--", "one", " -->")),
            format!("| a {} | b |", mark("<!--", "one", " -->")),
            format!("A sentence. {}", mark("<!--", "one", " -->")),
            format!("a = 1 {}", mark("#", "one", "")),
            mark("<!--", "one", " -->"),
            end("<!--", "one", " -->"),
        ]
        .join("\n");

        let lines = text_after_marker(&text);

        assert_eq!(lines, [1]);
    }

    #[test]
    fn arch_07_an_attribute_or_a_doc_comment_above_a_block_that_holds_an_item_fails() {
        let text = [
            "#[cfg(test)]".to_owned(),
            mark("//", "one", ""),
            "mod a;".to_owned(),
            end("//", "one", ""),
            "/// A module.".to_owned(),
            mark("//", "one", ""),
            "mod b;".to_owned(),
            end("//", "one", ""),
            "#[expect(".to_owned(),
            "    reason = \"r\"".to_owned(),
            ")]".to_owned(),
            mark("    //", "one", ""),
            "let c = 1;".to_owned(),
            end("    //", "one", ""),
            mark("//", "one", ""),
            "#[cfg(test)]".to_owned(),
            "mod d;".to_owned(),
            end("//", "one", ""),
            "mod e;".to_owned(),
            mark("//", "one", ""),
            "mod f;".to_owned(),
            end("//", "one", ""),
            "/// A function.".to_owned(),
            mark("//", "one", ""),
            "/// What the part adds to its doc.".to_owned(),
            "#[expect(".to_owned(),
            "    reason = \"r\"".to_owned(),
            ")]".to_owned(),
            end("//", "one", ""),
            "fn g() {}".to_owned(),
        ]
        .join("\n");

        let lines = attribute_above_block(&text);

        assert_eq!(lines, [1, 5, 11]);
    }

    #[test]
    fn arch_07_a_marker_of_no_part_or_a_block_that_doesn_t_close_fails_at_its_line() {
        let texts = [
            mark("#", "three", ""),
            format!("a\n{}\nb\n", mark("//", "one", "")),
            format!("{}\n{}\n", mark("//", "one", ""), end("#", "one", "")),
            format!("a\n{}\n", end("<!--", "one", " -->")),
        ];

        let lines: Vec<Option<usize>> = texts
            .iter()
            .map(|text| owners(text, &["one"]).err().map(|(line, _)| line))
            .collect();

        assert_eq!(lines, [Some(1), Some(2), Some(2), Some(2)]);
    }
}
