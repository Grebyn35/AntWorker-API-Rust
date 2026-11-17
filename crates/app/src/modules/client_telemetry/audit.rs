//! The audit actions `client_telemetry` records, the kind of resource they happen to, and their
//! details (ARCH-03, the audit spec's "The action catalog").
#![expect(
    clippy::disallowed_methods,
    reason = "a module declares its audit actions and resources here (the audit spec)"
)]

use serde::Serialize;
use utoipa::ToSchema;

use crate::modules::audit::{Action, Catalogued, Resource};

/// The user chose whether their clients send their telemetry, recorded in their own log
/// (CLIENT_TELEMETRY-002).
pub(super) const TELEMETRY_CONSENT_UPDATED: Action<TelemetryConsentChoice> = Action::new(
    "telemetry_consent.updated",
    "The user chose whether their clients send their telemetry.",
    &[USER],
)
.in_own_log();

/// A user, whose consent it is.
pub(super) const USER: Resource = Resource::new("user");

/// Every action `client_telemetry` records, which the action catalog lists (AUDIT-012).
pub(crate) const ACTIONS: &[&dyn Catalogued] = &[&TELEMETRY_CONSENT_UPDATED];

/// The details of `telemetry_consent.updated`: what the user chose.
#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
pub(super) struct TelemetryConsentChoice {
    /// Whether the user consents to their clients' sending their telemetry.
    pub(super) consented: bool,
}
