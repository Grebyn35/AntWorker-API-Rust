//! The IP limit every request but the health probes passes, before authentication, and the
//! rate-limit headers of every response the limits counted (PLATFORM-048, PLATFORM-050).

use std::{net::IpAddr, sync::Arc, time::Duration};

use axum::{
    extract::{FromRequestParts, Request, State},
    http::{HeaderValue, StatusCode, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::{
    errors::{self, Internal, Problem, RequestError},
    rate_limit::{Bucket, Count, Counted, RateLimiter, RateLimits},
    types::Client,
};

/// The IP limit's limiter and bucket.
pub(super) struct IpLimit {
    pub(super) limiter: RateLimiter,
    pub(super) bucket: Bucket,
}

/// Counts the request in its client IP's bucket, refuses it with 429 once the bucket is full, and
/// describes on the response the bucket closest to refusing it, of every bucket that counted it.
pub(super) async fn ip_limit(
    State(limit): State<Arc<IpLimit>>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(ip) = request.extensions().get::<Client>().map(|client| client.ip) else {
        return Problem(Internal::new("the request carries no client")).into_response();
    };
    let counted = Counted::default();
    request.extensions_mut().insert(counted.clone());
    request.extensions_mut().insert(limit.limiter.clone());
    let count = match limit.limiter.count(limit.bucket, &key(ip)).await {
        Ok(count) => count,
        Err(error) => return Problem(error).into_response(),
    };
    counted.push(count);
    let mut response = if count.refused() {
        Problem(RequestError::RateLimited).into_response()
    } else {
        next.run(request).await
    };
    describe(&mut response, &counted);
    response
}

/// What an IP bucket counts: an IPv4 address's bytes, with one that arrived mapped into IPv6
/// counted as itself, or an IPv6 address's /64 prefix, which one subscriber holds whole, so a
/// client can't escape its limit by moving within it (PLATFORM-048).
fn key(ip: IpAddr) -> Vec<u8> {
    match ip.to_canonical() {
        IpAddr::V4(address) => address.octets().to_vec(),
        IpAddr::V6(address) => address.octets()[..8].to_vec(),
    }
}

/// Writes the headers of the IETF draft for the bucket closest to refusing the request, and
/// `Retry-After` on a 429 (API-15). A 5xx carries none, as the document says: a request that load
/// shedding refused never reached the limiter.
fn describe(response: &mut Response, counted: &Counted) {
    if response.status().is_server_error() {
        return;
    }
    let Some(tightest) = counted.tightest() else {
        return;
    };
    let headers = response.headers_mut();
    for (name, value) in [
        ("ratelimit-policy", policy(&tightest)),
        ("ratelimit", state(&tightest)),
    ] {
        // A bucket's name and numbers are ASCII, so the value is always a header's.
        match HeaderValue::from_str(&value) {
            Ok(value) => {
                headers.insert(name, value);
            }
            Err(error) => errors::ignore(&error, "rate_limit_header_not_set"),
        }
    }
    if response.status() == StatusCode::TOO_MANY_REQUESTS {
        response.headers_mut().insert(
            header::RETRY_AFTER,
            HeaderValue::from(seconds(tightest.resets_in)),
        );
    }
}

fn policy(count: &Count) -> String {
    format!(
        "\"{}\";q={};w={}",
        count.bucket.name,
        count.bucket.limit,
        count.bucket.window.as_secs()
    )
}

fn state(count: &Count) -> String {
    format!(
        "\"{}\";r={};t={}",
        count.bucket.name,
        count.remaining(),
        seconds(count.resets_in)
    )
}

/// Whole seconds, rounded up, so a client that waits them finds the window ended.
fn seconds(duration: Duration) -> u64 {
    let whole = duration.as_secs();
    if duration.subsec_nanos() > 0 {
        whole.saturating_add(1)
    } else {
        whole
    }
}

impl<S: Send + Sync> FromRequestParts<S> for RateLimits {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let limiter = parts.extensions.get::<RateLimiter>().cloned();
        let counted = parts.extensions.get::<Counted>().cloned();
        match limiter.zip(counted) {
            Some((limiter, counted)) => Ok(Self::new(limiter, counted)),
            None => Err(Problem(Internal::new("the request passed no rate limit")).into_response()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_050_retry_after_rounds_up() {
        assert_eq!(seconds(Duration::from_millis(1)), 1);
        assert_eq!(seconds(Duration::from_secs(41)), 41);
        assert_eq!(seconds(Duration::from_millis(41_200)), 42);
    }

    #[test]
    fn platform_048_an_ipv6_client_counts_by_its_64_prefix() {
        let first: IpAddr = "2001:db8:1:2::1".parse().unwrap();
        let same: IpAddr = "2001:db8:1:2:ffff::9".parse().unwrap();
        let other: IpAddr = "2001:db8:1:3::1".parse().unwrap();
        assert_eq!(key(first), key(same));
        assert_ne!(key(first), key(other));
    }

    #[test]
    fn platform_051_an_ipv4_address_mapped_into_ipv6_counts_as_itself() {
        let mapped: IpAddr = "::ffff:192.0.2.1".parse().unwrap();
        let plain: IpAddr = "192.0.2.1".parse().unwrap();
        assert_eq!(key(mapped), key(plain));
    }
}
