//! Finding ID-shaped tokens, phrases, dates and comments in lines of text.

use crate::rust;

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

/// The lines of the file at `path` that cite what they name, by number: every line, except in a
/// check's own tests, the test module of a file under `crates/xtask/`, as [`rust::test_module`]
/// finds it, whose documents and code are made up for the check to read, with IDs of modules that
/// no repository need have (DOC-12). Every line of a file that `syn` can't parse cites, as
/// `check-layout` refuses one under a crate's `src/`.
pub fn citing_lines(path: &str, text: &str) -> usize {
    let lines = text.lines().count();
    if path.starts_with("crates/xtask/") && path.ends_with(".rs") {
        rust::test_module(text).map_or(lines, |tests| tests.before)
    } else {
        lines
    }
}

/// The line of the first occurrence of `needle` in `text`, or 1.
pub fn line_of(text: &str, needle: &str) -> usize {
    text.find(needle)
        .map_or(1, |at| text[..at].matches('\n').count() + 1)
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
/// checks read.
///
/// In a Rust or a JavaScript file, the comment starts at the first `//` of the line that no
/// string literal of the line holds. A literal is one in double quotes, in which a backslash
/// escapes the character after it; in Rust also a character literal, a `'` that one character,
/// or a backslash and what it escapes, and then a `'` follow, such as `'"'`, while a `'` that
/// isn't one starts a lifetime or a label, and a raw string, `r"…"`, `r#"…"#` and so on, whose
/// `r` starts a word, or follows a `b` or a `c` that does; and in JavaScript also one in single
/// quotes or in backticks. Each line is read on its own, from outside any literal, so the rest
/// of a literal that an earlier line opens is read as code: a `"` on it opens a literal, and a
/// `//` before that starts the comment. A block comment, `/* … */`, and a regular expression of
/// JavaScript are read as code too.
///
/// In a file of another type, the first comment marker of the line starts the comment, even
/// inside a string literal.
pub fn comment<'a>(path: &str, line: &'a str) -> Option<&'a str> {
    let name = path.rsplit('/').next().unwrap_or(path);
    if name.ends_with(".rs") || name.ends_with(".js") {
        return code_comment(line, name.ends_with(".rs")).map(|start| &line[start..]);
    }
    let marker = if name.ends_with(".sql") {
        "--"
    } else if [
        ".toml",
        ".yml",
        ".yaml",
        ".just",
        ".sh",
        ".caddy",
        ".env.example",
    ]
    .iter()
    .any(|extension| name.ends_with(extension))
        || matches!(
            name,
            "justfile" | ".gitignore" | ".gitattributes" | "Dockerfile" | "Caddyfile"
        )
        // The Postgres image's scripts, which take no extension, as commands on its PATH.
        || path.starts_with("deploy/postgres/bin/")
    {
        "#"
    } else {
        return None;
    };
    line.find(marker).map(|start| &line[start..])
}

/// Where the comment of `line`, a line of Rust when `rust` holds and of JavaScript otherwise,
/// starts, as [`comment`] reads it.
fn code_comment(line: &str, rust: bool) -> Option<usize> {
    let bytes = line.as_bytes();
    let mut at = 0;
    while let Some(&byte) = bytes.get(at) {
        at = match byte {
            b'/' if bytes.get(at + 1) == Some(&b'/') => return Some(at),
            b'"' => quoted_end(bytes, at + 1, b'"'),
            b'\'' | b'`' if !rust => quoted_end(bytes, at + 1, byte),
            b'\'' => character_end(line, at),
            b'r' if rust => raw_end(bytes, at).unwrap_or(at + 1),
            _ => at + 1,
        };
    }
    None
}

/// The index after the `quote` that ends a literal whose text starts at `from`, or the length of
/// `bytes` when none does.
fn quoted_end(bytes: &[u8], from: usize, quote: u8) -> usize {
    let mut at = from;
    while let Some(&byte) = bytes.get(at) {
        match byte {
            b'\\' => at += 2,
            _ if byte == quote => return at + 1,
            _ => at += 1,
        }
    }
    bytes.len()
}

/// The index after the Rust character literal that starts with the `'` at `at` in `line`, or
/// after that `'` alone when it starts a lifetime or a label.
fn character_end(line: &str, at: usize) -> usize {
    let bytes = line.as_bytes();
    if bytes.get(at + 1) == Some(&b'\\') {
        // An escape, such as `'\''` or `'\u{1F600}'`, which the next `'` after the escaped
        // character ends.
        return bytes
            .iter()
            .skip(at + 3)
            .position(|&byte| byte == b'\'')
            .map_or(bytes.len(), |offset| at + 3 + offset + 1);
    }
    let width = line[at + 1..].chars().next().map_or(0, char::len_utf8);
    if width > 0 && bytes.get(at + 1 + width) == Some(&b'\'') {
        at + 2 + width
    } else {
        at + 1
    }
}

/// The index after the Rust raw string that starts with the `r` at `at`, `r`, any number of
/// `#`, then `"`, with a `b` or a `c` before it or neither, or `None` when no raw string starts
/// there. A raw string that the line doesn't end ends at the line's end.
fn raw_end(bytes: &[u8], at: usize) -> Option<usize> {
    let starts_word = |at: usize| at == 0 || !is_word(bytes[at - 1]);
    let prefixed = at > 0 && matches!(bytes[at - 1], b'b' | b'c') && starts_word(at - 1);
    if !(starts_word(at) || prefixed) {
        return None;
    }
    let quote = scan(bytes, at + 1, |byte| byte == b'#');
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }
    let hashes = quote - at - 1;
    let closing: Vec<u8> = std::iter::once(b'"')
        .chain(std::iter::repeat_n(b'#', hashes))
        .collect();
    Some(
        bytes[quote + 1..]
            .windows(closing.len())
            .position(|window| window == closing.as_slice())
            .map_or(bytes.len(), |offset| quote + 1 + offset + closing.len()),
    )
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
    fn doc_12_id_shaped_tokens_stand_as_words() {
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
        assert_eq!(comment("just/check.just", "# why"), Some("# why"));
        assert_eq!(comment("deploy/tasks/apply.sh", "x=1 # why"), Some("# why"));
        assert_eq!(
            comment("deploy/postgres/bin/pgbackrest", "# why"),
            Some("# why")
        );
        assert_eq!(
            comment("deploy/settings.env.example", "# why"),
            Some("# why")
        );
        assert_eq!(
            comment("deploy/caddy/sites/api.caddy", "# why"),
            Some("# why")
        );
        assert_eq!(comment("deploy/caddy/Caddyfile", "# why"), Some("# why"));
        assert_eq!(
            comment("crates/app/tests/load/streams.js", "f(); // why"),
            Some("// why")
        );
        assert_eq!(comment("docs/a.md", "# Heading"), None);
        assert_eq!(comment("src/a.rs", "let a = 1;"), None);
    }

    #[test]
    fn doc_01_a_comment_of_rust_or_javascript_starts_outside_string_literals() {
        let rust = |line| comment("src/a.rs", line);
        for line in [
            r#"let url = "http://example.com/2006-03-01/"; // why"#,
            r#"let s = "a \" // b"; // why"#,
            r#"let c = '"'; let d = "//"; // why"#,
            r#"let c = '\''; let d = '\\'; let e = '\u{2F}'; let f = "//"; // why"#,
            r#"let c = 'é'; let d = "//"; // why"#,
            r#"fn f<'a>(x: &'a str) -> &'static str { "//" } // why"#,
            r##"let r = r"//"; let s = r#"a "//" b"#; // why"##,
            r##"let b = br"//"; let c = cr#"//"#; let r#type = 1; // why"##,
        ] {
            assert_eq!(rust(line), Some("// why"), "{line}");
        }
        assert_eq!(rust(r#"let url = "http://example.com";"#), None);
        assert_eq!(rust(r###"let r = r##"a "# // b"##;"###), None);
        // An `r` inside a word starts no raw string.
        assert_eq!(rust(r###"let s = xr#"a "// b"#;"###), Some(r##"// b"#;"##));
        assert_eq!(rust("/// A doc comment."), Some("/// A doc comment."));
        // A line on its own: the rest of a literal that an earlier line opens is read as code.
        assert_eq!(rust(r#"    2006-03-01/"; // why"#), None);
        assert_eq!(
            rust(r#"    http://example.com/\"#),
            Some(r#"//example.com/\"#)
        );
        let javascript = |line| comment("crates/app/tests/load/streams.js", line);
        assert_eq!(
            javascript("const a = 'http://x', b = `//`, c = \"//\"; // why"),
            Some("// why")
        );
        assert_eq!(javascript("const a = 'it\\'s //';"), None);
    }
}
