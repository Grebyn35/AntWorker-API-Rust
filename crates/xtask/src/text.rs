//! Finding ID-shaped tokens, phrases, dates and comments in lines of text.

/// An ID-shaped token: capitals, a hyphen and digits, such as `AUTH-001`. Whether a token is an
/// ID depends on its prefix, because `SHA-256` has the same shape.
#[derive(Debug, PartialEq, Eq)]
pub struct Token<'a> {
    /// The capitals before the hyphen.
    pub prefix: &'a str,
    /// The digits after the hyphen.
    pub number: &'a str,
    /// Where the token starts in its line, in bytes.
    pub column: usize,
}

impl Token<'_> {
    /// The token as written, such as `AUTH-001`.
    pub fn id(&self) -> String {
        format!("{}-{}", self.prefix, self.number)
    }
}

/// Every ID-shaped token in `line` that stands as a word of its own.
pub fn id_tokens(line: &str) -> Vec<Token<'_>> {
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        let at_word_start = start == 0 || !is_word(bytes[start - 1]);
        if !(at_word_start && bytes[start].is_ascii_uppercase()) {
            start += 1;
            continue;
        }
        let hyphen = scan(bytes, start, |b| b.is_ascii_uppercase() || b == b'_');
        let end = scan(bytes, hyphen + 1, |b| b.is_ascii_digit());
        let shaped = bytes.get(hyphen) == Some(&b'-')
            && bytes[hyphen - 1] != b'_'
            && end > hyphen + 1
            && bytes.get(end).is_none_or(|&b| !is_word(b));
        if shaped {
            tokens.push(Token {
                prefix: &line[start..hyphen],
                number: &line[hyphen + 1..end],
                column: start,
            });
            start = end;
        } else {
            start = hyphen.max(start + 1);
        }
    }
    tokens
}

/// Whether `text` holds `phrase` as whole words, ignoring the case of ASCII letters.
pub fn has_phrase(text: &str, phrase: &str) -> bool {
    has_whole(&text.to_ascii_lowercase(), &phrase.to_ascii_lowercase())
}

/// Whether `text` holds `word` as a whole word, in exactly this case.
pub fn has_word(text: &str, word: &str) -> bool {
    has_whole(text, word)
}

/// Whether `text` holds a date in the form `YYYY-MM-DD`.
pub fn has_date(text: &str) -> bool {
    let bytes = text.as_bytes();
    (0..bytes.len().saturating_sub(9)).any(|start| {
        let shaped = bytes[start..start + 10]
            .iter()
            .enumerate()
            .all(|(i, b)| match i {
                4 | 7 => *b == b'-',
                _ => b.is_ascii_digit(),
            });
        shaped
            && (start == 0 || !is_word(bytes[start - 1]))
            && bytes.get(start + 10).is_none_or(|&b| !is_word(b))
    })
}

/// The comment in `line`, a line of the file at `path`, for the file types whose comments the
/// checks read. The first comment marker starts the comment, even inside a string literal.
pub fn comment<'a>(path: &str, line: &'a str) -> Option<&'a str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let marker = if name.ends_with(".rs") {
        "//"
    } else if name.ends_with(".sql") {
        "--"
    } else if [".toml", ".yml", ".yaml"]
        .iter()
        .any(|extension| name.ends_with(extension))
        || matches!(
            name,
            "justfile" | ".env.example" | ".gitignore" | ".gitattributes" | "Dockerfile"
        )
    {
        "#"
    } else {
        return None;
    };
    line.find(marker).map(|start| &line[start..])
}

/// Whether `needle` occurs in `text` with no letter, digit or underscore on either side.
fn has_whole(text: &str, needle: &str) -> bool {
    let bytes = text.as_bytes();
    text.match_indices(needle).any(|(start, found)| {
        (start == 0 || !is_word(bytes[start - 1]))
            && bytes.get(start + found.len()).is_none_or(|&b| !is_word(b))
    })
}

/// The index of the first byte from `from` on that `accept` refuses, or the length of `bytes`.
fn scan(bytes: &[u8], from: usize, accept: impl Fn(u8) -> bool) -> usize {
    bytes
        .iter()
        .skip(from)
        .position(|&b| !accept(b))
        .map_or(bytes.len(), |offset| from + offset)
}

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_13_id_shaped_tokens_stand_as_words() {
        let line = "See WIDGETS-001, STYLE-21 and SHA-256, not xWIDGETS-002, WIDGETS-003x or W_-4.";
        let ids: Vec<String> = id_tokens(line).iter().map(Token::id).collect();
        assert_eq!(ids, ["WIDGETS-001", "STYLE-21", "SHA-256"]);
        assert_eq!(id_tokens("GADGETS_TWO-001")[0].prefix, "GADGETS_TWO");
    }

    #[test]
    fn doc_01_phrases_match_whole_words_in_any_case() {
        assert!(has_phrase("It Was Previously done.", "previously"));
        assert!(has_phrase("a key that is no longer valid", "no longer"));
        assert!(!has_phrase("unpreviously", "previously"));
        assert!(has_word("a FIX me", "FIX"));
        assert!(!has_word("FIXES", "FIX"));
    }

    #[test]
    fn doc_01_dates_are_found_as_whole_words() {
        assert!(has_date("since 2026-10-22."));
        assert!(has_date("2026-10-22"));
        assert!(!has_date("20261022"));
        assert!(!has_date("12026-10-22"));
        assert!(!has_date("v2026-10-221"));
    }

    #[test]
    fn doc_01_comments_are_found_by_file_type() {
        assert_eq!(comment("src/a.rs", "let a = 1; // why"), Some("// why"));
        assert_eq!(comment("m/1.sql", "SELECT 1; -- why"), Some("-- why"));
        assert_eq!(comment("Cargo.toml", "a = 1 # why"), Some("# why"));
        assert_eq!(comment("justfile", "# why"), Some("# why"));
        assert_eq!(comment("docs/a.md", "# Heading"), None);
        assert_eq!(comment("src/a.rs", "let a = 1;"), None);
    }
}
