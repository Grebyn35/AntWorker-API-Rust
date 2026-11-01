//! The audit actions `auth` records (ARCH-03).

/// A user registered (AUTH-001).
pub(super) const USER_REGISTERED: &str = "user.registered";
/// A session began, by signing in (AUTH-004).
pub(super) const SESSION_CREATED: &str = "session.created";
/// A sign-in to an existing account gave the wrong password (AUTH-005).
pub(super) const SIGN_IN_FAILED: &str = "auth.sign_in_failed";
/// A session's tokens were rotated (AUTH-010).
pub(super) const SESSION_REFRESHED: &str = "session.refreshed";
/// A rotated refresh token was presented again, which ended its session (AUTH-012).
pub(super) const SESSION_REUSE_DETECTED: &str = "session.reuse_detected";
/// A session ended, by signing out or by a sign-in on its device (AUTH-008, AUTH-013).
pub(super) const SESSION_ENDED: &str = "session.ended";
