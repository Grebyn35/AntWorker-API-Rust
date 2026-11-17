//! The domain types of `client_telemetry`.

use platform::types::{Timestamp, Version};
use serde::Serialize;
use utoipa::ToSchema;

/// A user's consent to the telemetry of their clients, as `GET /v1/users/me/telemetry-consent`
/// answers it (CLIENT_TELEMETRY-001).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub(crate) struct TelemetryConsent {
    /// Whether the user consents to their clients' sending their telemetry: `false` until they
    /// choose to.
    pub(crate) consented: bool,
}

/// The consent as it stands, with its version, which its `ETag` names (PLATFORM-071).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Current {
    /// The consent.
    pub(crate) consent: TelemetryConsent,
    /// Its version: 0 for a user who never chose (the client_telemetry spec's "Its version").
    pub(crate) version: Version,
}

/// A user's row of `telemetry_consents`, as their export holds it (CLIENT_TELEMETRY-003).
#[derive(Debug, Serialize)]
pub(super) struct Exported {
    /// Whether the user consents.
    pub(super) consented: bool,
    /// When the user first chose.
    pub(super) created_at: Timestamp,
    /// When the user last chose.
    pub(super) updated_at: Timestamp,
}
