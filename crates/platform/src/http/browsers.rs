//! What browsers need: cross-origin resource sharing (CORS) for the listed origins of web
//! clients, the cross-site check, and session cookies that no script can read (SEC-07,
//! PLATFORM-058, PLATFORM-059).

use std::sync::Arc;

use axum::{
    extract::{FromRequestParts, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::{
    authn::ACCESS_COOKIE,
    config::Origin,
    errors::{self, Internal, Problem, RequestError},
    types::Timestamp,
};

/// The methods a preflight allows: every method the API serves.
const ALLOWED_METHODS: &str = "GET, HEAD, POST, PUT, PATCH, DELETE";

/// The request headers a preflight allows.
const ALLOWED_HEADERS: &str = concat!(
    "authorization, content-type, idempotency-key, if-match, if-none-match, if-range, \
     last-event-id, range, traceparent, tracestate",
    // part: files
    ", content-disposition",
    // end part: files
    // part: client_telemetry
    ", content-encoding",
    // end part: client_telemetry
);

/// The response headers a browser lets a web client read, beyond the ones it always may.
const EXPOSED_HEADERS: &str = "location, content-location, etag, content-disposition, \
     content-range, ratelimit, ratelimit-policy, retry-after, server-timing";

/// How long a browser may keep a preflight's answer, in seconds.
const PREFLIGHT_MAX_AGE: &str = "600";

/// The prefix of every session cookie, which makes a browser refuse the cookie unless it is
/// `Secure`, has `Path=/`, and names no `Domain` (SEC-07).
const HOST_PREFIX: &str = "__Host-";

/// The origins that `CORS_ALLOWED_ORIGINS` lists.
#[derive(Clone, Debug, Default)]
pub struct Origins(Arc<Vec<HeaderValue>>);

impl Origins {
    /// The origins of `origins`.
    pub fn new(origins: &[Origin]) -> Self {
        let mut values = Vec::new();
        for origin in origins {
            // The settings admit only an origin's ASCII serialization, which is a header's value.
            match HeaderValue::from_str(origin.as_str()) {
                Ok(value) => values.push(value),
                Err(error) => errors::ignore(&error, "origin_not_a_header"),
            }
        }
        Self(Arc::new(values))
    }

    fn lists(&self, origin: &HeaderValue) -> bool {
        self.0.iter().any(|listed| listed == origin)
    }

    /// Whether `headers` carry an `Origin` that the list holds.
    fn admits(&self, headers: &HeaderMap) -> bool {
        headers
            .get(header::ORIGIN)
            .is_some_and(|origin| self.lists(origin))
    }
}

/// Answers a preflight from a listed origin, and lets a browser read every response to one and
/// send its cookies (PLATFORM-058). With no origin listed, it adds nothing.
pub(super) async fn cors(State(origins): State<Origins>, request: Request, next: Next) -> Response {
    if origins.0.is_empty() {
        return next.run(request).await;
    }
    let listed = request
        .headers()
        .get(header::ORIGIN)
        .filter(|origin| origins.lists(origin))
        .cloned();
    if let Some(origin) = &listed
        && request.method() == Method::OPTIONS
        && request
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
    {
        return preflight(origin);
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.append(header::VARY, HeaderValue::from_static("origin"));
    if let Some(origin) = listed {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            HeaderValue::from_static("true"),
        );
        headers.insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static(EXPOSED_HEADERS),
        );
    }
    response
}

fn preflight(origin: &HeaderValue) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static(ALLOWED_METHODS),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static(ALLOWED_HEADERS),
    );
    headers.insert(
        header::ACCESS_CONTROL_MAX_AGE,
        HeaderValue::from_static(PREFLIGHT_MAX_AGE),
    );
    headers.insert(header::VARY, HeaderValue::from_static("origin"));
    response
}

/// Refuses a request that changes something from an origin that isn't listed (PLATFORM-059). A
/// browser sends `Origin` with every such request, and only a browser sends it, so a client that
/// sets its own headers is never refused here.
pub(super) async fn cross_site(
    State(origins): State<Origins>,
    mut request: Request,
    next: Next,
) -> Response {
    if !request.method().is_safe()
        && request.headers().contains_key(header::ORIGIN)
        && !origins.admits(request.headers())
    {
        return Problem(RequestError::OriginNotAllowed).into_response();
    }
    request.extensions_mut().insert(origins);
    next.run(request).await
}

/// The request's cookies. A session cookie authenticates a request that changes something only
/// when the request comes from a listed origin (PLATFORM-059).
pub struct Cookies {
    pairs: Vec<(String, String)>,
    may_authenticate: bool,
}

/// Prints the cookies' names only: their values hold session tokens (OBS-04).
impl std::fmt::Debug for Cookies {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cookies")
            .field(
                "names",
                &self.pairs.iter().map(|(name, _)| name).collect::<Vec<_>>(),
            )
            .field("may_authenticate", &self.may_authenticate)
            .finish()
    }
}

impl Cookies {
    /// The value of the session cookie `__Host-<name>`, or `None` when the request has none. A
    /// request that changes something and carries it without a listed `Origin` is refused.
    pub fn credential(&self, name: &str) -> Result<Option<&str>, RequestError> {
        let full = format!("{HOST_PREFIX}{name}");
        let value = self
            .pairs
            .iter()
            .find(|(key, _)| *key == full)
            .map(|(_, value)| value.as_str());
        if value.is_some() && !self.may_authenticate {
            return Err(RequestError::OriginNotAllowed);
        }
        Ok(value)
    }

    /// Every cookie `__Host-<name>` but `__Host-access_token`, the access token's, by which
    /// `Principal` authenticates a request, each with its name after `__Host-`: a browser's device
    /// tokens among them (AUTH-015). Unlike [`Cookies::credential`], it refuses no request that
    /// carries no listed `Origin` (PLATFORM-059), so it also gives the cookie of another
    /// credential, such as a refresh token, which its route reads through `credential`.
    pub fn all_but_access(&self) -> Vec<(&str, &str)> {
        self.pairs
            .iter()
            .filter_map(|(key, value)| {
                key.strip_prefix(HOST_PREFIX)
                    .filter(|name| *name != ACCESS_COOKIE)
                    .map(|name| (name, value.as_str()))
            })
            .collect()
    }
}

impl Cookies {
    /// The cookies of the request `parts` describe.
    pub(crate) fn of(parts: &Parts) -> Self {
        let origins = parts
            .extensions
            .get::<Origins>()
            .cloned()
            .unwrap_or_default();
        Self {
            pairs: pairs(&parts.headers),
            may_authenticate: parts.method.is_safe() || origins.admits(&parts.headers),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Cookies {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::of(parts))
    }
}

/// The name and value of each cookie of every `Cookie` line, in their order (PLATFORM-092). A line
/// that holds a byte outside ASCII counts too, read as UTF-8, with the replacement character for
/// what isn't: a value that holds such a byte is so no token, since the API issues only tokens of
/// ASCII.
fn pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .flat_map(|value| value.as_bytes().split(|byte| *byte == b';'))
        .filter_map(|pair| {
            let mut parts = pair.trim_ascii().splitn(2, |byte| *byte == b'=');
            Some((parts.next()?, parts.next()?))
        })
        .map(|(key, value)| {
            (
                String::from_utf8_lossy(key).into_owned(),
                String::from_utf8_lossy(value).into_owned(),
            )
        })
        .collect()
}

/// The `Set-Cookie` value of the session cookie `__Host-<name>`, holding `value` until
/// `expires`, when its token expires: `Secure`, `HttpOnly`, `SameSite=Strict`, `Path=/` and no
/// `Domain` (SEC-07).
pub fn session_cookie(
    name: &str,
    value: &str,
    expires: Timestamp,
) -> Result<HeaderValue, Internal> {
    cookie(name, value, &format!("Expires={}", http_date(expires)))
}

/// The `Set-Cookie` value that removes the session cookie `__Host-<name>` from the browser.
pub fn expired_cookie(name: &str) -> Result<HeaderValue, Internal> {
    cookie(name, "", "Max-Age=0")
}

/// The cookie, or an internal error when its name or value can't be a header's, which a token
/// never causes.
fn cookie(name: &str, value: &str, lifetime: &str) -> Result<HeaderValue, Internal> {
    let text = format!(
        "{HOST_PREFIX}{name}={value}; {lifetime}; Path=/; Secure; HttpOnly; SameSite=Strict"
    );
    HeaderValue::from_str(&text).map_err(Internal::new)
}

/// `at` as HTTP writes a date, such as `Sun, 01 Nov 2026 02:26:40 GMT` (RFC 9110).
fn http_date(at: Timestamp) -> String {
    let at = at.at().to_offset(time::UtcOffset::UTC);
    let weekday = match at.weekday() {
        time::Weekday::Monday => "Mon",
        time::Weekday::Tuesday => "Tue",
        time::Weekday::Wednesday => "Wed",
        time::Weekday::Thursday => "Thu",
        time::Weekday::Friday => "Fri",
        time::Weekday::Saturday => "Sat",
        time::Weekday::Sunday => "Sun",
    };
    let month = match at.month() {
        time::Month::January => "Jan",
        time::Month::February => "Feb",
        time::Month::March => "Mar",
        time::Month::April => "Apr",
        time::Month::May => "May",
        time::Month::June => "Jun",
        time::Month::July => "Jul",
        time::Month::August => "Aug",
        time::Month::September => "Sep",
        time::Month::October => "Oct",
        time::Month::November => "Nov",
        time::Month::December => "Dec",
    };
    format!(
        "{weekday}, {:02} {month} {:04} {:02}:{:02}:{:02} GMT",
        at.day(),
        at.year(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sec_07_a_session_cookie_is_secure_http_only_strict_and_host_only() {
        let expires = Timestamp::from_database(
            time::OffsetDateTime::from_unix_timestamp(1_793_500_000).unwrap(),
        );
        assert_eq!(
            session_cookie("access_token", "abc", expires).unwrap(),
            "__Host-access_token=abc; Expires=Sun, 01 Nov 2026 02:26:40 GMT; Path=/; Secure; HttpOnly; SameSite=Strict"
        );
        assert_eq!(
            expired_cookie("refresh_token").unwrap(),
            "__Host-refresh_token=; Max-Age=0; Path=/; Secure; HttpOnly; SameSite=Strict"
        );
    }

    #[test]
    fn platform_092_cookies_on_several_lines_are_one_list_read_by_a_name_s_first_value() {
        let mut headers = HeaderMap::new();
        for line in [
            "__Host-access_token=one",
            "__Host-refresh_token=two; __Host-access_token=three",
        ] {
            headers.append(header::COOKIE, HeaderValue::from_static(line));
        }
        let cookies = Cookies {
            pairs: pairs(&headers),
            may_authenticate: true,
        };
        assert_eq!(cookies.credential("access_token").unwrap(), Some("one"));
        assert_eq!(cookies.credential("refresh_token").unwrap(), Some("two"));
    }

    #[test]
    fn platform_092_a_cookie_line_that_holds_a_byte_outside_ascii_still_counts() {
        let mut headers = HeaderMap::new();
        for line in [
            &b"__Host-access_token=caf\xc3\xa9; __Host-refresh_token=\xff"[..],
            b"__Host-access_token=one; __Host-refresh_token=two",
        ] {
            headers.append(header::COOKIE, HeaderValue::from_bytes(line).unwrap());
        }
        let cookies = Cookies {
            pairs: pairs(&headers),
            may_authenticate: true,
        };
        assert_eq!(cookies.credential("access_token").unwrap(), Some("café"));
        assert_eq!(
            cookies.credential("refresh_token").unwrap(),
            Some("\u{FFFD}")
        );
    }

    #[test]
    fn platform_059_the_cookies_but_the_access_token_s_never_hold_it() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::COOKIE,
            HeaderValue::from_static(
                "__Host-access_token=one; __Host-device_token_ab=two; __Host-access_token=three",
            ),
        );
        let cookies = Cookies {
            pairs: pairs(&headers),
            may_authenticate: false,
        };

        let others = cookies.all_but_access();

        assert_eq!(others, [("device_token_ab", "two")]);
    }
}
