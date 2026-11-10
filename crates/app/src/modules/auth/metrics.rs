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
    /// A sign-in limit refused it: one of the email's, or its device token's (AUTH-006).
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

/// Checks of the codes that prove an account's email address, by how each ended.
static EMAIL_VERIFICATIONS: Counter<1> = Counter::new(
    "auth.email_verifications",
    "{check}",
    "Checks of the codes that prove an account's email address, by how each ended: verified, \
     wrong_code, code_expired or rate_limited (AUTH-031 to AUTH-033).",
    [names::AUTH_OUTCOME],
);

/// How a check of a verification code ended.
#[derive(Clone, Copy, Debug)]
pub(super) enum Verification {
    /// The account proved its address (AUTH-031).
    Verified,
    /// The code wasn't the account's (AUTH-032).
    WrongCode,
    /// The account had no code that lasted (AUTH-032).
    CodeExpired,
    /// The limit on the account's checks refused it (AUTH-033).
    RateLimited,
}

/// Counts a check of a verification code that ended as `outcome`.
pub(super) fn verification(outcome: Verification) {
    let label = match outcome {
        Verification::Verified => "verified",
        Verification::WrongCode => "wrong_code",
        Verification::CodeExpired => "code_expired",
        Verification::RateLimited => "rate_limited",
    };
    EMAIL_VERIFICATIONS.add(1, [label.into()]);
}

/// Confirmations of password resets, by how each ended.
static PASSWORD_RESETS: Counter<1> = Counter::new(
    "auth.password_resets",
    "{confirmation}",
    "Confirmations of password resets, by how each ended: reset, invalid_reset_code or \
     rate_limited (AUTH-037 to AUTH-039).",
    [names::AUTH_OUTCOME],
);

/// How a confirmation of a password reset ended.
#[derive(Clone, Copy, Debug)]
pub(super) enum PasswordReset {
    /// The account's password was reset (AUTH-037).
    Reset,
    /// The code reset no account's password (AUTH-038).
    InvalidResetCode,
    /// The limit on the email's confirmations from the client's address refused it (AUTH-039).
    RateLimited,
}

/// Counts a confirmation of a password reset that ended as `outcome`.
pub(super) fn reset(outcome: PasswordReset) {
    let label = match outcome {
        PasswordReset::Reset => "reset",
        PasswordReset::InvalidResetCode => "invalid_reset_code",
        PasswordReset::RateLimited => "rate_limited",
    };
    PASSWORD_RESETS.add(1, [label.into()]);
}
