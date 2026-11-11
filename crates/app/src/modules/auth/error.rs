//! What `auth` reports (ERR-01).

use platform::{
    errors::{Category, ErrorCode, FieldError, Internal, PlatformError, RequestError},
    rate_limit::LimitError,
};

use crate::modules::{accounts::AccountsError, audit::AuditError};

/// A failure of `auth`.
#[derive(Debug, thiserror::Error)]
pub(crate) enum AuthError {
    /// Another user has the email (AUTH-002).
    #[error("another user has the email")]
    EmailInUse,
    /// The email and password match no account (AUTH-005).
    #[error("the credentials match no account")]
    InvalidCredentials,
    /// The refresh token or its session has expired (AUTH-011).
    #[error("the session has expired")]
    SessionExpired,
    /// No session has the refresh token as its current one (AUTH-012).
    #[error("the refresh token is invalid")]
    InvalidRefreshToken,
    /// The signed-in user's password is wrong, or they have none (AUTH-021).
    #[error("the password is wrong")]
    WrongPassword,
    /// The session hasn't proved its user's password within the last 5 minutes, which deleting the
    /// account and ending another session need (AUTH-023, AUTH-044, AUTH-045).
    #[error("the action needs the password proved again")]
    ReauthenticationRequired,
    /// The user owns a workspace that has another member, so their account isn't deleted
    /// (ACCOUNTS-012).
    #[error("the user owns a workspace that has another member")]
    OwnsSharedWorkspace,
    /// A request of the user's changed their account while its deletion ran, which rolled back
    /// (ACCOUNTS-016).
    #[error("the account changed while it was being deleted")]
    AccountChanged,
    /// The account proved its email address already (AUTH-028, AUTH-032).
    #[error("the email address is verified already")]
    EmailAlreadyVerified,
    /// The code isn't the account's (AUTH-032).
    #[error("the code is wrong")]
    WrongCode,
    /// The account has no code that lasts: its code expired, or none was issued since its last
    /// proved one (AUTH-032).
    #[error("the account has no code that lasts")]
    CodeExpired,
    /// The reset code isn't the reset code of an account that holds the address, or has expired
    /// (AUTH-038).
    #[error("the reset code resets no account's password")]
    InvalidResetCode,
    /// None of the caller's sessions has the ID (AUTH-044).
    #[error("no session of the caller's has the ID")]
    SessionNotFound,
    /// The caller has no setup of an authenticator app that lasts (AUTH-056).
    #[error("no setup of an authenticator app lasts")]
    SetupExpired,
    /// The caller has no authenticator app (AUTH-063).
    #[error("the account has no authenticator app")]
    NoSecondFactor,
    /// No sign-in waits for its second factor with the challenge (AUTH-060).
    #[error("the challenge completes no sign-in")]
    InvalidChallenge,
    /// The code that completes a sign-in isn't its user's (AUTH-060).
    #[error("the second factor's code is wrong")]
    InvalidSecondFactor,
    /// The account has an authenticator app, whose code the request lacks (AUTH-065).
    #[error("the request lacks the second factor's code")]
    SecondFactorRequired,
    /// The signed-in user's second factor's code is wrong (AUTH-065).
    #[error("the second factor's code is wrong")]
    WrongSecondFactor,
    /// A rate limit refused the request, or couldn't count it (AUTH-006).
    #[error(transparent)]
    Limit(#[from] LimitError),
    /// The request failed before `auth` ran, such as its body's validation.
    #[error(transparent)]
    Request(#[from] RequestError),
    /// Anything else (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

impl From<AccountsError> for AuthError {
    fn from(error: AccountsError) -> Self {
        match error {
            AccountsError::Internal(error)
            | AccountsError::Platform(PlatformError::Internal(error)) => Self::Internal(error),
            AccountsError::Platform(PlatformError::Request(error)) => Self::Request(error),
            other => Self::Internal(Internal::new(other)),
        }
    }
}

impl From<AuditError> for AuthError {
    fn from(error: AuditError) -> Self {
        match error {
            AuditError::Internal(error) => Self::Internal(error),
        }
    }
}

impl ErrorCode for AuthError {
    fn code(&self) -> &'static str {
        match self {
            Self::EmailInUse => "auth.email_in_use",
            Self::InvalidCredentials => "auth.invalid_credentials",
            Self::SessionExpired => "auth.session_expired",
            Self::InvalidRefreshToken => "auth.invalid_refresh_token",
            Self::WrongPassword => "auth.wrong_password",
            Self::ReauthenticationRequired => "auth.reauthentication_required",
            Self::OwnsSharedWorkspace => "auth.owns_shared_workspace",
            Self::AccountChanged => "auth.account_changed",
            Self::EmailAlreadyVerified => "auth.email_already_verified",
            Self::WrongCode => "auth.wrong_code",
            Self::CodeExpired => "auth.code_expired",
            Self::InvalidResetCode => "auth.invalid_reset_code",
            Self::SessionNotFound => "auth.session_not_found",
            Self::SetupExpired => "auth.setup_expired",
            Self::NoSecondFactor => "auth.no_second_factor",
            Self::InvalidChallenge => "auth.invalid_challenge",
            Self::InvalidSecondFactor => "auth.invalid_second_factor",
            Self::SecondFactorRequired => "auth.second_factor_required",
            Self::WrongSecondFactor => "auth.wrong_second_factor",
            Self::Limit(error) => error.code(),
            Self::Request(error) => error.code(),
            Self::Internal(error) => error.code(),
        }
    }

    fn title(&self) -> &'static str {
        match self {
            Self::EmailInUse => "Email in use",
            Self::InvalidCredentials => "Invalid email or password",
            Self::SessionExpired => "Session expired",
            Self::InvalidRefreshToken => "Invalid refresh token",
            Self::WrongPassword => "Wrong password",
            Self::ReauthenticationRequired => "Re-authentication required",
            Self::OwnsSharedWorkspace => "Owner of a shared workspace",
            Self::AccountChanged => "Account changed",
            Self::EmailAlreadyVerified => "Email address already verified",
            Self::WrongCode => "Wrong code",
            Self::CodeExpired => "Code expired",
            Self::InvalidResetCode => "Invalid email or reset code",
            Self::SessionNotFound => "Session not found",
            Self::SetupExpired => "Setup expired",
            Self::NoSecondFactor => "No second factor",
            Self::InvalidChallenge => "Invalid challenge",
            Self::InvalidSecondFactor => "Invalid second factor",
            Self::SecondFactorRequired => "Second factor required",
            Self::WrongSecondFactor => "Wrong second factor",
            Self::Limit(error) => error.title(),
            Self::Request(error) => error.title(),
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::EmailInUse => Category::Conflict,
            Self::InvalidCredentials
            | Self::SessionExpired
            | Self::InvalidRefreshToken
            | Self::InvalidResetCode
            | Self::InvalidChallenge
            | Self::InvalidSecondFactor => Category::Unauthenticated,
            Self::WrongPassword
            | Self::ReauthenticationRequired
            | Self::SecondFactorRequired
            | Self::WrongSecondFactor => Category::Forbidden,
            Self::OwnsSharedWorkspace
            | Self::AccountChanged
            | Self::EmailAlreadyVerified
            | Self::NoSecondFactor => Category::Conflict,
            Self::WrongCode | Self::CodeExpired | Self::SetupExpired => Category::BusinessRule,
            Self::SessionNotFound => Category::NotFound,
            Self::Limit(error) => error.category(),
            Self::Request(error) => error.category(),
            Self::Internal(error) => error.category(),
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
            Self::OwnsSharedWorkspace => {
                Some("You own a workspace that has other members.".to_owned())
            }
            Self::AccountChanged => Some(
                "Another request changed the account while it was being deleted; send the \
                 deletion again."
                    .to_owned(),
            ),
            Self::WrongCode => {
                Some("The code isn't the one sent last: check it, or ask for another.".to_owned())
            }
            Self::CodeExpired => Some(
                "The code's 10 minutes have passed, or a newer one replaced it: ask for another."
                    .to_owned(),
            ),
            Self::InvalidResetCode => Some(
                "The code isn't one sent to the address, or its 10 minutes have passed, or a \
                 reset used one of the address's codes: ask for another."
                    .to_owned(),
            ),
            Self::SetupExpired => Some(
                "The setup's 15 minutes have passed, or another confirmation ended it: start \
                 another."
                    .to_owned(),
            ),
            Self::InvalidChallenge => {
                Some("The sign-in's 5 minutes have passed, or it ended: sign in again.".to_owned())
            }
            Self::SecondFactorRequired => Some(
                "The account has an authenticator app: send its code, or a recovery code, with \
                 the password."
                    .to_owned(),
            ),
            Self::Request(error) => error.detail(),
            _ => None,
        }
    }

    fn errors(&self) -> Option<Vec<FieldError>> {
        match self {
            Self::Request(error) => error.errors(),
            _ => None,
        }
    }

    /// The wrapped error's, such as a disabled feature's hour (OPS-04).
    fn retry_after_seconds(&self) -> u32 {
        match self {
            Self::Limit(error) => error.retry_after_seconds(),
            Self::Request(error) => error.retry_after_seconds(),
            Self::Internal(error) => error.retry_after_seconds(),
            _ => 1,
        }
    }
}
