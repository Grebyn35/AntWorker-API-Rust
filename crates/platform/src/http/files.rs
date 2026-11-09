//! Files over HTTP: the name an upload gives in its `Content-Disposition` (PLATFORM-075).

use axum::http::{HeaderMap, header};
use percent_encoding::percent_decode_str;

/// The name in an upload's `Content-Disposition`: its `filename*` parameter, whose value RFC 8187
/// encodes in UTF-8, or else its `filename` parameter, a token or a quoted string, whatever the
/// disposition's type. `None` when the header is absent, isn't text, or names no file
/// (PLATFORM-075).
pub fn disposition_name(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(header::CONTENT_DISPOSITION)?.to_str().ok()?;
    let mut plain = None;
    for parameter in parameters(value).into_iter().skip(1) {
        let Some((name, value)) = parameter.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("filename*") {
            if let Some(decoded) = extended(value) {
                return Some(decoded);
            }
        } else if name.eq_ignore_ascii_case("filename") {
            plain = Some(unquoted(value));
        }
    }
    plain
}

/// The parameters of a header's value, split at each `;` outside a quoted string, the first being
/// the disposition's type.
fn parameters(value: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut quoted, mut escaped) = (0, false, false);
    for (at, c) in value.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => {
                parts.push(&value[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&value[start..]);
    parts
}

/// A token, or a quoted string with its quotes removed and each quoted pair read as the character
/// it quotes.
fn unquoted(value: &str) -> String {
    let Some(inner) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return value.to_owned();
    };
    let mut text = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        text.push(if c == '\\' {
            chars.next().unwrap_or(c)
        } else {
            c
        });
    }
    text
}

/// An extended value of RFC 8187, `UTF-8'<language>'<percent-encoded bytes>`, decoded, or `None`
/// when it names another character set or its bytes aren't UTF-8.
fn extended(value: &str) -> Option<String> {
    let mut parts = value.splitn(3, '\'');
    let charset = parts.next()?;
    let _language = parts.next()?;
    let encoded = parts.next()?;
    if !charset.eq_ignore_ascii_case("utf-8") {
        return None;
    }
    percent_decode_str(encoded)
        .decode_utf8()
        .ok()
        .map(std::borrow::Cow::into_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The name in a `Content-Disposition` of `value`.
    fn name(value: &str) -> Option<String> {
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_DISPOSITION, value.parse().unwrap());
        disposition_name(&headers)
    }

    #[test]
    fn platform_075_a_name_comes_from_filename_star_or_else_filename() {
        assert_eq!(name("attachment; filename=\"a b.txt\"").unwrap(), "a b.txt");
        assert_eq!(name("attachment; filename=plain.txt").unwrap(), "plain.txt");
        assert_eq!(
            name("attachment; filename=\"q\\\"uote;d.txt\"").unwrap(),
            "q\"uote;d.txt"
        );
        assert_eq!(
            name("attachment; filename*=UTF-8''caf%C3%A9.txt; filename=\"cafe.txt\"").unwrap(),
            "café.txt"
        );
        assert_eq!(
            name("attachment; filename=\"cafe.txt\"; FILENAME*=utf-8'fr'caf%C3%A9.txt").unwrap(),
            "café.txt"
        );
        // A character set other than UTF-8, or bytes that aren't, fall back to `filename`.
        assert_eq!(
            name("attachment; filename*=ISO-8859-1''caf%E9.txt; filename=\"cafe.txt\"").unwrap(),
            "cafe.txt"
        );
        assert_eq!(
            name("attachment; filename*=UTF-8''caf%E9.txt; filename=\"cafe.txt\"").unwrap(),
            "cafe.txt"
        );
        assert!(name("attachment").is_none());
        assert!(name("attachment; name=\"field\"").is_none());
        assert!(disposition_name(&HeaderMap::new()).is_none());
    }
}
