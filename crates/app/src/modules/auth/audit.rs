//! The audit actions `auth` records, the kinds of resource they happen to, and their details
//! (ARCH-03, the audit spec's "The action catalog").
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use serde::Serialize;
use utoipa::ToSchema;

use crate::modules::audit::{Action, Catalogued, Resource};

/// A user registered (AUTH-001).
pub(super) const USER_REGISTERED: Action<ThroughProvider> = Action::new(
    "user.registered",
    "The user registered an account.",
    &[USER],
)
.in_own_log();
/// A session began, by signing in (AUTH-004).
pub(super) const SESSION_CREATED: Action<ThroughProvider> = Action::new(
    "session.created",
    "The user signed in, which began a session.",
    &[SESSION],
)
.in_own_log();
/// A sign-in to an existing account gave the wrong password (AUTH-005).
pub(super) const SIGN_IN_FAILED: Action = Action::new(
    "sign_in.failed",
    "A sign-in to the account gave a wrong password.",
    &[USER],
)
.in_own_log();
/// A password stored with older parameters was hashed again at sign-in (AUTH-014).
pub(super) const PASSWORD_REHASHED: Action = Action::new(
    "password.rehashed",
    "A sign-in stored the account's password again, hashed with newer parameters.",
    &[IDENTITY],
)
.in_own_log();
/// A session's tokens were rotated (AUTH-010).
pub(super) const SESSION_REFRESHED: Action = Action::new(
    "session.refreshed",
    "A session's tokens were renewed.",
    &[SESSION],
)
.in_own_log();
/// A rotated refresh token was presented again, which ended its session (AUTH-012).
pub(super) const SESSION_REUSE_DETECTED: Action = Action::new(
    "session.reuse_detected",
    "A session's refresh token that was already renewed was used again, which ended the session.",
    &[SESSION],
)
.in_own_log();
/// A session ended, by signing out, by a sign-in on its device, by a reset or a change of its
/// user's password, or as its user ended it from another of their sessions (AUTH-008, AUTH-013,
/// AUTH-037, AUTH-040, AUTH-044, AUTH-045).
pub(super) const SESSION_ENDED: Action = Action::new(
    "session.ended",
    "A session ended: its user signed out or ended it, or a change ended it, such as a new \
     sign-in on its device, or a change of the account's password.",
    &[SESSION],
)
.in_own_log();
/// A sign-in issued its device a device token, ending the one it used and the account's expired
/// ones (AUTH-019).
pub(super) const DEVICE_TOKEN_ISSUED: Action = Action::new(
    "device_token.issued",
    "A sign-in gave its device a new device token, which its next sign-in from the device \
     brings.",
    &[DEVICE_TOKEN],
)
.in_own_log();
/// A session proved again who its user is, with their password or through a provider (AUTH-021,
/// AUTH-080).
pub(super) const SESSION_REAUTHENTICATED: Action<ThroughProvider> = Action::new(
    "session.reauthenticated",
    "The user proved who they are again in a session, for an action that asks for a recent proof.",
    &[SESSION],
)
.in_own_log();
/// A signed-in user's password was wrong (AUTH-021).
pub(super) const PASSWORD_CHECK_FAILED: Action = Action::new(
    "password.check_failed",
    "A signed-in user gave a wrong password.",
    &[USER],
)
.in_own_log();
/// A code that proves the account's email address was issued, and sent to it (AUTH-030).
pub(super) const VERIFICATION_CODE_ISSUED: Action = Action::new(
    "verification_code.issued",
    "A code that proves the account's email address was sent to it.",
    &[VERIFICATION_CODE],
)
.in_own_log();
/// A code that the signed-in user entered wasn't their account's (AUTH-032, AUTH-052).
pub(super) const VERIFICATION_CODE_CHECK_FAILED: Action = Action::new(
    "verification_code.check_failed",
    "A signed-in user gave a wrong code to prove an email address.",
    &[VERIFICATION_CODE],
)
.in_own_log();
/// The account proved its email address (AUTH-031), or a reset proved it (AUTH-037).
pub(super) const EMAIL_VERIFIED: Action = Action::new(
    "email.verified",
    "The account's email address was proved.",
    &[USER],
)
.in_own_log();
/// A code that resets the account's password was issued, and sent to its address (AUTH-036).
pub(super) const PASSWORD_RESET_CODE_ISSUED: Action = Action::new(
    "password_reset_code.issued",
    "A code that resets the account's password was sent to its email address.",
    &[VERIFICATION_CODE],
)
.in_own_log();
/// A confirmation of a reset for the account didn't reset its password (AUTH-038).
pub(super) const PASSWORD_RESET_FAILED: Action = Action::new(
    "password_reset.failed",
    "A reset of the account's password gave a wrong code.",
    &[USER],
)
.in_own_log();
/// A reset set the account's password (AUTH-037).
pub(super) const PASSWORD_RESET: Action = Action::new(
    "password.reset",
    "A reset set the account's password.",
    &[IDENTITY],
)
.in_own_log();
/// The signed-in user changed their password (AUTH-040).
pub(super) const PASSWORD_CHANGED: Action = Action::new(
    "password.changed",
    "The user changed their password.",
    &[IDENTITY],
)
.in_own_log();
/// A code that changes the account's address to the one it was sent to was issued, and sent there
/// (AUTH-050).
pub(super) const EMAIL_CHANGE_CODE_ISSUED: Action = Action::new(
    "email_change_code.issued",
    "A code that changes the account's email address to a new one was sent to the new one.",
    &[VERIFICATION_CODE],
)
.in_own_log();
/// The signed-in user changed their account's address to one they proved (AUTH-051).
pub(super) const EMAIL_CHANGED: Action = Action::new(
    "email.changed",
    "The user changed their account's email address to one they proved.",
    &[USER],
)
.in_own_log();

/// A sign-in to an account with an authenticator app, with its password or through a provider,
/// issued a challenge for the app's code (AUTH-058).
pub(super) const SIGN_IN_CHALLENGED: Action<ThroughProvider> = Action::new(
    "sign_in.challenged",
    "A sign-in to an account with an authenticator app asked for the app's code.",
    &[USER],
)
.in_own_log();
/// A setup of an authenticator app began (AUTH-055).
pub(super) const SECOND_FACTOR_SETUP_STARTED: Action = Action::new(
    "second_factor.setup_started",
    "The user began to set up an authenticator app.",
    &[SECOND_FACTOR],
)
.in_own_log();
/// A setup's confirmation made its authenticator app the account's (AUTH-056).
pub(super) const SECOND_FACTOR_ENABLED: Action = Action::new(
    "second_factor.enabled",
    "The user set up an authenticator app, which their sign-ins now ask the code of.",
    &[SECOND_FACTOR],
)
.in_own_log();
/// The account's authenticator app was removed (AUTH-064, AUTH-072).
pub(super) const SECOND_FACTOR_REMOVED: Action = Action::new(
    "second_factor.removed",
    "The account's authenticator app was removed, by the user or by an operator.",
    &[SECOND_FACTOR],
)
.in_own_log();
/// A second factor's code that a request gave wasn't the account's (AUTH-056, AUTH-060, AUTH-065).
pub(super) const SECOND_FACTOR_CHECK_FAILED: Action = Action::new(
    "second_factor.check_failed",
    "A request gave a code of the account's authenticator app, or a recovery code, that isn't \
     the account's.",
    &[USER],
)
.in_own_log();
/// A recovery code stood in for the app's code, which used it up (AUTH-062).
pub(super) const RECOVERY_CODE_USED: Action = Action::new(
    "recovery_code.used",
    "A recovery code stood in for the authenticator app's code, and can't be used again.",
    &[RECOVERY_CODE],
)
.in_own_log();
/// New recovery codes replaced the account's (AUTH-063).
pub(super) const RECOVERY_CODES_ISSUED: Action = Action::new(
    "recovery_codes.issued",
    "The user got new recovery codes, in place of their others.",
    &[USER],
)
.in_own_log();
/// A signed-in user's re-authentication through a provider named another of its accounts than
/// theirs (AUTH-080).
pub(super) const IDENTITY_CHECK_FAILED: Action<LinkedProvider> = Action::new(
    "identity.check_failed",
    "A signed-in user's proof through a provider named another account than theirs.",
    &[USER],
)
.in_own_log();
/// The signed-in user added a password to their account, which had none (AUTH-086).
pub(super) const PASSWORD_ADDED: Action = Action::new(
    "password.added",
    "The user added a password to their account, which had none.",
    &[IDENTITY],
)
.in_own_log();
/// The signed-in user removed their account's password (AUTH-088).
pub(super) const PASSWORD_REMOVED: Action = Action::new(
    "password.removed",
    "The user removed their account's password.",
    &[IDENTITY],
)
.in_own_log();
/// The signed-in user linked their account at a provider, which then signs in to their account
/// (AUTH-087).
pub(super) const PROVIDER_LINKED: Action<LinkedProvider> = Action::new(
    "provider.linked",
    "The user linked their account at a provider, which then signs in to their account.",
    &[IDENTITY],
)
.in_own_log();
/// The signed-in user removed their account at a provider from the ways their account signs in
/// (AUTH-088).
pub(super) const PROVIDER_UNLINKED: Action<LinkedProvider> = Action::new(
    "provider.unlinked",
    "The user removed their account at a provider from the ways they sign in.",
    &[IDENTITY],
)
.in_own_log();
/// A user's account.
pub(super) const USER: Resource = Resource::new("user");
/// A user's way of signing in: their password, or their account at a provider.
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

/// Every action `auth` records, which the action catalog lists (AUDIT-012).
pub(crate) const ACTIONS: &[&dyn Catalogued] = &[
    &USER_REGISTERED,
    &SESSION_CREATED,
    &SIGN_IN_FAILED,
    &PASSWORD_REHASHED,
    &SESSION_REFRESHED,
    &SESSION_REUSE_DETECTED,
    &SESSION_ENDED,
    &DEVICE_TOKEN_ISSUED,
    &SESSION_REAUTHENTICATED,
    &PASSWORD_CHECK_FAILED,
    &VERIFICATION_CODE_ISSUED,
    &VERIFICATION_CODE_CHECK_FAILED,
    &EMAIL_VERIFIED,
    &PASSWORD_RESET_CODE_ISSUED,
    &PASSWORD_RESET_FAILED,
    &PASSWORD_RESET,
    &PASSWORD_CHANGED,
    &EMAIL_CHANGE_CODE_ISSUED,
    &EMAIL_CHANGED,
    &SIGN_IN_CHALLENGED,
    &SECOND_FACTOR_SETUP_STARTED,
    &SECOND_FACTOR_ENABLED,
    &SECOND_FACTOR_REMOVED,
    &SECOND_FACTOR_CHECK_FAILED,
    &RECOVERY_CODE_USED,
    &RECOVERY_CODES_ISSUED,
    &IDENTITY_CHECK_FAILED,
    &PASSWORD_ADDED,
    &PASSWORD_REMOVED,
    &PROVIDER_LINKED,
    &PROVIDER_UNLINKED,
];

/// The details of a sign-in's actions, and of a re-authentication's: the provider that it went
/// through, absent for one with a password.
#[derive(Clone, Debug, Default, Serialize, ToSchema)]
pub(super) struct ThroughProvider {
    /// The provider's key, such as `google`, as the capabilities list it (AUTH-081).
    #[serde(skip_serializing_if = "Option::is_none")]
    provider: Option<String>,
}

impl ThroughProvider {
    /// The details of an action of a sign-in or a re-authentication through `provider`, or with
    /// a password, for none (AUTH-073).
    pub(super) fn new(provider: Option<&str>) -> Self {
        Self {
            provider: provider.map(str::to_owned),
        }
    }
}

/// The details of an action on the user's account at a provider: the provider.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub(super) struct LinkedProvider {
    /// The provider's key, such as `google`, as the capabilities list it (AUTH-081).
    provider: String,
}

impl LinkedProvider {
    /// The details of an action on the user's account at `provider`.
    pub(super) fn new(provider: &str) -> Self {
        Self {
            provider: provider.to_owned(),
        }
    }
}
