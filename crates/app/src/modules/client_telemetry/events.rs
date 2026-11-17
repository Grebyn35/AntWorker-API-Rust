//! The events `client_telemetry` publishes, each with its payload, which the event catalog
//! documents (API-19, REALTIME-008).
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its events here (the platform spec's events)"
)]

use platform::events::UserEvent;
use serde::Serialize;
use utoipa::ToSchema;

/// The user chose whether their clients send their telemetry, sent to their own streams, so each
/// of their clients follows the choice at once (CLIENT_TELEMETRY-002).
pub(super) const TELEMETRY_CONSENT_UPDATED: UserEvent<TelemetryConsentUpdated> =
    UserEvent::new("telemetry_consent.updated");

/// The payload of `telemetry_consent.updated`: the consent as the user chose it, which reaches
/// only their own streams (API-19).
#[derive(Debug, Serialize, ToSchema)]
pub(super) struct TelemetryConsentUpdated {
    /// Whether the user consents to their clients' sending their telemetry.
    pub(super) consented: bool,
}
