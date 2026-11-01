//! What `auth` reports (ERR-01).

use platform::{
    errors::{Category, ErrorCode, FieldError, Internal, RequestError},
    rate_limit::LimitError,
};

use crate::modules::accounts::AccountsError;

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
            AccountsError::EmailInUse => Self::EmailInUse,
            AccountsError::Request(error) => Self::Request(error),
            AccountsError::Internal(error) => Self::Internal(error),
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
            Self::Limit(error) => error.title(),
            Self::Request(error) => error.title(),
            Self::Internal(error) => error.title(),
        }
    }

    fn category(&self) -> Category {
        match self {
            Self::EmailInUse => Category::Conflict,
            Self::InvalidCredentials | Self::SessionExpired | Self::InvalidRefreshToken => {
                Category::Unauthenticated
            }
            Self::Limit(error) => error.category(),
            Self::Request(error) => error.category(),
            Self::Internal(error) => error.category(),
        }
    }

    fn detail(&self) -> Option<String> {
        match self {
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
}
