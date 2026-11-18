//! The domain types of `client_telemetry`.

use bytes::Bytes;
use platform::{
    rate_limit::Bucket,
    telemetry::Signal,
    types::{Timestamp, Version},
};
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

/// The budgets of the batches that the proxy forwards (the client_telemetry spec's "The
/// budgets").
#[derive(Clone, Copy, Debug)]
pub(crate) struct Settings {
    /// How many bytes of batches each user's clients may send in each window
    /// (CLIENT_TELEMETRY-007).
    pub(crate) user_budget: Bucket,
    /// How many bytes of batches each replica forwards in each window (CLIENT_TELEMETRY-008).
    pub(crate) replica_budget: Bucket,
}

/// The most bytes of a batch, as sent and once inflated (CLIENT_TELEMETRY-006), and so the least
/// that the settings take for a budget, whose window then holds any batch.
pub(crate) const MAX_BATCH_BYTES: u32 = 1 << 20;

/// A batch that a client sent the proxy, as its route read it (CLIENT_TELEMETRY-004).
#[derive(Clone, Debug)]
pub(super) struct Sent {
    /// The signal that the batch holds, which its route says.
    pub(super) signal: Signal,
    /// The media type of the bytes, one of the protocol's two.
    pub(super) media_type: &'static str,
    /// The batch's bytes, inflated if they came as gzip.
    pub(super) bytes: Bytes,
}
