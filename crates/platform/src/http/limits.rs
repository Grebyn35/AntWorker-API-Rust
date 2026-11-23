//! The IP limit that each request that PLATFORM-054 counts passes, before authentication, and
//! the rate-limit headers of every response the limits counted (PLATFORM-054, PLATFORM-056).

use std::{sync::Arc, time::Duration};

use axum::{
    extract::{FromRequestParts, Request, State},
    http::{HeaderValue, StatusCode, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};

use super::stack::DATABASE_WAIT;
use crate::{
    clock::SharedClock,
    errors::{self, Internal, Problem, RequestError},
    rate_limit::{self, Bucket, Count, Counted, RateLimiter, RateLimits},
    types::Client,
};

/// The IP limit's limiter and its bucket.
// part: files
/// A public download counts in a bucket of its own.
// end part: files
pub(super) struct IpLimit {
    pub(super) limiter: RateLimiter,
    /// The clock that bounds the count's wait for the database.
    pub(super) clock: SharedClock,
    /// The bucket of every request that the IP limit counts.
    pub(super) bucket: Bucket,
    // part: files
    /// The bucket of the requests under [`super::PUBLIC_PREFIX`], public downloads of files,
    /// avatars and workspaces' pictures, which count there instead (PLATFORM-082).
    pub(super) public_bucket: Bucket,
    // end part: files
}

/// Counts the request in its client IP's bucket, refuses it with 429 once the bucket is full, and
/// describes on the response the bucket closest to refusing it, of every bucket that counted it
/// (PLATFORM-054). A count that fails, or that the database hasn't answered within
/// [`DATABASE_WAIT`], its wait for a pooled connection included, answers the internal error, and
/// nothing after it runs (OPS-07).
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
    let bucket = limit.bucket;
    // part: files
    let bucket = if request.uri().path().starts_with(super::PUBLIC_PREFIX) {
        limit.public_bucket
    } else {
        bucket
    };
    // end part: files
    let address = rate_limit::address(ip);
    // Once the bound has passed, the count is late, whatever it answers then.
    let count = tokio::select! {
        biased;
        () = limit.clock.sleep(DATABASE_WAIT) => Err(Internal::new(format!(
            "the IP limit's count wasn't answered within {} seconds",
            DATABASE_WAIT.as_secs()
        ))),
        count = limit.limiter.count(bucket, &address) => count,
    };
    let count = match count {
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
    let quota = format!("\"{}\";q={}", count.bucket.name, count.bucket.limit);
    // A budget's quota is in bytes, which the draft's quota unit says (PLATFORM-089).
    let quota = if count.bytes {
        format!("{quota};qu=\"content-bytes\"")
    } else {
        quota
    };
    format!("{quota};w={}", count.bucket.window.as_secs())
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
    fn platform_056_retry_after_rounds_up() {
        assert_eq!(seconds(Duration::from_millis(1)), 1);
        assert_eq!(seconds(Duration::from_secs(41)), 41);
        assert_eq!(seconds(Duration::from_millis(41_200)), 42);
    }
}
