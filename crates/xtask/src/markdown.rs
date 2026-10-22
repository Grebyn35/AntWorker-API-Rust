//! What the checks read from Markdown: heading anchors, links and prose.

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
