//! The markers that give a line, or a block of lines, to an optional part (ARCH-07). A marker is
//! a comment that reads `part: <part>`, or `end part: <part>`, in one of the syntaxes below.

use std::borrow::Cow;

use syn::{
    Expr, ExprMatch,
    visit::{self, Visit},
};

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

/// How many markers `line` starts, in any of the syntaxes, whatever follows each of them on the
/// line: the places where a comment's start is followed by `part: `, or by `end part: `, and a
/// part's name.
fn started(line: &str) -> usize {
    SYNTAXES
        .into_iter()
        .map(|(opener, _)| {
            line.match_indices(opener)
                .filter(|(at, _)| {
                    let rest = line[at + opener.len()..].trim_start();
                    rest.strip_prefix("end ")
                        .unwrap_or(rest)
                        .strip_prefix("part: ")
                        .is_some_and(|name| name.starts_with(|c: char| c.is_ascii_lowercase()))
                })
                .count()
        })
        .sum()
}

/// The parts that own each line of `text`: those whose block holds it, or whose marker it holds.
/// Fails, with the line counted from 1, on a line that holds a marker and starts another, of
/// which [`marker`] reads one, a marker of a part that `known` doesn't name, an end marker that
/// doesn't stand alone or closes no block of its part, and a block that no end marker closes.
pub fn owners<'a>(text: &'a str, known: &[&str]) -> Result<Vec<Vec<&'a str>>, (usize, String)> {
    let mut open: Vec<(&str, &str, usize)> = Vec::new();
    let mut owners = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        let mut owning: Vec<&str> = open.iter().map(|(part, _, _)| *part).collect();
        if let Some(marker) = marker(line) {
            if started(line) > 1 {
                return Err((
                    number,
                    "the line holds two markers, and a removal reads one: what exists only for \
                     two parts together is a line of one inside a block of the other"
                        .to_owned(),
                ));
            }
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
/// 1, where `owners` holds the parts that own each line, as [`owners`] gives them.
pub fn kept<'a>(text: &'a str, owners: &[Vec<&str>], removed: &[&str]) -> Vec<(usize, &'a str)> {
    text.lines()
        .zip(owners)
        .enumerate()
        .filter(|(_, (_, parts))| !parts.iter().any(|part| removed.contains(part)))
        .map(|(index, (line, _))| (index + 1, line))
        .collect()
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

/// The lines of Rust `text`, counted from 1, that close the block of a part's arm of a match,
/// right above the marker that closes the part's block, when no arm of the core follows the arm.
/// A removal of the parts whose arms follow it leaves such an arm its match's last, and rustfmt,
/// which the removal runs, moves a comment that follows a last arm's block onto the block's
/// closing line, where the marker stands alone no more. It leaves a comment after an arm that
/// another follows, or that is an expression (ARCH-07). Text that doesn't parse has none: the
/// build refuses it.
pub fn block_arm_that_can_be_last(text: &str) -> Vec<usize> {
    struct Arms<'a> {
        lines: Vec<&'a str>,
        /// Whether each line is a part's: in a part's block, or one of its markers.
        marked: Vec<bool>,
        found: Vec<usize>,
    }

    impl<'ast> Visit<'ast> for Arms<'_> {
        fn visit_expr_match(&mut self, node: &'ast ExprMatch) {
            for (index, arm) in node.arms.iter().enumerate() {
                let Expr::Block(body) = &*arm.body else {
                    continue;
                };
                let closes = body.block.brace_token.span.close().start().line;
                // The line after the one that closes the block, counted from 0.
                let ends_part = self.lines.get(closes).is_some_and(|next| {
                    marker(next).is_some_and(|marker| marker.kind == Kind::Close)
                });
                let core_follows = node.arms[index + 1..].iter().any(|later| {
                    let line = later.fat_arrow_token.spans[0].start().line;
                    !self.marked.get(line - 1).copied().unwrap_or(false)
                });
                if ends_part && !core_follows {
                    self.found.push(closes);
                }
            }
            visit::visit_expr_match(self, node);
        }
    }

    let Ok(file) = syn::parse_file(text) else {
        return Vec::new();
    };
    let mut depth = 0_usize;
    let marked = text
        .lines()
        .map(|line| match marker(line).map(|marker| marker.kind) {
            Some(Kind::Open) => {
                depth += 1;
                true
            }
            Some(Kind::Close) => {
                depth = depth.saturating_sub(1);
                true
            }
            Some(Kind::Line) => true,
            None => depth > 0,
        })
        .collect();
    let mut arms = Arms {
        lines: text.lines().collect(),
        marked,
        found: Vec::new(),
    };
    arms.visit_file(&file);
    arms.found
}

/// `text`, the file at `path`, without the lines that the parts in `removed` own, or `None` if it
/// has none of them. Outside Rust, they take a blank line with them, as [`joined`] says.
pub fn strip(
    path: &str,
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
    let lines = text
        .split_inclusive('\n')
        .zip(owners)
        .map(|(line, parts)| (!parts.iter().any(|part| removed.contains(part))).then_some(line));
    Ok(Some(joined(path, lines)))
}

/// `text`, the file at `path`, without its markers, or `None` if it holds none: a line that holds
/// only a marker goes, and a marker after other text goes from its line, which keeps what stood
/// before and after it, as a table row keeps its last pipe. Outside Rust, the lines that go take a
/// blank line with them, as [`joined`] says. `just keep` takes the markers out once it has removed
/// every part that it doesn't keep.
pub fn unmark(path: &str, text: &str) -> Option<String> {
    let mut changed = false;
    let lines: Vec<Option<Cow<'_, str>>> = text
        .split_inclusive('\n')
        .map(|line| {
            let (body, ending) = line
                .strip_suffix('\n')
                .map_or((line, ""), |body| (body, "\n"));
            let Some(marker) = marker(body) else {
                return Some(Cow::Borrowed(line));
            };
            changed = true;
            if marker.alone {
                return None;
            }
            let mut kept = body[..marker.at].trim_end().to_owned();
            let after = marker.after.trim();
            if !after.is_empty() {
                kept.push(' ');
                kept.push_str(after);
            }
            kept.push_str(ending);
            Some(Cow::Owned(kept))
        })
        .collect();
    changed.then(|| joined(path, lines))
}

/// The lines that `lines` keeps, each `None` that a removal, or `just keep`, deletes, joined, of
/// the file at `path`. Outside Rust, whose blank lines rustfmt sets once the removal has formatted
/// the code, each run of deleted lines that stood between two blank lines, or between a blank
/// line and the file's end, takes one blank line with it: the one after it, or, at the file's
/// end, the last one kept before it. A blank line holds nothing but white space. No other blank
/// line goes here, since a blank line can be part of what a file says, as in a block of text in a
/// Compose file (ARCH-07).
fn joined<S: AsRef<str>>(path: &str, lines: impl IntoIterator<Item = Option<S>>) -> String {
    let rust = path.ends_with(".rs");
    let blank = |line: &str| line.trim().is_empty();
    let mut kept = String::new();
    // Where the last line kept starts in `kept`, if it is blank.
    let mut last_blank: Option<usize> = None;
    // Whether lines were deleted since the last line kept.
    let mut deleted = false;
    for line in lines {
        let Some(line) = line else {
            deleted = true;
            continue;
        };
        let line = line.as_ref();
        if !rust && deleted && last_blank.is_some() && blank(line) {
            deleted = false;
            continue;
        }
        last_blank = blank(line).then_some(kept.len());
        kept.push_str(line);
        deleted = false;
    }
    if let Some(start) = last_blank.filter(|_| !rust && deleted) {
        kept.truncate(start);
    }
    kept
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

        let kept = strip("src/a.rs", &text, &["one"], &["one", "two"]).unwrap();

        assert_eq!(kept, Some(format!("a\ne {}\n", mark("//", "two", ""))));
    }

    #[test]
    fn arch_07_removing_a_part_keeps_the_lines_and_blocks_of_another() {
        let text = two_parts();

        let kept = strip("src/a.rs", &text, &["two"], &["one", "two"])
            .unwrap()
            .unwrap();

        let starts: Vec<&str> = kept.lines().map(|line| &line[..1]).collect();
        assert_eq!(starts, ["a", "b", "/", "c", "/"], "{kept}");
    }

    /// A block of `one` in Markdown that holds `lines`.
    fn block(lines: &str) -> String {
        format!(
            "{}\n{lines}{}\n",
            mark("<!--", "one", " -->"),
            end("<!--", "one", " -->")
        )
    }

    #[test]
    fn arch_07_outside_rust_a_removal_drops_one_of_the_blank_lines_its_lines_stood_between() {
        let text = format!("a\n\n{}\nc\n", block("b\n"));
        let spaces = format!("a\n  \nb {}\n\t\nc\n", mark("<!--", "one", " -->"));
        let strip = |text: &str| strip("docs/a.md", text, &["one"], &["one"]).unwrap();

        assert_eq!(strip(&text).as_deref(), Some("a\n\nc\n"));
        assert_eq!(strip(&spaces).as_deref(), Some("a\n  \nc\n"));
    }

    #[test]
    fn arch_07_outside_rust_a_removal_drops_the_blank_line_its_lines_leave_at_the_file_s_end() {
        let line = format!("a: 1\n\nb: 2 {}\n", mark("#", "one", ""));
        let unended = format!(
            "a: 1\n\n{}\nb: 2\n{}",
            mark("#", "one", ""),
            end("#", "one", "")
        );
        let strip = |text: &str| strip("a.yml", text, &["one"], &["one"]).unwrap();

        assert_eq!(strip(&line).as_deref(), Some("a: 1\n"));
        assert_eq!(strip(&unended).as_deref(), Some("a: 1\n"));
    }

    #[test]
    fn arch_07_a_removal_keeps_every_other_blank_line() {
        let marked = |text: &str| format!("{text} {}", mark("<!--", "one", " -->"));
        let strip = |path: &str, text: &str| strip(path, text, &["one"], &["one"]).unwrap();

        // Two blank lines that stood together before the removal, with no line of the part
        // between them, stay; so does the one a deletion leaves beside a line of text.
        let together = format!("a\n\n\nb\n{}\nc\n", marked("x"));
        assert_eq!(
            strip("docs/a.md", &together).as_deref(),
            Some("a\n\n\nb\nc\n")
        );
        let then_deleted = format!("a\n\n\n{}\nc\n", block("b\n"));
        assert_eq!(
            strip("docs/a.md", &then_deleted).as_deref(),
            Some("a\n\n\nc\n")
        );
        let beside_text = format!("a\n\n{}\nc\n", marked("b"));
        assert_eq!(
            strip("docs/a.md", &beside_text).as_deref(),
            Some("a\n\nc\n")
        );
        // Each run of deleted lines takes one blank line, and only one.
        let runs = format!(
            "a\n\n{}\n\n{}\n\n{}\nc\n",
            marked("x"),
            marked("y"),
            marked("z")
        );
        assert_eq!(strip("docs/a.md", &runs).as_deref(), Some("a\n\nc\n"));
        let at_end = format!("a\n\n{}\n\n{}\n", marked("x"), marked("y"));
        assert_eq!(strip("docs/a.md", &at_end).as_deref(), Some("a\n"));
        // The file's start is no end of it.
        let at_start = format!("{}\n\nc\n", marked("b"));
        assert_eq!(strip("docs/a.md", &at_start).as_deref(), Some("\nc\n"));
        // rustfmt sets Rust's blank lines once the removal has formatted the code.
        let rust = format!(
            "fn a() {{}}\n\n{}\nfn b() {{}}\n{}\n\nfn c() {{}}\n",
            mark("//", "one", ""),
            end("//", "one", "")
        );
        assert_eq!(
            strip("src/a.rs", &rust).as_deref(),
            Some("fn a() {}\n\n\nfn c() {}\n")
        );
    }

    #[test]
    fn arch_07_keeping_the_parts_drops_a_blank_line_that_only_a_marker_stood_between() {
        let text = format!("a\n\n{}\nc\n", block("b\n\n"));

        assert_eq!(unmark("docs/a.md", &text).unwrap(), "a\n\nb\n\nc\n");
        let rust = text.replace("<!--", "//").replace(" -->", "");
        assert_eq!(unmark("src/a.rs", &rust).unwrap(), "a\n\nb\n\n\nc\n");
    }

    #[test]
    fn arch_07_a_file_that_holds_none_of_a_part_s_lines_is_left_as_it_is() {
        let text = "a\nb\n";

        let kept = strip("src/a.rs", text, &["one"], &["one"]).unwrap();

        assert_eq!(kept, None);
    }

    #[test]
    fn arch_07_a_removal_keeps_the_other_lines_with_their_numbers() {
        let text = format!(
            "a\n{}\nb\n{}\nc\n",
            mark("//", "one", ""),
            end("//", "one", "")
        );

        let lines = kept(&text, &owners(&text, &["one"]).unwrap(), &["one"]);

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
    fn arch_07_a_part_s_arm_with_a_block_that_no_arm_of_the_core_follows_is_found() {
        let arms = |after: &[String]| {
            let mut lines = vec![
                "fn f(x: u8) -> u8 {".to_owned(),
                "    match x {".to_owned(),
                "        0 => 1,".to_owned(),
                mark("//", "one", ""),
                "        1 => {".to_owned(),
                "            2".to_owned(),
                "        }".to_owned(),
                end("//", "one", ""),
            ];
            lines.extend_from_slice(after);
            lines.extend(["    }".to_owned(), "}".to_owned()]);
            lines.join("\n")
        };
        let last = arms(&[]);
        let before_a_part_s = arms(&[
            mark("//", "two", ""),
            "        2 => 3,".to_owned(),
            end("//", "two", ""),
        ]);
        let before_the_core_s = arms(&["        _ => 0,".to_owned()]);
        let expression = last.replace(
            "        1 => {\n            2\n        }",
            "        1 => 2,",
        );

        // Once the part `two` is removed, the arm of `one` is its match's last too.
        assert_eq!(block_arm_that_can_be_last(&last), [7]);
        assert_eq!(block_arm_that_can_be_last(&before_a_part_s), [7]);
        assert_eq!(
            block_arm_that_can_be_last(&before_the_core_s),
            Vec::<usize>::new()
        );
        assert_eq!(block_arm_that_can_be_last(&expression), Vec::<usize>::new());
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

    #[test]
    fn arch_07_a_line_that_holds_two_markers_fails() {
        let known = ["one", "two"];
        let row = format!(
            "| a |\n| b {} {} |\n",
            mark("<!--", "one", " -->"),
            mark("<!--", "two", " -->")
        );
        let setting = format!("A=1 {} {}\n", mark("#", "one", ""), mark("#", "two", ""));
        let closed = format!("{} {}\n", mark("//", "one", ""), end("//", "one", ""));
        for text in [row.as_str(), setting.as_str(), closed.as_str()] {
            let (line, message) = owners(text, &known).unwrap_err();
            assert_eq!(line, text.lines().count(), "{text}");
            assert!(
                message.starts_with("the line holds two markers"),
                "{message}"
            );
        }
        // One marker on a line, and two on two lines, are read as they were, and so is a line
        // that holds no marker, such as a string of a test with the text of two.
        let one = format!(
            "| b {} |\n- c {}\n",
            mark("<!--", "one", " -->"),
            mark("#", "two", "")
        );
        assert_eq!(owners(&one, &known).unwrap(), [["one"], ["two"]]);
        let string = format!(
            "\"{}\\nmod a;\\n{}\\n\"\n",
            mark("//", "one", ""),
            end("//", "one", "")
        );
        assert_eq!(owners(&string, &known).unwrap(), [Vec::<&str>::new()]);
    }

    #[test]
    fn arch_07_keeping_the_parts_takes_each_marker_out_and_keeps_its_line_s_text() {
        let text = [
            format!(
                "mod a;\n{}\nmod b;\n{}\n",
                mark("//", "one", ""),
                end("//", "one", "")
            ),
            format!("| a | b {} |\n", mark("<!--", "one", " -->")),
            format!("- item {}\n", mark("#", "two", "")),
            "plain\n".to_owned(),
        ]
        .concat();
        assert_eq!(
            unmark("docs/a.md", &text).unwrap(),
            "mod a;\nmod b;\n| a | b |\n- item\nplain\n"
        );
        assert_eq!(unmark("docs/a.md", "plain\nlines"), None);
    }
}
