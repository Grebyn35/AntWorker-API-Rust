//! What the checks read from Markdown: heading anchors, links and prose, through a Markdown
//! parser; and, line by line, level-2 sections, the lines under a heading, numbered lists of
//! named items, and tables.

use std::collections::{BTreeMap, BTreeSet};

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

/// A link, with the line it starts on.
#[derive(Debug, PartialEq, Eq)]
pub struct Link {
    /// Where the link points, as written.
    pub destination: String,
    /// The line, counted from 1.
    pub line: usize,
}

/// The parts of a Markdown document the checks read.
#[derive(Debug, Default)]
pub struct Document {
    /// The anchor of every heading, as GitHub derives it from the heading's text.
    pub anchors: BTreeSet<String>,
    /// Every link.
    pub links: Vec<Link>,
    /// The prose, one entry per block, with the line the block starts on. Prose leaves out
    /// code spans and code blocks.
    pub prose: Vec<(usize, String)>,
}

/// Reads `text` as Markdown.
pub fn read(text: &str) -> Document {
    let lines = LineIndex::new(text);
    let mut reader = Reader::default();
    for (event, range) in Parser::new_ext(text, Options::ENABLE_TABLES).into_offset_iter() {
        reader.event(event, lines.line(range.start));
    }
    reader.finish()
}

#[derive(Default)]
struct Reader {
    document: Document,
    anchor_uses: BTreeMap<String, usize>,
    heading: Option<String>,
    in_code_block: bool,
    block: Option<(usize, String)>,
}

impl Reader {
    fn event(&mut self, event: Event<'_>, line: usize) {
        match event {
            Event::Start(Tag::Heading { .. }) => self.heading = Some(String::new()),
            Event::End(TagEnd::Heading(_)) => {
                if let Some(heading) = self.heading.take() {
                    self.add_anchor(&heading);
                }
                self.end_block();
            }
            Event::Start(Tag::CodeBlock(_)) => self.in_code_block = true,
            Event::End(TagEnd::CodeBlock) => self.in_code_block = false,
            Event::Start(Tag::Link { dest_url, .. }) => self.document.links.push(Link {
                destination: dest_url.into_string(),
                line,
            }),
            Event::Text(text) if !self.in_code_block => {
                if let Some(heading) = &mut self.heading {
                    heading.push_str(&text);
                }
                self.add_prose(line, &text);
            }
            Event::Code(code) => {
                if let Some(heading) = &mut self.heading {
                    heading.push_str(&code);
                }
                self.add_prose(line, " ");
            }
            Event::SoftBreak | Event::HardBreak => self.add_prose(line, " "),
            Event::End(TagEnd::Paragraph | TagEnd::TableCell | TagEnd::Item) => self.end_block(),
            _ => {}
        }
    }

    fn add_prose(&mut self, line: usize, text: &str) {
        self.block
            .get_or_insert_with(|| (line, String::new()))
            .1
            .push_str(text);
    }

    fn end_block(&mut self) {
        if let Some(block) = self.block.take() {
            self.document.prose.push(block);
        }
    }

    /// Adds the heading's anchor. GitHub numbers repeated anchors: `notes`, `notes-1`.
    fn add_anchor(&mut self, heading: &str) {
        let slug = slug(heading);
        let uses = self.anchor_uses.entry(slug.clone()).or_default();
        let anchor = if *uses == 0 {
            slug
        } else {
            format!("{slug}-{uses}")
        };
        *uses += 1;
        self.document.anchors.insert(anchor);
    }

    fn finish(mut self) -> Document {
        self.end_block();
        self.document
    }
}

/// The anchor GitHub gives a heading: lowercase, without punctuation, with a hyphen for each
/// space.
fn slug(heading: &str) -> String {
    heading
        .trim()
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | ' '))
        .map(|c| if c == ' ' { '-' } else { c })
        .collect()
}

/// A table: a block of lines whose first character other than whitespace is `|`.
#[derive(Debug, PartialEq, Eq)]
pub struct Table {
    /// The level-2 heading the table sits under, if any.
    pub section: Option<String>,
    /// The line of the header row, counted from 1.
    pub line: usize,
    /// The header's cells.
    pub header: Vec<String>,
    /// The line of the delimiter row under the header, such as `|---|`, and its number of
    /// cells, if the table has one.
    pub delimiter: Option<(usize, usize)>,
    /// The rows after the delimiter row, each with its line.
    pub rows: Vec<(usize, Vec<String>)>,
}

/// The level-2 headings of `text`, outside code blocks, with their lines.
pub fn sections(text: &str) -> Vec<(usize, &str)> {
    outside_code(text)
        .filter_map(|(line, text)| Some((line, text.strip_prefix("## ")?.trim())))
        .collect()
}

/// The line of the first heading that starts with `heading`, and the lines under it, up to
/// the next heading of any level.
pub fn under<'a>(text: &'a str, heading: &str) -> Option<(usize, Vec<(usize, &'a str)>)> {
    let mut lines = outside_code(text).skip_while(|(_, line)| !line.starts_with(heading));
    let (start, _) = lines.next()?;
    Some((
        start,
        lines
            .take_while(|(_, line)| !line.starts_with('#'))
            .collect(),
    ))
}

/// The items of the numbered list under `heading` whose text starts with a bold name, such as
/// `1. **Purpose.** Why the module exists.`: each name, with the rest of its item.
pub fn named_items<'a>(text: &'a str, heading: &str) -> Vec<(&'a str, &'a str)> {
    let Some((_, lines)) = under(text, heading) else {
        return Vec::new();
    };
    lines
        .into_iter()
        .filter_map(|(_, line)| {
            let item = line
                .strip_prefix(|c: char| c.is_ascii_digit())?
                .trim_start_matches(|c: char| c.is_ascii_digit())
                .strip_prefix(". **")?;
            let (name, rest) = item.split_once(".**")?;
            Some((name, rest.trim()))
        })
        .collect()
}

/// The tables in `text`, outside code blocks.
pub fn tables(text: &str) -> Vec<Table> {
    let mut tables = Vec::new();
    let mut section = None;
    let mut current: Option<Table> = None;
    for (line, row) in outside_code(text) {
        if let Some(heading) = row.strip_prefix("## ") {
            section = Some(heading.trim().to_owned());
        }
        if !row.trim_start().starts_with('|') {
            tables.extend(current.take());
            continue;
        }
        let cells = cells(row);
        match &mut current {
            None => {
                current = Some(Table {
                    section: section.clone(),
                    line,
                    header: cells,
                    delimiter: None,
                    rows: Vec::new(),
                });
            }
            Some(table)
                if table.delimiter.is_none() && table.rows.is_empty() && is_separator(&cells) =>
            {
                table.delimiter = Some((line, cells.len()));
            }
            Some(table) => table.rows.push((line, cells)),
        }
    }
    tables.extend(current);
    tables
}

/// The cells of a table row, trimmed (DOC-15): one pipe at each end of the row bounds it, and
/// a pipe that follows a backslash belongs to its cell.
fn cells(row: &str) -> Vec<String> {
    let row = row.trim();
    let row = row.strip_prefix('|').unwrap_or(row);
    let row = match row.strip_suffix('|') {
        Some(inner) if !inner.ends_with('\\') => inner,
        _ => row,
    };
    let mut cells = Vec::new();
    let mut cell = String::new();
    for c in row.chars() {
        if c == '|' && !cell.ends_with('\\') {
            cells.push(std::mem::take(&mut cell).trim().to_owned());
        } else {
            cell.push(c);
        }
    }
    cells.push(cell.trim().to_owned());
    cells
}

/// Whether `cells` are the row that separates a table's header from its rows, such as `|---|`.
fn is_separator(cells: &[String]) -> bool {
    cells
        .iter()
        .all(|cell| !cell.is_empty() && cell.chars().all(|c| matches!(c, '-' | ':')))
}

/// The lines of `text` outside fenced code blocks, each with its number, counted from 1.
pub fn outside_code(text: &str) -> impl Iterator<Item = (usize, &str)> {
    let mut in_code = false;
    text.lines().enumerate().filter_map(move |(index, line)| {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            return None;
        }
        (!in_code).then_some((index + 1, line))
    })
}

/// The byte offsets where the lines of a text start.
struct LineIndex {
    starts: Vec<usize>,
}

impl LineIndex {
    fn new(text: &str) -> Self {
        let starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(offset, _)| offset + 1))
            .collect();
        Self { starts }
    }

    /// The line, counted from 1, that holds the byte at `offset`.
    fn line(&self, offset: usize) -> usize {
        self.starts.partition_point(|&start| start <= offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_13_anchors_follow_github() {
        let document = read(
            "# Title\n\n## 4. Crates\n\n### STYLE-01 `widgets` never fail\n\n## Notes\n\n## Notes\n",
        );
        let anchors: Vec<&str> = document.anchors.iter().map(String::as_str).collect();
        assert_eq!(
            anchors,
            [
                "4-crates",
                "notes",
                "notes-1",
                "style-01-widgets-never-fail",
                "title"
            ]
        );
    }

    #[test]
    fn doc_13_links_keep_their_line() {
        let document = read(
            "Intro.\n\nSee [the spec](spec/widgets.md#purpose) and\n[a site](https://example.com).\n",
        );
        let links: Vec<(&str, usize)> = document
            .links
            .iter()
            .map(|link| (link.destination.as_str(), link.line))
            .collect();
        assert_eq!(
            links,
            [("spec/widgets.md#purpose", 3), ("https://example.com", 4)]
        );
    }

    #[test]
    fn doc_15_sections_lists_and_tables_are_read_line_by_line() {
        let text = "# Doc\n\n## Sections\n\n1. **Purpose.** Why.\n10. **Notes.** Only some.\n\
                    - **Not.** An item.\n\n```\n## Code\n| not | a table |\n```\n\n\
                    ## Rows\n\n| A | B |\n|---|:-:|\n| 1 | `x` |\n\nAfter.\n";

        assert_eq!(sections(text), [(3, "Sections"), (14, "Rows")]);
        assert_eq!(
            named_items(text, "## Sections"),
            [("Purpose", "Why."), ("Notes", "Only some.")]
        );
        assert_eq!(
            tables(text),
            [Table {
                section: Some("Rows".to_owned()),
                line: 16,
                header: vec!["A".to_owned(), "B".to_owned()],
                delimiter: Some((17, 2)),
                rows: vec![(18, vec!["1".to_owned(), "`x`".to_owned()])],
            }]
        );
        assert_eq!(under(text, "## Nowhere"), None);
    }

    #[test]
    fn doc_01_prose_leaves_out_code() {
        let document = read(
            "Plain words with `a span` in a\nparagraph.\n\n```\na block\n```\n\n| Cell |\n|---|\n| text |\n",
        );
        let prose: Vec<(usize, &str)> = document
            .prose
            .iter()
            .map(|(line, text)| (*line, text.as_str()))
            .collect();
        assert_eq!(
            prose,
            [
                (1, "Plain words with   in a paragraph."),
                (8, "Cell"),
                (10, "text")
            ]
        );
    }
}
