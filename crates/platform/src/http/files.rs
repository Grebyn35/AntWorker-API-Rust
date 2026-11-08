//! Files over HTTP: the name an upload gives in its `Content-Disposition`, and the response that
//! serves a stored file, or a range of its bytes, so that it can't run in a browser (SEC-11,
//! PLATFORM-081).

use axum::{
    body::{Body, Bytes},
    extract::FromRequestParts,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header, request::Parts},
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

/// What a download's request asks of the file's bytes: its method, and its `Range`, `If-Range`
/// and `If-None-Match`, which [`DownloadRequest::serve`] reads once the file's size and entity
/// tag are known (PLATFORM-081).
#[derive(Clone, Debug, Default)]
pub struct DownloadRequest {
    get: bool,
    range: Option<String>,
    if_range: Option<String>,
    if_none_match: Option<String>,
}

impl<S: Send + Sync> FromRequestParts<S> for DownloadRequest {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let text = |name: HeaderName| {
            parts
                .headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        Ok(Self {
            get: parts.method == Method::GET,
            range: text(header::RANGE),
            if_range: text(header::IF_RANGE),
            if_none_match: text(header::IF_NONE_MATCH),
        })
    }
}

/// The bytes of a file, from its first to its last, both included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    /// The offset of its first byte.
    pub first: u64,
    /// The offset of its last byte.
    pub last: u64,
}

impl ByteRange {
    /// The offsets it covers, as a read of the bucket takes them.
    pub fn offsets(self) -> std::ops::Range<u64> {
        self.first..self.last + 1
    }

    /// How many bytes it covers.
    fn len(self) -> u64 {
        self.last - self.first + 1
    }
}

/// What a download serves of a file's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Served {
    /// Nothing: the client holds the bytes its `If-None-Match` names (304).
    NotModified,
    /// Every byte (200).
    Whole,
    /// One range of them (206).
    Range(ByteRange),
}

impl Served {
    /// The range that a read of the bucket asks for: `None` for every byte.
    pub fn offsets(self) -> Option<std::ops::Range<u64>> {
        match self {
            Self::Range(range) => Some(range.offsets()),
            Self::NotModified | Self::Whole => None,
        }
    }
}

impl DownloadRequest {
    /// What to serve of a file of `size` bytes whose entity tag is `tag`, as RFC 9110 orders
    /// the conditions: an `If-None-Match` that is `*`, or names `tag`, weakly or strongly, serves
    /// nothing; then a `Range` of `GET` that holds one range starting within the file serves it,
    /// unless an `If-Range` names something other than `tag`, strongly; and every other request
    /// serves every byte, a `Range` that can't be served being ignored, as RFC 9110 allows.
    pub fn serve(&self, size: u64, tag: &str) -> Served {
        if self
            .if_none_match
            .as_deref()
            .is_some_and(|value| names_tag(value, tag))
        {
            return Served::NotModified;
        }
        if !self.get {
            return Served::Whole;
        }
        let Some(range) = self
            .range
            .as_deref()
            .and_then(|value| one_range(value, size))
        else {
            return Served::Whole;
        };
        match self.if_range.as_deref() {
            Some(value) if value.trim() != quoted(tag) => Served::Whole,
            _ => Served::Range(range),
        }
    }
}

/// `tag` as an entity tag: quoted, and strong.
fn quoted(tag: &str) -> String {
    format!("\"{tag}\"")
}

/// Whether `value`, an `If-None-Match`, is `*` or names `tag` among its entity tags, by the weak
/// comparison it takes.
fn names_tag(value: &str, tag: &str) -> bool {
    let value = value.trim();
    value == "*"
        || value.split(',').any(|named| {
            let named = named.trim();
            named.strip_prefix("W/").unwrap_or(named) == quoted(tag)
        })
}

/// The one range of bytes that `value`, a `Range`, asks for of a file of `size` bytes:
/// `bytes=first-last`, its last clamped to the file's, `bytes=first-`, or `bytes=-suffix`, the
/// file's last `suffix` bytes, or all of them for a longer suffix. `None` for a value of several
/// ranges, of another unit, that isn't one, or whose range starts past the file's last byte.
fn one_range(value: &str, size: u64) -> Option<ByteRange> {
    let (unit, set) = value.trim().split_once('=')?;
    if !unit.trim().eq_ignore_ascii_case("bytes") || set.contains(',') || size == 0 {
        return None;
    }
    let (first, last) = set.trim().split_once('-')?;
    let number = |text: &str| -> Option<u64> {
        let text = text.trim();
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    };
    let end = size - 1;
    if first.trim().is_empty() {
        let suffix = number(last).filter(|suffix| *suffix > 0)?;
        return Some(ByteRange {
            first: size.saturating_sub(suffix),
            last: end,
        });
    }
    let first = number(first).filter(|first| *first <= end)?;
    let last = if last.trim().is_empty() {
        end
    } else {
        number(last).filter(|last| *last >= first)?.min(end)
    };
    Some(ByteRange { first, last })
}

/// A stored file's bytes, or a range of them, as a response that a browser can't run:
/// `Content-Disposition` is `attachment` unless the file's kind may be shown in place, and gives
/// the name twice, as RFC 6266 recommends, or none, for a public file; `Content-Security-Policy`
/// sandboxes the file and lets it load nothing; `ETag` names its bytes, and `Accept-Ranges` says
/// that a request may ask for a range of them. The security headers add
/// `X-Content-Type-Options: nosniff`, and `Cache-Control: no-store` unless the download sets its
/// own (SEC-11, SEC-12, PLATFORM-081).
pub struct Download<S> {
    /// The file's media type, as stored.
    pub media_type: String,
    /// The size of the whole file, in bytes.
    pub size: u64,
    /// What the response serves of the bytes.
    pub served: Served,
    /// Whether a browser may show it in place, rather than save it: only for images (SEC-11).
    pub inline: bool,
    /// The name it is saved under, or `None` for a file whose name the response withholds, as a
    /// public file's (SEC-11).
    pub name: Option<String>,
    /// What names its bytes, which never change, as its entity tag, unquoted.
    pub tag: String,
    /// How long a shared cache may keep it: `None` for a file that no cache may keep.
    pub public_for: Option<std::time::Duration>,
    /// The bytes it serves, as they arrive from storage, or `None` when it serves none.
    pub bytes: Option<S>,
}

/// The policy that sandboxes a served file and lets it load nothing (SEC-11).
const SANDBOX: &str = "default-src 'none'; sandbox";

impl<S, E> IntoResponse for Download<S>
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: Into<axum::BoxError>,
{
    fn into_response(self) -> Response {
        let Ok(tag) = HeaderValue::from_str(&quoted(&self.tag)) else {
            return Problem(Internal::new("an entity tag isn't a header's value")).into_response();
        };
        let mut headers = HeaderMap::new();
        headers.insert(header::ETAG, tag);
        if let Some(duration) = self.public_for {
            let value = format!("public, max-age={}", duration.as_secs());
            if let Ok(value) = HeaderValue::from_str(&value) {
                headers.insert(header::CACHE_CONTROL, value);
            }
        }
        let (status, length) = match self.served {
            Served::NotModified => return (StatusCode::NOT_MODIFIED, headers).into_response(),
            Served::Whole => (StatusCode::OK, self.size),
            Served::Range(range) => {
                let value = format!("bytes {}-{}/{}", range.first, range.last, self.size);
                if let Ok(value) = HeaderValue::from_str(&value) {
                    headers.insert(header::CONTENT_RANGE, value);
                }
                (StatusCode::PARTIAL_CONTENT, range.len())
            }
        };
        let Ok(media_type) = HeaderValue::from_str(&self.media_type) else {
            return Problem(Internal::new("a stored media type isn't a header's value"))
                .into_response();
        };
        let Some(bytes) = self.bytes else {
            return Problem(Internal::new("a download that serves bytes holds none"))
                .into_response();
        };
        headers.insert(header::CONTENT_TYPE, media_type);
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
        headers.insert(
            header::CONTENT_DISPOSITION,
            disposition(self.inline, self.name.as_deref()),
        );
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(SANDBOX),
        );
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        (status, headers, Body::from_stream(bytes)).into_response()
    }
}

/// `Content-Disposition` for a file named `name`: its type, then the name as `filename`, an ASCII
/// fallback, and as `filename*`, in UTF-8 percent-encoded as RFC 8187 says (RFC 6266), or its
/// type alone for a file whose name is withheld.
fn disposition(inline: bool, name: Option<&str>) -> HeaderValue {
    let kind = if inline { "inline" } else { "attachment" };
    let Some(name) = name else {
        return HeaderValue::from_static(kind);
    };
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

    /// A request of `method` with the headers `headers`, as a download reads it.
    fn request(method: Method, headers: &[(HeaderName, &str)]) -> DownloadRequest {
        let text = |name: &HeaderName| {
            headers
                .iter()
                .find(|(named, _)| named == name)
                .map(|(_, value)| (*value).to_owned())
        };
        DownloadRequest {
            get: method == Method::GET,
            range: text(&header::RANGE),
            if_range: text(&header::IF_RANGE),
            if_none_match: text(&header::IF_NONE_MATCH),
        }
    }

    fn range(value: &str) -> Served {
        request(Method::GET, &[(header::RANGE, value)]).serve(1_000, "tag")
    }

    fn bytes(first: u64, last: u64) -> Served {
        Served::Range(ByteRange { first, last })
    }

    #[test]
    fn platform_081_one_range_of_bytes_is_served_in_each_of_its_forms() {
        assert_eq!(range("bytes=0-499"), bytes(0, 499));
        assert_eq!(range("bytes=500-"), bytes(500, 999));
        assert_eq!(range("bytes=-200"), bytes(800, 999));
        assert_eq!(range(" Bytes = 10 - 20 "), bytes(10, 20));
        // A last byte past the file's ends at its last, and a suffix longer than it is the file.
        assert_eq!(range("bytes=990-5000"), bytes(990, 999));
        assert_eq!(range("bytes=-5000"), bytes(0, 999));
        assert_eq!(range("bytes=999-999"), bytes(999, 999));
    }

    #[test]
    fn platform_081_a_range_that_can_t_be_served_serves_every_byte() {
        for value in [
            "bytes=1000-",
            "bytes=2000-3000",
            "bytes=-0",
            "bytes=20-10",
            "bytes=0-10, 20-30",
            "items=0-10",
            "bytes=a-b",
            "bytes=+1-2",
            "bytes",
            "bytes=-",
        ] {
            assert_eq!(range(value), Served::Whole, "{value}");
        }
        assert_eq!(
            request(Method::GET, &[(header::RANGE, "bytes=0-0")]).serve(0, "tag"),
            Served::Whole
        );
    }

    #[test]
    fn platform_081_if_range_serves_the_range_only_for_the_same_strong_tag() {
        let with = |if_range: &str| {
            request(
                Method::GET,
                &[(header::RANGE, "bytes=10-"), (header::IF_RANGE, if_range)],
            )
            .serve(1_000, "tag")
        };
        assert_eq!(with("\"tag\""), bytes(10, 999));
        assert_eq!(with("\"other\""), Served::Whole);
        assert_eq!(with("W/\"tag\""), Served::Whole);
        assert_eq!(with("Sat, 07 Nov 2026 10:00:00 GMT"), Served::Whole);
    }

    #[test]
    fn platform_081_if_none_match_of_the_tag_serves_nothing_and_head_ignores_range() {
        let none_match = |value: &str| {
            request(
                Method::GET,
                &[(header::IF_NONE_MATCH, value), (header::RANGE, "bytes=0-1")],
            )
            .serve(1_000, "tag")
        };
        assert_eq!(none_match("\"tag\""), Served::NotModified);
        assert_eq!(none_match("W/\"tag\""), Served::NotModified);
        assert_eq!(none_match("\"a\", \"tag\""), Served::NotModified);
        assert_eq!(none_match("*"), Served::NotModified);
        assert_eq!(none_match("\"other\""), bytes(0, 1));
        assert_eq!(
            request(Method::HEAD, &[(header::RANGE, "bytes=0-1")]).serve(1_000, "tag"),
            Served::Whole
        );
    }

    #[test]
    fn platform_081_a_public_file_s_disposition_holds_no_name() {
        assert_eq!(disposition(true, None).to_str().unwrap(), "inline");
        assert_eq!(disposition(false, None).to_str().unwrap(), "attachment");
    }

    #[test]
    fn sec_11_a_name_is_served_in_ascii_and_in_utf_8() {
        let value = disposition(false, Some("Ré\"port 100%.pdf"));
        assert_eq!(
            value.to_str().unwrap(),
            "attachment; filename=\"R__port 100_.pdf\"; filename*=UTF-8''R%C3%A9%22port%20100%25.pdf"
        );
        assert!(
            disposition(true, Some("a.png"))
                .to_str()
                .unwrap()
                .starts_with("inline; ")
        );
        // A name that names a header or a parameter stays inside the parameters.
        let value = disposition(false, Some("a.txt\r\nSet-Cookie: x; filename=evil"));
        assert!(!value.to_str().unwrap().contains('\n'));
        assert_eq!(
            name(value.to_str().unwrap()).unwrap(),
            "a.txt\r\nSet-Cookie: x; filename=evil"
        );
    }
}
