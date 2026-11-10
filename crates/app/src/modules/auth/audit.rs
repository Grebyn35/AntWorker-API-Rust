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
/// A session ended, by signing out or by a sign-in on its device (AUTH-008, AUTH-013).
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
/// A code that the signed-in user entered wasn't their account's (AUTH-032).
pub(super) const VERIFICATION_CODE_CHECK_FAILED: Action =
    Action::new("verification_code.check_failed");
/// The account proved its email address (AUTH-031).
pub(super) const EMAIL_VERIFIED: Action = Action::new("email.verified");

/// A user's account.
pub(super) const USER: Resource = Resource::new("user");
/// A user's way of signing in, such as their password.
pub(super) const IDENTITY: Resource = Resource::new("identity");
/// A signed-in device or client.
pub(super) const SESSION: Resource = Resource::new("session");
/// The token that tells a sign-in from a device that signed in to the account before.
pub(super) const DEVICE_TOKEN: Resource = Resource::new("device_token");
/// A code sent to the account's email address, which proves that the account holds it.
pub(super) const VERIFICATION_CODE: Resource = Resource::new("verification_code");
