//! A device's name, and a session as the list of a user's sessions shows it (AUTH-042, AUTH-043).

use std::borrow::Cow;

use platform::types::{Invalid, SessionId, Timestamp, one_line};
use serde::Serialize;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{ObjectBuilder, RefOr, Schema, Type},
};

use super::{DeviceId, StoredSession};

/// The most characters a device's name has (AUTH-042).
const DEVICE_NAME_MAX_CHARS: usize = 100;

/// The name a client gives its device, such as `Ada's laptop`: one line of 1 to 100 characters,
/// normalized as a user's name is (AUTH-042). It is personal data, so its `Debug` shows nothing
/// of it (OBS-04).
#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(crate) struct DeviceName(String);

impl std::fmt::Debug for DeviceName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceName([redacted])")
    }
}

impl DeviceName {
    /// The device's name in `value`, normalized, or why it isn't one.
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        one_line(
            value,
            DEVICE_NAME_MAX_CHARS,
            "Enter a name for the device, or leave it out.",
            "A device's name has at most 100 characters.",
        )
        .map(Self)
    }

    /// A name the database holds, which was normalized when it was stored.
    pub(in crate::modules::auth) fn from_stored(value: String) -> Self {
        Self(value)
    }

    /// The normalized name.
    pub(in crate::modules::auth) fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialSchema for DeviceName {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .min_length(Some(1))
            .max_length(Some(DEVICE_NAME_MAX_CHARS))
            .into()
    }
}

impl ToSchema for DeviceName {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("DeviceName")
    }
}

/// One of the user's sessions, as their list shows it (AUTH-043).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub(crate) struct SessionItem {
    /// The session's ID.
    pub(crate) id: SessionId,
    /// The device ID its client signed in with, if any.
    pub(crate) device_id: Option<DeviceId>,
    /// The name its client gave its device, if any.
    pub(crate) device_name: Option<DeviceName>,
    /// The provider whose sign-in started it, by its key, or null for a password
    /// (AUTH-016).
    pub(crate) provider: Option<String>,
    /// When the session began.
    pub(crate) created_at: Timestamp,
    /// When its access token expires.
    pub(crate) access_token_expires_at: Timestamp,
    /// When its refresh token expires, unless a refresh slides it.
    pub(crate) refresh_token_expires_at: Timestamp,
    /// When the session ends, whatever its refreshes.
    pub(crate) expires_at: Timestamp,
    /// When its user last proved who they are in it, their password or their account at a provider, and the second factor of an
    /// account that has one: when it began, re-authenticated, or changed the password (AUTH-021,
    /// AUTH-040, AUTH-065).
    pub(crate) authenticated_at: Timestamp,
    /// Whether it is the session that authenticated the request.
    pub(crate) current: bool,
}

impl StoredSession {
    /// The session as the list of its user's sessions shows it to the session `current`.
    pub(crate) fn item(self, current: SessionId) -> SessionItem {
        SessionItem {
            current: self.id == current,
            id: self.id,
            device_id: self.device_id,
            device_name: self.device_name,
            provider: self.provider,
            created_at: self.created_at,
            access_token_expires_at: self.access_token_expires_at,
            refresh_token_expires_at: self.refresh_token_expires_at,
            expires_at: self.expires_at,
            authenticated_at: self.authenticated_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_042_a_device_s_name_is_one_normalized_line_of_1_to_100_characters() {
        let name = DeviceName::parse("  Ada's \t laptop \n").unwrap();
        assert_eq!(name.as_str(), "Ada's laptop");
        // An e and a combining acute accent compose into one é.
        let composed = DeviceName::parse("Re\u{301}my's phone").unwrap();
        assert_eq!(composed.as_str(), "R\u{e9}my's phone");
        assert_eq!(DeviceName::parse(" \t ").unwrap_err().code, "required");
        assert!(DeviceName::parse(&"é".repeat(100)).is_ok());
        assert_eq!(
            DeviceName::parse(&"é".repeat(101)).unwrap_err().code,
            "too_long"
        );
    }
}
