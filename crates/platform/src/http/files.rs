//! Files over HTTP: the name an upload gives in its `Content-Disposition`, and the response that
//! serves a stored file so that it can't run in a browser (SEC-11).

use axum::{
    body::{Body, Bytes},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::Stream;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};

use crate::errors::{Internal, Problem};

/// A file's bytes, as an upload sends them and a download serves them, which the document
/// describes as a binary string, not as JSON (API-17).
#[derive(utoipa::ToSchema)]
#[schema(value_type = String, format = Binary)]
pub struct FileBytes(pub Vec<u8>);

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

/// A stored file's bytes, as a response that a browser can't run: `Content-Disposition` is
/// `attachment` unless the file's kind may be shown in place, and gives the name twice, as RFC
/// 6266 recommends; `Content-Security-Policy` sandboxes the file and lets it load nothing, and
/// the security headers add `X-Content-Type-Options: nosniff` and `Cache-Control: no-store`
/// (SEC-11, SEC-12).
pub struct Download<S> {
    /// The file's media type, as stored.
    pub media_type: String,
    /// Its size, in bytes.
    pub length: u64,
    /// Whether a browser may show it in place, rather than save it: only for images (SEC-11).
    pub inline: bool,
    /// The name it is saved under.
    pub name: String,
    /// Its bytes, as they arrive from storage.
    pub bytes: S,
}

/// The policy that sandboxes a served file and lets it load nothing (SEC-11).
const SANDBOX: &str = "default-src 'none'; sandbox";

impl<S, E> IntoResponse for Download<S>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: Into<axum::BoxError>,
{
    fn into_response(self) -> Response {
        let Ok(media_type) = HeaderValue::from_str(&self.media_type) else {
            return Problem(Internal::new("a stored media type isn't a header's value"))
                .into_response();
        };
        let disposition = disposition(self.inline, &self.name);
        let headers: [(HeaderName, HeaderValue); 4] = [
            (header::CONTENT_TYPE, media_type),
            (header::CONTENT_LENGTH, HeaderValue::from(self.length)),
            (header::CONTENT_DISPOSITION, disposition),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(SANDBOX),
            ),
        ];
        (StatusCode::OK, headers, Body::from_stream(self.bytes)).into_response()
    }
}

/// `Content-Disposition` for a file named `name`: its type, then the name as `filename`, an ASCII
/// fallback, and as `filename*`, in UTF-8 percent-encoded as RFC 8187 says (RFC 6266).
fn disposition(inline: bool, name: &str) -> HeaderValue {
    let kind = if inline { "inline" } else { "attachment" };
    let value = format!(
        "{kind}; filename=\"{}\"; filename*=UTF-8''{}",
        fallback(name),
        percent_encoded(name)
    );
    // Both forms of the name hold only visible ASCII, so the value is always a header's.
    HeaderValue::from_str(&value).unwrap_or_else(|_| HeaderValue::from_static("attachment"))
}

/// `name` in visible ASCII for the `filename` parameter: every other character, the quote, the
/// backslash and the percent sign, which some browsers would decode, become `_`.
fn fallback(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            ' '..='~' if !matches!(c, '"' | '\\' | '%') => c,
            _ => '_',
        })
        .collect()
}

/// The bytes that RFC 8187 percent-encodes: every byte but its attribute characters.
const NOT_ATTRIBUTE_CHARACTER: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'!')
    .remove(b'#')
    .remove(b'$')
    .remove(b'&')
    .remove(b'+')
    .remove(b'-')
    .remove(b'.')
    .remove(b'^')
    .remove(b'_')
    .remove(b'`')
    .remove(b'|')
    .remove(b'~');

/// `name`'s UTF-8 bytes with every byte but the attribute characters of RFC 8187
/// percent-encoded.
fn percent_encoded(name: &str) -> String {
    utf8_percent_encode(name, NOT_ATTRIBUTE_CHARACTER).to_string()
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

    #[test]
    fn sec_11_a_name_is_served_in_ascii_and_in_utf_8() {
        let value = disposition(false, "Ré\"port 100%.pdf");
        assert_eq!(
            value.to_str().unwrap(),
            "attachment; filename=\"R__port 100_.pdf\"; filename*=UTF-8''R%C3%A9%22port%20100%25.pdf"
        );
        assert!(
            disposition(true, "a.png")
                .to_str()
                .unwrap()
                .starts_with("inline; ")
        );
        // A name that names a header or a parameter stays inside the parameters.
        let value = disposition(false, "a.txt\r\nSet-Cookie: x; filename=evil");
        assert!(!value.to_str().unwrap().contains('\n'));
        assert_eq!(
            name(value.to_str().unwrap()).unwrap(),
            "a.txt\r\nSet-Cookie: x; filename=evil"
        );
    }
}
