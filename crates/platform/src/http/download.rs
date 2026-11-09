//! The response that serves bytes that never change, such as an export's archive, or a range of
//! them, so that a browser can't run them, and the extractor of what a download's request asks of
//! them (PLATFORM-081).

use axum::{
    body::{Body, Bytes},
    extract::FromRequestParts,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use futures_util::Stream;
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};

use crate::{
    errors::{Internal, Problem},
    ranges::{Served, Wanted, quoted},
};

/// Bytes that a route takes or serves as they are, such as a file's or an archive's, which the
/// document describes as a binary string, not as JSON (API-17).
#[derive(utoipa::ToSchema)]
#[schema(value_type = String, format = Binary)]
pub struct FileBytes(pub Vec<u8>);

/// A download's [`Wanted`]: its method, and its `Range`, `If-Range` and `If-None-Match`
/// (PLATFORM-081).
impl<S: Send + Sync> FromRequestParts<S> for Wanted {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let text = |name: HeaderName| {
            parts
                .headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        Ok(Self::new(
            parts.method == Method::GET,
            text(header::RANGE),
            text(header::IF_RANGE),
            text(header::IF_NONE_MATCH),
        ))
    }
}

/// Bytes that never change, such as a stored file's or an export's archive, or a range of them,
/// as a response that a browser can't run: `Content-Disposition` is `attachment` unless the
/// bytes may be shown in place, and gives the name twice, as RFC 6266 recommends;
/// `Content-Security-Policy` sandboxes the bytes and lets them load nothing; `ETag` names the
/// bytes, and `Accept-Ranges` says that a request may ask for a range of them. The security
/// headers add `X-Content-Type-Options: nosniff`, and `Cache-Control: no-store` unless the
/// download sets its own (SEC-12, PLATFORM-081).
pub struct Download<S> {
    /// The bytes' media type.
    pub media_type: String,
    /// The size of all the bytes.
    pub size: u64,
    /// What the response serves of the bytes.
    pub served: Served,
    /// Whether a browser may show the bytes in place, rather than save them, which their caller
    /// allows of images alone (PLATFORM-081).
    pub inline: bool,
    /// The name it is saved under.
    pub name: String,
    /// What names its bytes, which never change, as its entity tag, unquoted.
    pub tag: String,
    /// How long a shared cache may keep it: `None` for bytes that no cache may keep.
    pub public_for: Option<std::time::Duration>,
    /// The bytes it serves, as they arrive from where they are stored, or `None` when it serves
    /// none.
    pub bytes: Option<S>,
}

/// The policy that sandboxes the served bytes and lets them load nothing (PLATFORM-081).
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
            disposition(self.inline, &self.name),
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

    #[test]
    fn platform_081_a_name_is_served_in_ascii_and_in_utf_8() {
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
        assert_eq!(
            value.to_str().unwrap(),
            "attachment; filename=\"a.txt__Set-Cookie: x; filename=evil\"; \
             filename*=UTF-8''a.txt%0D%0ASet-Cookie%3A%20x%3B%20filename%3Devil"
        );
    }
}
