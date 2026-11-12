//! The metrics `auth` declares (OBS-11).

use platform::telemetry::{metrics::Counter, names};

/// Sign-in attempts, by how each ended.
static SIGN_INS: Counter<1> = Counter::new(
    "auth.sign_ins",
    "{attempt}",
    "Sign-in attempts, by how each ended: succeeded, challenged, for an account whose second \
     factor completes it, invalid_credentials or rate_limited (AUTH-004 to AUTH-006, AUTH-058).",
    [names::AUTH_OUTCOME],
);

/// The exchanges of providers' codes, by provider and by how each ended.
static PROVIDER_EXCHANGES: Counter<2> = Counter::new(
    "auth.provider_exchanges",
    "{exchange}",
    "Exchanges of an OAuth provider's code for the user's identity, by provider and by how each \
     ended: succeeded, rejected, when the provider refused the code, or unavailable, when it \
     refused the API's own client, failed, answered what the API can't read, or didn't answer in \
     time (AUTH-077).",
    [names::AUTH_PROVIDER, names::AUTH_OUTCOME],
);

/// How an exchange of a provider's code ended (AUTH-077).
#[derive(Clone, Copy, Debug)]
pub(super) enum Exchange {
    /// The provider gave the user's identity.
    Succeeded,
    /// The provider refused the code, its verifier or its redirect URI.
    Rejected,
    /// The provider failed, answered what the API can't read, or didn't answer in time.
    Unavailable,
}

/// Counts an exchange at `provider` that ended as `outcome`.
pub(super) fn exchange(provider: &'static str, outcome: Exchange) {
    let outcome = match outcome {
        Exchange::Succeeded => "succeeded",
        Exchange::Rejected => "rejected",
        Exchange::Unavailable => "unavailable",
    };
    PROVIDER_EXCHANGES.add(1, [provider.into(), outcome.into()]);
}

/// How a sign-in ended.
#[derive(Clone, Copy, Debug)]
pub(super) enum SignIn {
    /// A session began (AUTH-004).
    Succeeded,
    /// The password matched, and a challenge waits for the account's second factor (AUTH-058).
    Challenged,
    /// The credentials matched no account (AUTH-005).
    InvalidCredentials,
    /// A sign-in limit refused it: one of the email's, or its device token's (AUTH-006).
    RateLimited,
}

/// Counts a sign-in that ended as `outcome`.
pub(super) fn sign_in(outcome: SignIn) {
    let label = match outcome {
        SignIn::Succeeded => "succeeded",
        SignIn::Challenged => "challenged",
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

/// Confirmations of changes of an account's address, by how each ended.
static EMAIL_CHANGES: Counter<1> = Counter::new(
    "auth.email_changes",
    "{confirmation}",
    "Confirmations of changes of an account's email address, by how each ended: changed, \
     wrong_code, code_expired, email_in_use or rate_limited (AUTH-051, AUTH-052, AUTH-033).",
    [names::AUTH_OUTCOME],
);

/// How a confirmation of a change of the address ended.
#[derive(Clone, Copy, Debug)]
pub(super) enum EmailChange {
    /// The account's address changed (AUTH-051).
    Changed,
    /// The code wasn't the account's for the address (AUTH-052).
    WrongCode,
    /// The account had no code for a change that lasted (AUTH-052).
    CodeExpired,
    /// Another account took the address since the code was issued (AUTH-052).
    EmailInUse,
    /// The limit on the account's checks of codes refused it (AUTH-033).
    RateLimited,
}

/// Counts a confirmation of a change of the address that ended as `outcome`.
pub(super) fn email_change(outcome: EmailChange) {
    let label = match outcome {
        EmailChange::Changed => "changed",
        EmailChange::WrongCode => "wrong_code",
        EmailChange::CodeExpired => "code_expired",
        EmailChange::EmailInUse => "email_in_use",
        EmailChange::RateLimited => "rate_limited",
    };
    EMAIL_CHANGES.add(1, [label.into()]);
}

/// Checks of second factors' codes, by how each ended.
static SECOND_FACTOR_CHECKS: Counter<1> = Counter::new(
    "auth.second_factor_checks",
    "{check}",
    "Checks of second factors' codes, an authenticator app's or a recovery code, at a sign-in's \
     completion, a setup's confirmation, a reset's confirmation, a re-authentication or a change \
     of the password, by how each ended: passed, wrong_code or rate_limited (AUTH-037, AUTH-056 \
     to AUTH-065).",
    [names::AUTH_OUTCOME],
);

/// How a check of a second factor's code ended.
#[derive(Clone, Copy, Debug)]
pub(super) enum FactorCheck {
    /// The code was the account's (AUTH-057, AUTH-062).
    Passed,
    /// The code wasn't the account's (AUTH-056, AUTH-060, AUTH-065).
    WrongCode,
    /// A limit on the checks refused it (AUTH-061).
    RateLimited,
}

/// Counts a check of a second factor's code that ended as `outcome`.
pub(super) fn factor_check(outcome: FactorCheck) {
    let label = match outcome {
        FactorCheck::Passed => "passed",
        FactorCheck::WrongCode => "wrong_code",
        FactorCheck::RateLimited => "rate_limited",
    };
    SECOND_FACTOR_CHECKS.add(1, [label.into()]);
}
