//! The caller's consent to the telemetry of their clients (the client_telemetry spec's Routes).

use axum::extract::State;
use platform::{
    authn::Principal,
    db::Database,
    errors::Problem,
    http::{IfMatch, Json, ResourceResponse, Routes},
    types::Client,
};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::routes;

use super::{
    error::ClientTelemetryError, events::TELEMETRY_CONSENT_UPDATED, model::TelemetryConsent,
    service,
};

/// The module's routes, which take the database as their state, and the event they publish.
pub(crate) fn routes() -> Routes<Database> {
    Routes::new()
        .events(&[&TELEMETRY_CONSENT_UPDATED])
        .user(routes!(consent, choose_consent), &[])
}

/// Reads whether the caller consents to their clients' sending their telemetry.
#[utoipa::path(
    get,
    path = "/v1/users/me/telemetry-consent",
    tag = "client_telemetry",
    responses(
        (status = 200, description = "The caller's consent.", body = TelemetryConsent,
            headers(("etag" = String, description = "The consent's version, which a choice's `If-Match` names (API-13)."))),
    )
)]
async fn consent(
    State(database): State<Database>,
    client: Client,
    principal: Principal,
) -> Result<ResourceResponse<TelemetryConsent>, Problem<ClientTelemetryError>> {
    let current = service::consent(&database, &client, &principal).await?;
    Ok(ResourceResponse::ok(current.consent).tagged(current.version))
}

/// What choosing the consent takes: the whole consent (CLIENT_TELEMETRY-002).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct TelemetryConsentRequest {
    /// Whether the caller consents to their clients' sending their telemetry.
    consented: bool,
}

/// Stores whether the caller consents to their clients' sending their telemetry.
#[utoipa::path(
    put,
    path = "/v1/users/me/telemetry-consent",
    tag = "client_telemetry",
    request_body = TelemetryConsentRequest,
    params(
        ("If-Match" = Option<String>, Header,
            description = "The `ETag` of the version the client replaces, so that a choice made since it read the consent fails rather than being overwritten (API-13)."),
    ),
    responses(
        (status = 200, description = "The caller's consent, as they chose it.", body = TelemetryConsent,
            headers(("etag" = String, description = "The consent's new version (API-13)."))),
    )
)]
async fn choose_consent(
    State(database): State<Database>,
    client: Client,
    principal: Principal,
    if_match: IfMatch,
    Json(request): Json<TelemetryConsentRequest>,
) -> Result<ResourceResponse<TelemetryConsent>, Problem<ClientTelemetryError>> {
    let current = service::choose(
        &database,
        &client,
        &principal,
        request.consented,
        if_match.versions(),
    )
    .await?;
    Ok(ResourceResponse::ok(current.consent).tagged(current.version))
}
