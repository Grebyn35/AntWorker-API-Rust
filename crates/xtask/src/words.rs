//! `check-docs`, the bounds on words: no sentence of a document holds more than 100 words, no
//! paragraph and no item of a list more than 300, and no cell of a table more than 200
//! (DOC-15). The migration workspace isn't checked, as DOC-15 says.
//!
//! What the check reads, through the Markdown parser:
//! - A block is a paragraph, a heading, which is held to a paragraph's bound, a cell of a
//!   table, or the text of a list's item that no paragraph holds, up to a list nested in the
//!   item, and again after it. A code block is none.
//! - A block's words are those of its text and of its code spans: a link counts by its text,
//!   without its destination. A word is a run of characters between whitespace that holds a
//!   letter or a digit.
//! - A sentence ends at a `.`, a `?` or a `!` of the block's text that whitespace or the
//!   block's end follows, and at the block's end. A mark inside a code span ends none.
//! - An HTML comment isn't counted. Any other HTML is a finding, since the parser gives the
//!   check none of the words inside it.

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::{Finding, docs, repo::Repo};

/// The most words a sentence holds.
const SENTENCE: usize = 100;
/// The most words a paragraph, or an item of a list, holds.
const PARAGRAPH: usize = 300;
/// The most words a cell of a table holds.
const CELL: usize = 200;

/// Runs the check.
pub fn check(repo: &Repo) -> Vec<Finding> {
    repo.files()
        .filter(|file| file.path.ends_with(".md") && docs::outside_migration(&file.path))
        .flat_map(|file| over(&file.path, &file.text))
        .collect()
}

/// A block's kind, which sets its bound.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Paragraph,
    Item,
    Cell,
}

/// A block being read: its first line, its kind, the words of its sentences that have ended,
/// and those of the sentence that hasn't.
struct Block {
    line: usize,
    kind: Kind,
    sentences: Vec<Sentence>,
    current: Sentence,
    /// Whether the text so far ends with a mark that ends a sentence if whitespace follows.
    marked: bool,
}

/// A sentence: how many words it holds, and its first ones, which a finding quotes.
#[derive(Default)]
struct Sentence {
    words: usize,
    start: Vec<String>,
}

impl Sentence {
    fn add(&mut self, token: &str) {
        if token.chars().any(char::is_alphanumeric) {
            self.words += 1;
            if self.start.len() < 6 {
                self.start.push(token.to_owned());
            }
        }
    }
}

impl Block {
    fn new(line: usize, kind: Kind) -> Self {
        Self {
            line,
            kind,
            sentences: Vec::new(),
            current: Sentence::default(),
            marked: false,
        }
    }

    /// Adds text outside code spans, whose marks end sentences.
    fn text(&mut self, text: &str) {
        let mut token = String::new();
        for character in text.chars() {
            if character.is_whitespace() {
                self.current.add(&token);
                token.clear();
                if self.marked {
                    self.sentences.push(std::mem::take(&mut self.current));
                }
                self.marked = false;
            } else {
                token.push(character);
                self.marked = matches!(character, '.' | '?' | '!');
            }
        }
        self.current.add(&token);
    }

    /// Adds a code span, whose words count, and whose marks end no sentence.
    fn code(&mut self, code: &str) {
        for token in code.split_whitespace() {
            self.current.add(token);
        }
        self.marked = false;
    }

    /// The findings of the block, once its text has ended.
    fn findings(mut self, path: &str) -> Vec<Finding> {
        self.sentences.push(self.current);
        let total: usize = self.sentences.iter().map(|sentence| sentence.words).sum();
        let (name, bound) = match self.kind {
            Kind::Paragraph => ("a paragraph", PARAGRAPH),
            Kind::Item => ("an item of a list", PARAGRAPH),
            Kind::Cell => ("a cell of a table", CELL),
        };
        let mut findings = Vec::new();
        if total > bound {
            findings.push(Finding::new(
                path,
                self.line,
                format!("{name} of {total} words: it holds at most {bound} (DOC-15)"),
            ));
        }
        for sentence in self.sentences {
            if sentence.words > SENTENCE {
                findings.push(Finding::new(
                    path,
                    self.line,
                    format!(
                        "a sentence of {} words, which starts \"{}\": it holds at most \
                         {SENTENCE} (DOC-15)",
                        sentence.words,
                        sentence.start.join(" ")
                    ),
                ));
            }
        }
        findings
    }
}

/// The blocks and sentences of the document `text`, at `path`, that hold more words than
/// their bounds.
fn over(path: &str, text: &str) -> Vec<Finding> {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(index, _)| index + 1))
        .collect();
    let line = |offset: usize| starts.partition_point(|start| *start <= offset);
    let mut findings = Vec::new();
    let mut markup = Vec::new();
    let mut block: Option<Block> = None;
    // The kind of the block that text opens where it stands: a cell's in a table, an item's
    // in an item that holds no paragraph, and a paragraph's anywhere else, as in a heading.
    let mut kinds = vec![Kind::Paragraph];
    let mut in_code_block = false;
    let mut end = |block: &mut Option<Block>| {
        if let Some(block) = block.take() {
            findings.extend(block.findings(path));
        }
    };
    for (event, range) in Parser::new_ext(text, Options::ENABLE_TABLES).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(_)) => {
                end(&mut block);
                in_code_block = true;
            }
            Event::End(TagEnd::CodeBlock) => in_code_block = false,
            Event::Start(Tag::Item) => {
                end(&mut block);
                kinds.push(Kind::Item);
            }
            Event::Start(Tag::TableCell) => {
                end(&mut block);
                kinds.push(Kind::Cell);
            }
            Event::Start(Tag::Paragraph) => {
                end(&mut block);
                kinds.push(Kind::Paragraph);
            }
            Event::End(TagEnd::Item | TagEnd::TableCell | TagEnd::Paragraph) => {
                end(&mut block);
                kinds.pop();
            }
            Event::Start(Tag::List(_) | Tag::Heading { .. }) | Event::End(TagEnd::Heading(_)) => {
                end(&mut block);
            }
            Event::Text(text) if !in_code_block => {
                let kind = kinds.last().copied().unwrap_or(Kind::Paragraph);
                block
                    .get_or_insert_with(|| Block::new(line(range.start), kind))
                    .text(&text);
            }
            Event::Code(code) => {
                let kind = kinds.last().copied().unwrap_or(Kind::Paragraph);
                block
                    .get_or_insert_with(|| Block::new(line(range.start), kind))
                    .code(&code);
            }
            Event::SoftBreak | Event::HardBreak => {
                if let Some(block) = &mut block {
                    block.text(" ");
                }
            }
            Event::Html(html) | Event::InlineHtml(html)
                if !html.trim_start().starts_with("<!--") =>
            {
                markup.push(Finding::new(
                    path,
                    line(range.start),
                    format!(
                        "`{}` is HTML, whose words the check of a document's words can't \
                         count: write it as Markdown (DOC-15)",
                        html.trim()
                    ),
                ));
            }
            _ => {}
        }
    }
    end(&mut block);
    findings.extend(markup);
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `count` words, with a space between them.
    fn words(count: usize) -> String {
        vec!["word"; count].join(" ")
    }

    fn findings(text: &str) -> Vec<String> {
        over("docs/guide.md", text)
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn doc_15_a_sentence_holds_at_most_100_words() {
        let within = format!("{}. {}? {}!\n", words(100), words(100), words(100));
        let beyond = format!("{}. {}.\n", words(3), words(101));

        assert_eq!(findings(&within), Vec::<String>::new());
        assert_eq!(
            findings(&beyond),
            [
                "docs/guide.md:1: a sentence of 101 words, which starts \"word word word word \
                 word word\": it holds at most 100 (DOC-15)"
            ]
        );
    }

    #[test]
    fn doc_15_a_paragraph_and_an_item_of_a_list_hold_at_most_300_words() {
        let sentence = format!("{}.", words(60));
        let five = [sentence.as_str(); 5].join(" ");
        let six = [sentence.as_str(); 6].join(" ");
        let within = format!("{five}\n\n{five}\n\n- {five}\n- {five}\n");
        let beyond = format!("Lead.\n\n{six}\n\n- {five}\n- {six}\n");

        assert_eq!(findings(&within), Vec::<String>::new());
        assert_eq!(
            findings(&beyond),
            [
                "docs/guide.md:3: a paragraph of 360 words: it holds at most 300 (DOC-15)",
                "docs/guide.md:6: an item of a list of 360 words: it holds at most 300 (DOC-15)",
            ]
        );
    }

    #[test]
    fn doc_15_a_cell_of_a_table_holds_at_most_200_words() {
        let sentence = format!("{}.", words(50));
        let four = [sentence.as_str(); 4].join(" ");
        let five = [sentence.as_str(); 5].join(" ");
        let table = format!("| Name | Says |\n|---|---|\n| one | {four} |\n| two | {five} |\n");

        assert_eq!(
            findings(&table),
            ["docs/guide.md:4: a cell of a table of 250 words: it holds at most 200 (DOC-15)"]
        );
    }

    #[test]
    fn doc_15_a_mark_ends_a_sentence_before_whitespace_and_outside_a_code_span() {
        // A version's point, a path's and a code span's end no sentence, so these are one of
        // 102 words each; a code span's words count, and a mark right after one ends a sentence.
        let version = format!("{} 4.53 and docs/a.md hold {}.\n", words(49), words(49));
        let span = format!("{} `a. b? c!` {}.\n", words(50), words(49));
        let after = format!("{} `code`. {}.\n", words(99), words(100));

        assert_eq!(findings(&version).len(), 1, "{:?}", findings(&version));
        assert_eq!(
            findings(&span),
            [
                "docs/guide.md:1: a sentence of 102 words, which starts \"word word word word \
                 word word\": it holds at most 100 (DOC-15)"
            ]
        );
        assert_eq!(findings(&after), Vec::<String>::new());
    }

    #[test]
    fn doc_15_a_link_counts_by_its_text_and_a_comment_counts_nothing() {
        let link = format!(
            "{} [two words](https://example.invalid/a%20path%20of%20many%20words).\n",
            words(98)
        );
        let comment = format!("{}. <!-- part: files and more words -->\n", words(100));
        let lines = format!("{}\n{}.\n", words(50), words(51));

        assert_eq!(findings(&link), Vec::<String>::new());
        assert_eq!(findings(&comment), Vec::<String>::new());
        // A sentence goes on over the end of a line.
        assert_eq!(findings(&lines).len(), 1, "{:?}", findings(&lines));
    }

    #[test]
    fn doc_15_an_item_s_text_and_a_list_nested_in_it_are_blocks_of_their_own() {
        let sentence = format!("{}.", words(60));
        let four = [sentence.as_str(); 4].join(" ");
        let nested = format!("- {four}\n  - {four}\n  - {four}\n");

        assert_eq!(findings(&nested), Vec::<String>::new());
    }

    #[test]
    fn doc_15_a_code_block_is_not_read_and_a_heading_is() {
        let code = format!("```\n{}\n```\n\n    {}\n", words(400), words(400));
        // The second is a heading too, by the line under it.
        let headings = format!("# {}\n\n{}\n---\n", words(101), words(101));

        assert_eq!(findings(&code), Vec::<String>::new());
        assert_eq!(
            over("docs/guide.md", &headings)
                .iter()
                .map(|finding| finding.line)
                .collect::<Vec<_>>(),
            [1, 3]
        );
    }

    #[test]
    fn doc_15_html_other_than_a_comment_is_refused() {
        let block = format!("<p>{}</p>\n", words(400));
        let inline = "A cell's <br> breaks no line.\n";

        assert_eq!(findings(&block).len(), 1, "{:?}", findings(&block));
        assert_eq!(
            findings(inline),
            [
                "docs/guide.md:1: `<br>` is HTML, whose words the check of a document's words \
                 can't count: write it as Markdown (DOC-15)"
            ]
        );
    }

    #[test]
    fn doc_15_the_migration_workspace_is_not_read() {
        let long = format!("{}.\n", words(101));
        let repo = Repo::of(&[
            ("docs/MIGRATION.md", long.as_str()),
            ("docs/inventory/old.md", long.as_str()),
            ("docs/spec/widgets.md", long.as_str()),
        ]);

        let read: Vec<String> = check(&repo)
            .into_iter()
            .map(|finding| finding.path)
            .collect();

        assert_eq!(read, ["docs/spec/widgets.md"]);
    }
}
