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

/// The lines of Rust `text`, counted from 1, whose marker follows code, where rustfmt can join
/// the line to another or move the marker onto the line before (ARCH-07).
pub fn after_code(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter(|(_, line)| {
            marker(line).is_some_and(|marker| marker.syntax == "//" && !marker.alone)
        })
        .map(|(index, _)| index + 1)
        .collect()
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
    fn arch_07_a_marker_after_text_marks_its_line_and_one_alone_opens_a_block() {
        let rust = format!("mod a; {}", mark("//", "one", ""));
        assert_eq!(marker(&rust).unwrap().kind, Kind::Line);
        assert_eq!(marker(&mark("    //", "one", "")).unwrap().kind, Kind::Open);
        assert_eq!(marker(&end("#", "one", "")).unwrap().kind, Kind::Close);
        let row = format!("| a | b {} |", mark("<!--", "one", " -->"));
        assert_eq!(marker(&row).unwrap().kind, Kind::Line);
        assert_eq!(marker(&mark("  %%", "one", "")).unwrap().part, "one");
    }

    #[test]
    fn arch_07_text_that_only_looks_like_a_marker_isn_t_one() {
        for line in [
            format!("{} more", mark("//", "one", "")),
            mark("//", "<part>", ""),
            mark("<!--", "one", ""),
            format!("the first {} jobs", "part:"),
            "#[derive(Debug)]".to_owned(),
        ] {
            assert_eq!(marker(&line), None, "{line}");
        }
    }

    #[test]
    fn arch_07_removing_a_part_deletes_its_lines_and_blocks_and_keeps_the_others() {
        let text = [
            "a".to_owned(),
            format!("b {}", mark("//", "one", "")),
            mark("//", "one", ""),
            "c".to_owned(),
            format!("d {}", mark("//", "two", "")),
            end("//", "one", ""),
            format!("e {}", mark("//", "two", "")),
            String::new(),
        ]
        .join("\n");

        let one = strip(&text, &["one"], &["one", "two"]).unwrap().unwrap();
        let two = strip(&text, &["two"], &["one", "two"]).unwrap().unwrap();

        assert_eq!(one, format!("a\ne {}\n", mark("//", "two", "")));
        let kept: Vec<&str> = two.lines().map(|line| &line[..1]).collect();
        assert_eq!(kept, ["a", "b", "/", "c", "/"], "{two}");
        assert_eq!(strip("a\nb\n", &["one"], &["one"]).unwrap(), None);
    }

    #[test]
    fn arch_07_a_removal_keeps_the_other_lines_with_their_numbers() {
        let text = format!(
            "a\n{}\nb\n{}\nc\n",
            mark("//", "one", ""),
            end("//", "one", "")
        );

        assert_eq!(
            kept(&text, &["one"], &["one"]).unwrap(),
            [(1, "a"), (5, "c")]
        );
        assert_eq!(kept(&text, &[], &["one"]).unwrap().len(), 5);
    }

    #[test]
    fn arch_07_a_marker_after_rust_code_fails_and_one_alone_on_its_line_doesn_t() {
        let text = [
            format!("    1, {}", mark("//", "one", "")),
            mark("    //", "one", ""),
            "    2,".to_owned(),
            end("    //", "one", ""),
            format!("{} {}", "# A setting.", mark("#", "one", "")),
        ]
        .join("\n");

        assert_eq!(after_code(&text), [1]);
    }

    #[test]
    fn arch_07_a_marker_of_no_part_or_a_block_that_never_ends_fails() {
        let unknown = mark("#", "three", "");
        assert_eq!(owners(&unknown, &["one"]).unwrap_err().0, 1);
        let unclosed = format!("a\n{}\nb\n", mark("//", "one", ""));
        assert_eq!(owners(&unclosed, &["one"]).unwrap_err().0, 2);
        let crossed = format!("{}\n{}\n", mark("//", "one", ""), end("#", "one", ""));
        assert_eq!(owners(&crossed, &["one"]).unwrap_err().0, 2);
        let stray = format!("a\n{}\n", end("<!--", "one", " -->"));
        assert_eq!(owners(&stray, &["one"]).unwrap_err().0, 2);
    }
}
