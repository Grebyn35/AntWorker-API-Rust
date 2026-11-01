//! What browsers need: cross-origin resource sharing (CORS) for the listed origins of web
//! clients, the cross-site check, and session cookies that no script can read (SEC-19,
//! PLATFORM-052, PLATFORM-053).

use std::sync::Arc;

use axum::{
    extract::{FromRequestParts, Request, State},
    http::{HeaderMap, HeaderValue, Method, StatusCode, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::{
    config::Origin,
    errors::{self, Internal, Problem, RequestError},
    types::Timestamp,
};

/// The methods a preflight allows: every method the API serves.
const ALLOWED_METHODS: &str = "GET, HEAD, POST, PUT, PATCH, DELETE";

/// The request headers a preflight allows.
const ALLOWED_HEADERS: &str =
    "authorization, content-type, idempotency-key, if-match, last-event-id, traceparent";

/// The response headers a browser lets a web client read, beyond the ones it always may.
const EXPOSED_HEADERS: &str =
    "location, etag, ratelimit, ratelimit-policy, retry-after, server-timing";

/// How long a browser may keep a preflight's answer, in seconds.
const PREFLIGHT_MAX_AGE: &str = "600";

/// The prefix of every session cookie, which makes a browser refuse the cookie unless it is
/// `Secure`, has `Path=/`, and names no `Domain` (SEC-19).
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
/// send its cookies (PLATFORM-052). With no origin listed, it adds nothing.
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

/// Refuses a request that changes something from an origin that isn't listed (PLATFORM-053). A
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
/// when the request comes from a listed origin (PLATFORM-053).
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

fn pairs(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

/// The `Set-Cookie` value of the session cookie `__Host-<name>`, holding `value` until
/// `expires`, when its token expires: `Secure`, `HttpOnly`, `SameSite=Strict`, `Path=/` and no
/// `Domain` (SEC-19).
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
    fn sec_19_a_session_cookie_is_secure_http_only_strict_and_host_only() {
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
}
