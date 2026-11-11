//! The fixed limits of the actions of `auth` that check a password or a code, or send an email
//! (SEC-13), each of which a behavior of its spec states. The deployment's settings give only the
//! sign-in limits (AUTH-006), which the composition root reads.

use std::time::Duration;

use platform::rate_limit::Bucket;

/// The bucket of each user's checks of their password, once signed in: 5 in 15 minutes, fixed
/// (AUTH-022).
pub(super) const PASSWORD_CHECK: Bucket = Bucket {
    name: "password_check",
    limit: 5,
    window: Duration::from_secs(900),
};

/// The bucket of the deletions of an email's accounts, each of which sends the address an email:
/// 5 in 24 hours, fixed (AUTH-024).
pub(super) const DELETION_EMAIL: Bucket = Bucket {
    name: "deletion_email",
    limit: 5,
    window: Duration::from_secs(86_400),
};

/// The bucket of the registrations of an email address, each of which sends the address a code
/// while email is on: 3 in 15 minutes, fixed (AUTH-029).
pub(super) const REGISTRATION_EMAIL: Bucket = Bucket {
    name: "registration_email",
    limit: 3,
    window: Duration::from_secs(900),
};

/// The bucket of the requests for a code to an email address, each of which sends the address an
/// email: 3 in 15 minutes, fixed, apart from the registrations', which anyone may send, so that
/// none of them keeps the account's owner from asking for a code (AUTH-029).
pub(super) const VERIFICATION_EMAIL: Bucket = Bucket {
    name: "verification_email",
    limit: 3,
    window: Duration::from_secs(900),
};

/// The bucket of each user's checks of the codes that prove their address, or a new one: 10 in
/// 24 hours, fixed (AUTH-033).
pub(super) const CODE_CHECK: Bucket = Bucket {
    name: "code_check",
    limit: 10,
    window: Duration::from_secs(86_400),
};

/// The bucket of the requests for a reset code to an email address, each of which sends the
/// address an email: 3 in 10 minutes, fixed, apart from the registrations' and the verification
/// codes'. Its window lasts at most as long as a code, so while it is full, a code it let through
/// in its window still works, and no one who fills it keeps the owner from resetting (AUTH-035).
pub(super) const RESET_EMAIL: Bucket = Bucket {
    name: "reset_email",
    limit: 3,
    window: Duration::from_secs(600),
};

/// The bucket of an email's confirmations of a reset from one client address: 5 in 15 minutes,
/// fixed, with no count of the email from every address, so no one elsewhere fills the owner's
/// (AUTH-039).
pub(super) const RESET_CHECK: Bucket = Bucket {
    name: "reset_check",
    limit: 5,
    window: Duration::from_secs(900),
};

/// The bucket of the requests to change an account's address to an email address, each of which
/// sends that address an email: 3 in 10 minutes, fixed, apart from the registrations', the
/// verification codes' and the resets'. Its window lasts at most as long as a code, so while it is
/// full, the last code it let through still works (AUTH-049).
pub(super) const CHANGE_EMAIL: Bucket = Bucket {
    name: "change_email",
    limit: 3,
    window: Duration::from_secs(600),
};

/// The bucket of each user's checks of a second factor from a signed-in session, a setup's
/// confirmation's, a re-authentication's or a change of the password's: 10 in 24 hours, fixed
/// (AUTH-061).
pub(super) const SECOND_FACTOR_CHECK: Bucket = Bucket {
    name: "second_factor_check",
    limit: 10,
    window: Duration::from_secs(86_400),
};

/// The bucket of each user's completions of a sign-in that bring no valid device token: 10 in 24
/// hours, fixed (AUTH-061).
pub(super) const SECOND_FACTOR_SIGN_IN: Bucket = Bucket {
    name: "second_factor_sign_in",
    limit: 10,
    window: Duration::from_secs(86_400),
};

/// The bucket of the completions of a sign-in whose challenge holds a valid device token, in place
/// of the user's, so no completion from elsewhere fills it: 10 in 24 hours, fixed (AUTH-061).
pub(super) const SECOND_FACTOR_DEVICE: Bucket = Bucket {
    name: "second_factor_device",
    limit: 10,
    window: Duration::from_secs(86_400),
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::auth::service::verification::CODE_MINUTES;

    #[test]
    fn auth_035_the_window_of_the_requests_for_reset_codes_lasts_at_most_as_long_as_a_code() {
        let lasts = Duration::from_secs(60 * u64::try_from(CODE_MINUTES).unwrap());
        assert!(RESET_EMAIL.window <= lasts);
    }

    #[test]
    fn auth_049_the_window_of_the_requests_to_change_to_an_address_lasts_at_most_as_long_as_a_code()
    {
        let lasts = Duration::from_secs(60 * u64::try_from(CODE_MINUTES).unwrap());
        assert!(CHANGE_EMAIL.window <= lasts);
        assert_eq!(
            (CHANGE_EMAIL.limit, CHANGE_EMAIL.window.as_secs()),
            (3, 600)
        );
    }
}
