//! What `auth` reports (ERR-01).

use platform::{
    errors::{Category, ErrorCode, FieldError, Internal, RequestError},
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
    /// The session hasn't proved its user's password within the last 5 minutes (AUTH-023).
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
            AccountsError::Internal(error) => Self::Internal(error),
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
            | Self::InvalidResetCode => Category::Unauthenticated,
            Self::WrongPassword | Self::ReauthenticationRequired => Category::Forbidden,
            Self::OwnsSharedWorkspace | Self::AccountChanged | Self::EmailAlreadyVerified => {
                Category::Conflict
            }
            Self::WrongCode | Self::CodeExpired => Category::BusinessRule,
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
