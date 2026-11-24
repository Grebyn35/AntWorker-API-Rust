//! The instructions of a Dockerfile, as `check-pins` reads them.

use super::numbered;

/// The instructions of the Dockerfile `text`, each with the line it starts on, as BuildKit reads
/// them: a comment line, which starts with `#` after any whitespace, belongs to none, and a line
/// that ends in `\` goes on over the next, past comment lines and empty ones.
pub(super) fn instructions(text: &str) -> Vec<(usize, String)> {
    let mut instructions = Vec::new();
    let mut open: Option<(usize, String)> = None;
    for (number, line) in numbered(text) {
        let line = line.trim();
        if line.starts_with('#') || (open.is_some() && line.is_empty()) {
            continue;
        }
        let (start, mut joined) = open.take().unwrap_or((number, String::new()));
        match line.strip_suffix('\\') {
            Some(part) => {
                joined.push_str(part);
                joined.push(' ');
                open = Some((start, joined));
            }
            None => {
                joined.push_str(line);
                instructions.push((start, joined));
            }
        }
    }
    instructions.extend(open);
    instructions
}

/// Whether the instruction `line` is a `RUN` or an `ADD`, in any case, whose text after its
/// options, the words that start with `--`, is a JSON array: the exec form, which `check-pins`
/// doesn't read.
pub(super) fn is_exec_form(line: &str) -> bool {
    /// The text after the first word of `text`.
    fn after(text: &str) -> &str {
        text.split_once(char::is_whitespace)
            .map_or("", |(_, rest)| rest)
            .trim_start()
    }
    let line = line.trim_start();
    let keyword = line.split_whitespace().next().unwrap_or_default();
    if !keyword.eq_ignore_ascii_case("RUN") && !keyword.eq_ignore_ascii_case("ADD") {
        return false;
    }
    let mut rest = after(line);
    while rest.starts_with("--") {
        rest = after(rest);
    }
    serde_json::from_str::<Vec<serde_json::Value>>(rest).is_ok()
}
