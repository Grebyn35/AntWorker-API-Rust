//! The response that serves bytes that never change, such as an export's archive, or a range of
//! them, so that a browser can't run them, and the extractor of what a download's request asks of
//! them (PLATFORM-081).

use axum::{
    body::{Body, Bytes},
    extract::FromRequestParts,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use futures_util::{Stream, StreamExt};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use tracing_opentelemetry::OpenTelemetrySpanExt;

use crate::{
    errors::{self, Internal, Problem},
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
        // A condition that holds a byte outside ASCII is read too, as UTF-8, with the replacement
        // character for what isn't, so a member that holds such a byte names no entity tag the
        // API sent, and a `Range` that holds one is none that a download can serve
        // (PLATFORM-092).
        let condition = |name: HeaderName| {
            parts
                .headers
                .get(name)
                .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned())
        };
        let range = parts
            .headers
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        Ok(Self::new(
            parts.method == Method::GET,
            range,
            condition(header::IF_RANGE),
            condition(header::IF_NONE_MATCH),
        ))
    }
}

/// Bytes that never change, such as an export's archive, or a range of them, as a response that
/// a browser can't run: `Content-Disposition` is `attachment`, and gives the name twice, as RFC
/// 6266 recommends; `Content-Security-Policy` sandboxes the bytes and lets them load nothing;
/// `ETag` names the bytes, and `Accept-Ranges` says that a request may ask for a range of them.
/// The security headers add `X-Content-Type-Options: nosniff` and `Cache-Control: no-store`
/// (SEC-12, PLATFORM-081).
// part: files
///
/// `Content-Disposition` is `inline` instead where [`Download::inline`] is set, and a public
/// file's `Cache-Control` lets shared caches keep it for [`Download::public_for`], in place of
/// `no-store` (SEC-11, FILES-023).
// end part: files
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
    /// none: a 304, or the answer to a `HEAD`, which carries the headers of the bytes alone.
    pub bytes: Option<S>,
}

/// The policy that sandboxes the served bytes, beside what every response's policy says: they
/// load nothing, set no base address, and no page may frame them (PLATFORM-081, SEC-12).
const SANDBOX: &str = "default-src 'none'; base-uri 'none'; frame-ancestors 'none'; sandbox";

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
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(SANDBOX),
        );
        // part: files
        if let Some(duration) = self.public_for {
            // A number of seconds is always a header's value. Without it, no cache keeps the
            // bytes, since the security headers add `no-store`.
            match HeaderValue::from_str(&format!("public, max-age={}", duration.as_secs())) {
                Ok(value) => {
                    headers.insert(header::CACHE_CONTROL, value);
                }
                Err(error) => errors::ignore(&error, "cache_control_not_set"),
            }
        }
        // end part: files
        let (status, length) = match self.served {
            Served::NotModified => return (StatusCode::NOT_MODIFIED, headers).into_response(),
            Served::Whole => (StatusCode::OK, self.size),
            Served::Range(range) => {
                let value = format!("bytes {}-{}/{}", range.first, range.last, self.size);
                // A 206 that doesn't say which bytes it holds is no answer to its request.
                match HeaderValue::from_str(&value) {
                    Ok(value) => {
                        headers.insert(header::CONTENT_RANGE, value);
                    }
                    Err(error) => return Problem(Internal::new(error)).into_response(),
                }
                (StatusCode::PARTIAL_CONTENT, range.len())
            }
        };
        let Ok(media_type) = HeaderValue::from_str(&self.media_type) else {
            return Problem(Internal::new("a media type isn't a header's value")).into_response();
        };
        headers.insert(header::CONTENT_TYPE, media_type);
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(length));
        let disposition = attachment(&self.name);
        // part: files
        let disposition = if self.inline {
            inline(&self.name)
        } else {
            disposition
        };
        // end part: files
        headers.insert(header::CONTENT_DISPOSITION, disposition);
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        let Some(bytes) = self.bytes else {
            return (status, headers).into_response();
        };
        // A failure of the bytes once the status is sent can become no answer: it ends the body
        // short of its length. It is written and counted as an ignored error, in a span of its
        // own in the request's trace, since the request's span ends with the handler (ERR-11).
        let trace = tracing::Span::current().context();
        let bytes = bytes.map(move |chunk| {
            chunk.map_err(|error| {
                let error: axum::BoxError = error.into();
                let span = tracing::info_span!(parent: None, "download");
                if let Err(unset) = span.set_parent(trace.clone()) {
                    errors::ignore(&unset, "trace_parent_not_set");
                }
                span.in_scope(|| errors::ignore(error.as_ref(), "download_cut"));
                error
            })
        });
        (status, headers, Body::from_stream(bytes)).into_response()
    }
}

/// `Content-Disposition: attachment` for bytes named `name`, with [`named`]'s parameters
/// (RFC 6266).
fn attachment(name: &str) -> HeaderValue {
    to_header(&format!("attachment; {}", named(name)))
}

// part: files
/// `Content-Disposition: inline` for bytes named `name`, with [`named`]'s parameters (RFC 6266).
fn inline(name: &str) -> HeaderValue {
    to_header(&format!("inline; {}", named(name)))
}
// end part: files

/// The parameters of `Content-Disposition` that name bytes `name`: the name as `filename`, an
/// ASCII fallback, and as `filename*`, in UTF-8 percent-encoded as RFC 8187 says (RFC 6266).
fn named(name: &str) -> String {
    format!(
        "filename=\"{}\"; filename*=UTF-8''{}",
        fallback(name),
        percent_encoded(name)
    )
}

/// `value`, a `Content-Disposition`, as a header's value.
fn to_header(value: &str) -> HeaderValue {
    // Both forms of the name hold only visible ASCII, so the value is always a header's.
    match HeaderValue::from_str(value) {
        Ok(value) => value,
        Err(error) => {
            errors::ignore(&error, "disposition_name_not_set");
            HeaderValue::from_static("attachment")
        }
    }
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

    /// What a `GET` with `headers` asks of the 1,000 bytes whose entity tag is `tag`.
    async fn served(headers: &[(HeaderName, &[u8])]) -> Served {
        let mut request = axum::http::Request::builder().method(Method::GET);
        for (name, value) in headers {
            request = request.header(name, HeaderValue::from_bytes(value).unwrap());
        }
        let (mut parts, ()) = request.body(()).unwrap().into_parts();
        let wanted = Wanted::from_request_parts(&mut parts, &()).await.unwrap();
        wanted.serve(1_000, "tag")
    }

    #[tokio::test]
    async fn platform_092_a_download_reads_an_if_range_or_if_none_match_that_holds_a_byte_outside_ascii()
     {
        let range = (header::RANGE, &b"bytes=0-1"[..]);

        let other_tag = served(&[range.clone(), (header::IF_RANGE, b"\"tag\xc3\xa9\"")]).await;
        let spaced_tag = served(&[range.clone(), (header::IF_RANGE, b"\xc2\xa0\"tag\"")]).await;
        let named = served(&[(header::IF_NONE_MATCH, b"\"tag\", \"caf\xc3\xa9\"")]).await;
        let spaced = served(&[(header::IF_NONE_MATCH, b"\xc2\xa0\"tag\"")]).await;
        let no_range = served(&[(header::RANGE, b"bytes=\xc2\xa00-1")]).await;

        assert_eq!(other_tag, Served::Whole);
        assert_eq!(spaced_tag, Served::Whole);
        assert_eq!(named, Served::NotModified);
        assert_eq!(spaced, Served::Whole);
        assert_eq!(no_range, Served::Whole);
    }

    #[test]
    fn platform_081_a_304_carries_the_sandboxed_policy() {
        let download = Download {
            media_type: "application/zip".to_owned(),
            size: 1_000,
            served: Served::NotModified,
            // part: files
            inline: false,
            public_for: None,
            // end part: files
            name: "export.zip".to_owned(),
            tag: "tag".to_owned(),
            bytes: None::<futures_util::stream::Empty<Result<Bytes, std::io::Error>>>,
        };

        let response = download.into_response();

        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(
            response.headers().get(header::CONTENT_SECURITY_POLICY),
            Some(&HeaderValue::from_static(SANDBOX))
        );
        assert_eq!(
            response.headers().get(header::ETAG),
            Some(&HeaderValue::from_static("\"tag\""))
        );
    }

    #[test]
    fn platform_081_a_name_is_served_in_ascii_and_in_utf_8() {
        let value = attachment("Ré\"port 100%.pdf");
        assert_eq!(
            value.to_str().unwrap(),
            "attachment; filename=\"R__port 100_.pdf\"; filename*=UTF-8''R%C3%A9%22port%20100%25.pdf"
        );
        // A name that names a header or a parameter stays inside the parameters.
        let value = attachment("a.txt\r\nSet-Cookie: x; filename=evil");
        assert_eq!(
            value.to_str().unwrap(),
            "attachment; filename=\"a.txt__Set-Cookie: x; filename=evil\"; \
             filename*=UTF-8''a.txt%0D%0ASet-Cookie%3A%20x%3B%20filename%3Devil"
        );
    }
}
