//! The metrics `auth` declares (OBS-11).

use platform::telemetry::{metrics::Counter, names};

/// Sign-in attempts, by how each ended.
static SIGN_INS: Counter<1> = Counter::new(
    "auth.sign_ins",
    "{attempt}",
    "Sign-in attempts, by how each ended: succeeded, invalid_credentials or rate_limited \
     (AUTH-004 to AUTH-006).",
    [names::AUTH_OUTCOME],
);

/// How a sign-in ended.
#[derive(Clone, Copy, Debug)]
pub(super) enum SignIn {
    /// A session began (AUTH-004).
    Succeeded,
    /// The credentials matched no account (AUTH-005).
    InvalidCredentials,
    /// The email's limit refused it (AUTH-006).
    RateLimited,
}

/// Counts a sign-in that ended as `outcome`.
pub(super) fn sign_in(outcome: SignIn) {
    let label = match outcome {
        SignIn::Succeeded => "succeeded",
        SignIn::InvalidCredentials => "invalid_credentials",
        SignIn::RateLimited => "rate_limited",
    };
    SIGN_INS.add(1, [label.into()]);
}
