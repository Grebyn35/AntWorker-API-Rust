//! The audit actions `auth` records, and the kinds of resource they happen to (ARCH-03, the
//! audit spec).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use crate::modules::audit::{Action, Resource};

/// A user registered (AUTH-001).
pub(super) const USER_REGISTERED: Action = Action::new("user.registered");
/// A session began, by signing in (AUTH-004).
pub(super) const SESSION_CREATED: Action = Action::new("session.created");
/// A sign-in to an existing account gave the wrong password (AUTH-005).
pub(super) const SIGN_IN_FAILED: Action = Action::new("sign_in.failed");
/// A password stored with older parameters was hashed again at sign-in (AUTH-014).
pub(super) const PASSWORD_REHASHED: Action = Action::new("password.rehashed");
/// A session's tokens were rotated (AUTH-010).
pub(super) const SESSION_REFRESHED: Action = Action::new("session.refreshed");
/// A rotated refresh token was presented again, which ended its session (AUTH-012).
pub(super) const SESSION_REUSE_DETECTED: Action = Action::new("session.reuse_detected");
/// A session ended, by signing out, by a sign-in on its device, by a reset or a change of its
/// user's password, or as its user ended it from another of their sessions (AUTH-008, AUTH-013,
/// AUTH-037, AUTH-040, AUTH-044, AUTH-045).
pub(super) const SESSION_ENDED: Action = Action::new("session.ended");
/// A sign-in issued its device a device token, ending the one it used and the account's expired
/// ones (AUTH-019).
pub(super) const DEVICE_TOKEN_ISSUED: Action = Action::new("device_token.issued");
/// A session proved its user's password again (AUTH-021).
pub(super) const SESSION_REAUTHENTICATED: Action = Action::new("session.reauthenticated");
/// A signed-in user's password was wrong (AUTH-021).
pub(super) const PASSWORD_CHECK_FAILED: Action = Action::new("password.check_failed");
/// A code that proves the account's email address was issued, and sent to it (AUTH-030).
pub(super) const VERIFICATION_CODE_ISSUED: Action = Action::new("verification_code.issued");
/// A code that the signed-in user entered wasn't their account's (AUTH-032, AUTH-052).
pub(super) const VERIFICATION_CODE_CHECK_FAILED: Action =
    Action::new("verification_code.check_failed");
/// The account proved its email address (AUTH-031), or a reset proved it (AUTH-037).
pub(super) const EMAIL_VERIFIED: Action = Action::new("email.verified");
/// A code that resets the account's password was issued, and sent to its address (AUTH-036).
pub(super) const PASSWORD_RESET_CODE_ISSUED: Action = Action::new("password_reset_code.issued");
/// A confirmation of a reset for the account didn't reset its password (AUTH-038).
pub(super) const PASSWORD_RESET_FAILED: Action = Action::new("password_reset.failed");
/// A reset set the account's password (AUTH-037).
pub(super) const PASSWORD_RESET: Action = Action::new("password.reset");
/// The signed-in user changed their password (AUTH-040).
pub(super) const PASSWORD_CHANGED: Action = Action::new("password.changed");
/// A code that changes the account's address to the one it was sent to was issued, and sent there
/// (AUTH-050).
pub(super) const EMAIL_CHANGE_CODE_ISSUED: Action = Action::new("email_change_code.issued");
/// The signed-in user changed their account's address to one they proved (AUTH-051).
pub(super) const EMAIL_CHANGED: Action = Action::new("email.changed");

/// A sign-in's password matched, for an account with an authenticator app, which issued a
/// challenge for its code (AUTH-058).
pub(super) const SIGN_IN_CHALLENGED: Action = Action::new("sign_in.challenged");
/// A setup of an authenticator app began (AUTH-055).
pub(super) const SECOND_FACTOR_SETUP_STARTED: Action = Action::new("second_factor.setup_started");
/// A setup's confirmation made its authenticator app the account's (AUTH-056).
pub(super) const SECOND_FACTOR_ENABLED: Action = Action::new("second_factor.enabled");
/// The account's authenticator app was removed (AUTH-064, AUTH-072).
pub(super) const SECOND_FACTOR_REMOVED: Action = Action::new("second_factor.removed");
/// A second factor's code that a request gave wasn't the account's (AUTH-056, AUTH-060, AUTH-065).
pub(super) const SECOND_FACTOR_CHECK_FAILED: Action = Action::new("second_factor.check_failed");
/// A recovery code stood in for the app's code, which used it up (AUTH-062).
pub(super) const RECOVERY_CODE_USED: Action = Action::new("recovery_code.used");
/// New recovery codes replaced the account's (AUTH-063).
pub(super) const RECOVERY_CODES_ISSUED: Action = Action::new("recovery_codes.issued");
/// A user's account.
pub(super) const USER: Resource = Resource::new("user");
/// A user's way of signing in, such as their password.
pub(super) const IDENTITY: Resource = Resource::new("identity");
/// A signed-in device or client.
pub(super) const SESSION: Resource = Resource::new("session");
/// The token that tells a sign-in from a device that signed in to the account before.
pub(super) const DEVICE_TOKEN: Resource = Resource::new("device_token");
/// A code sent to an address, which proves that the account holds its address, resets its
/// password, or changes its address to the one the code was sent to.
pub(super) const VERIFICATION_CODE: Resource = Resource::new("verification_code");
/// A user's second factor: their authenticator app, or its setup.
pub(super) const SECOND_FACTOR: Resource = Resource::new("second_factor");
/// A code that stands in once for an authenticator app's code.
pub(super) const RECOVERY_CODE: Resource = Resource::new("recovery_code");
