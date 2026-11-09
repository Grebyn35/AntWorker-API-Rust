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

/// Bytes that a route serves, or takes, as they are, such as an archive's, which the document
/// describes as a binary string, not as JSON (API-17).
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

/// Bytes that never change, such as an export's archive, or a range of them, as a response that
/// a browser can't run: `Content-Disposition` is `attachment`, and gives the name twice, as RFC
/// 6266 recommends; `Content-Security-Policy` sandboxes the bytes and lets them load nothing;
/// `ETag` names the bytes, and `Accept-Ranges` says that a request may ask for a range of them.
/// The security headers add `X-Content-Type-Options: nosniff` and `Cache-Control: no-store`
/// (SEC-12, PLATFORM-081).
pub struct Download<S> {
    /// The bytes' media type.
    pub media_type: String,
    /// The size of all the bytes.
    pub size: u64,
    /// What the response serves of the bytes.
    pub served: Served,
    // part: files
    /// Whether a browser may show the bytes in place, as `inline`, rather than save them, which
    /// `files` allows of images alone (SEC-11).
    pub inline: bool,
    // end part: files
    /// The name it is saved under.
    pub name: String,
    /// What names its bytes, which never change, as its entity tag, unquoted.
    pub tag: String,
    // part: files
    /// How long a shared cache may keep it, in place of `no-store`: `None` for bytes that no
    /// cache may keep, as every download but a public file's (FILES-023).
    pub public_for: Option<std::time::Duration>,
    // end part: files
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
        // part: files
        if let Some(duration) = self.public_for {
            let value = format!("public, max-age={}", duration.as_secs());
            if let Ok(value) = HeaderValue::from_str(&value) {
                headers.insert(header::CACHE_CONTROL, value);
            }
        }
        // end part: files
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
        let kind = "attachment";
        // part: files
        let kind = if self.inline { "inline" } else { kind };
        // end part: files
        headers.insert(header::CONTENT_DISPOSITION, disposition(kind, &self.name));
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(SANDBOX),
        );
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        (status, headers, Body::from_stream(bytes)).into_response()
    }
}

/// `Content-Disposition` of the type `kind` for bytes named `name`: the type, then the name as
/// `filename`, an ASCII fallback, and as `filename*`, in UTF-8 percent-encoded as RFC 8187 says
/// (RFC 6266).
fn disposition(kind: &str, name: &str) -> HeaderValue {
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
        let value = disposition("attachment", "Ré\"port 100%.pdf");
        assert_eq!(
            value.to_str().unwrap(),
            "attachment; filename=\"R__port 100_.pdf\"; filename*=UTF-8''R%C3%A9%22port%20100%25.pdf"
        );
        // A name that names a header or a parameter stays inside the parameters.
        let value = disposition("attachment", "a.txt\r\nSet-Cookie: x; filename=evil");
        assert_eq!(
            value.to_str().unwrap(),
            "attachment; filename=\"a.txt__Set-Cookie: x; filename=evil\"; \
             filename*=UTF-8''a.txt%0D%0ASet-Cookie%3A%20x%3B%20filename%3Devil"
        );
    }
}
