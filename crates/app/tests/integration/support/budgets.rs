//! What the tests of a budget share: how the 429 of a budget reads (PLATFORM-089).

use super::api::Response;

/// Asserts that `response` is the 429 of the budget `name`, whose window of `window` seconds
/// allows `limit` bytes, none of which the window has left.
pub fn assert_refused_by(response: &Response, name: &str, limit: u32, window: u64) {
    assert_eq!(response.status, 429);
    assert_eq!(response.json()["code"], "request.rate_limited");
    assert_eq!(
        response.header("ratelimit-policy"),
        format!("\"{name}\";q={limit};qu=\"content-bytes\";w={window}")
    );
    let state = response.header("ratelimit");
    assert!(state.starts_with(&format!("\"{name}\";r=0;t=")), "{state}");
    let seconds: u64 = response.header("retry-after").parse().unwrap();
    assert!((1..=window).contains(&seconds), "{seconds}");
}
