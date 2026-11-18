//! The caller's consent to the telemetry of their clients, and the proxy's three routes, which
//! take a consenting caller's batches (the client_telemetry spec's Routes).

use axum::extract::State;
use platform::{
    authn::Principal,
    errors::{Problem, RequestError},
    http::{
        Batch, BatchJson, BatchTaken, BatchTakenJson, FileBytes, IfMatch, Json, ReadBatch,
        ResourceResponse, Routes,
    },
    rate_limit::{LimitError, RateLimits},
    telemetry::Signal,
    types::Client,
};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::routes;

use super::{
    error::ClientTelemetryError,
    events::TELEMETRY_CONSENT_UPDATED,
    model::{MAX_BATCH_BYTES, Sent, TelemetryConsent},
    service::{self, ClientTelemetry},
};

/// The module's routes, which take [`ClientTelemetry`] as their state, and the event they
/// publish.
pub(crate) fn routes() -> Routes<ClientTelemetry> {
    let proxy_errors: [&dyn platform::errors::ErrorCode; 5] = [
        &ClientTelemetryError::ConsentRequired,
        &ClientTelemetryError::BatchRefused,
        &ClientTelemetryError::CollectorUnavailable(None),
        &LimitError::RateLimited,
        &RequestError::FeatureDisabled,
    ];
    Routes::new()
        .events(&[&TELEMETRY_CONSENT_UPDATED])
        .user(routes!(consent, choose_consent), &[])
        .user_batch(routes!(send_traces), &proxy_errors)
        .user_batch(routes!(send_logs), &proxy_errors)
        .user_batch(routes!(send_metrics), &proxy_errors)
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
    State(telemetry): State<ClientTelemetry>,
    client: Client,
    principal: Principal,
) -> Result<ResourceResponse<TelemetryConsent>, Problem<ClientTelemetryError>> {
    let current = service::consent(&telemetry.database, &client, &principal).await?;
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
    State(telemetry): State<ClientTelemetry>,
    client: Client,
    principal: Principal,
    if_match: IfMatch,
    Json(request): Json<TelemetryConsentRequest>,
) -> Result<ResourceResponse<TelemetryConsent>, Problem<ClientTelemetryError>> {
    let current = service::choose(
        &telemetry.database,
        &client,
        &principal,
        request.consented,
        if_match.versions(),
    )
    .await?;
    Ok(ResourceResponse::ok(current.consent).tagged(current.version))
}

/// Takes a batch of the spans of one of the caller's clients, and forwards it to the collector.
#[utoipa::path(
    post,
    path = "/v1/users/me/telemetry/traces",
    tag = "client_telemetry",
    request_body(
        description = "A batch of spans, as the request of the OpenTelemetry Protocol over HTTP (OTLP/HTTP): an `ExportTraceServiceRequest`, sent as `application/x-protobuf` or as `application/json`, with `Content-Encoding: gzip` or without one, of at most 1,048,576 bytes, as sent and once inflated (CLIENT_TELEMETRY-004, CLIENT_TELEMETRY-006).",
        content(
            (FileBytes = "application/x-protobuf"),
            (BatchJson = "application/json"),
        ),
    ),
    responses(
        (status = 200, description = "The collector took the batch: the protocol's full success, in the media type of the request, which holds no bytes as `application/x-protobuf`.",
            content(
                (FileBytes = "application/x-protobuf"),
                (BatchTakenJson = "application/json"),
            )),
    )
)]
async fn send_traces(
    State(telemetry): State<ClientTelemetry>,
    client: Client,
    principal: Principal,
    limits: RateLimits,
    batch: Batch,
) -> Result<BatchTaken, Problem<ClientTelemetryError>> {
    // The turn the bytes hold lasts until `batch` is dropped, once the collector has answered
    // (PLATFORM-090).
    let batch = read(batch, &telemetry, &principal).await?;
    let sent = Sent {
        signal: Signal::Traces,
        media_type: batch.encoding.media_type(),
        bytes: batch.bytes.clone(),
    };
    service::forward(&telemetry, &client, &principal, &limits, sent).await?;
    Ok(BatchTaken(batch.encoding))
}

/// Takes a batch of the log records of one of the caller's clients, and forwards it to the
/// collector.
#[utoipa::path(
    post,
    path = "/v1/users/me/telemetry/logs",
    tag = "client_telemetry",
    request_body(
        description = "A batch of log records, as the request of the OpenTelemetry Protocol over HTTP (OTLP/HTTP): an `ExportLogsServiceRequest`, sent as `application/x-protobuf` or as `application/json`, with `Content-Encoding: gzip` or without one, of at most 1,048,576 bytes, as sent and once inflated (CLIENT_TELEMETRY-004, CLIENT_TELEMETRY-006).",
        content(
            (FileBytes = "application/x-protobuf"),
            (BatchJson = "application/json"),
        ),
    ),
    responses(
        (status = 200, description = "The collector took the batch: the protocol's full success, in the media type of the request, which holds no bytes as `application/x-protobuf`.",
            content(
                (FileBytes = "application/x-protobuf"),
                (BatchTakenJson = "application/json"),
            )),
    )
)]
async fn send_logs(
    State(telemetry): State<ClientTelemetry>,
    client: Client,
    principal: Principal,
    limits: RateLimits,
    batch: Batch,
) -> Result<BatchTaken, Problem<ClientTelemetryError>> {
    // The turn the bytes hold lasts until `batch` is dropped, once the collector has answered
    // (PLATFORM-090).
    let batch = read(batch, &telemetry, &principal).await?;
    let sent = Sent {
        signal: Signal::Logs,
        media_type: batch.encoding.media_type(),
        bytes: batch.bytes.clone(),
    };
    service::forward(&telemetry, &client, &principal, &limits, sent).await?;
    Ok(BatchTaken(batch.encoding))
}

/// Takes a batch of the metrics of one of the caller's clients, and forwards it to the collector.
#[utoipa::path(
    post,
    path = "/v1/users/me/telemetry/metrics",
    tag = "client_telemetry",
    request_body(
        description = "A batch of metrics, as the request of the OpenTelemetry Protocol over HTTP (OTLP/HTTP): an `ExportMetricsServiceRequest`, sent as `application/x-protobuf` or as `application/json`, with `Content-Encoding: gzip` or without one, of at most 1,048,576 bytes, as sent and once inflated (CLIENT_TELEMETRY-004, CLIENT_TELEMETRY-006).",
        content(
            (FileBytes = "application/x-protobuf"),
            (BatchJson = "application/json"),
        ),
    ),
    responses(
        (status = 200, description = "The collector took the batch: the protocol's full success, in the media type of the request, which holds no bytes as `application/x-protobuf`.",
            content(
                (FileBytes = "application/x-protobuf"),
                (BatchTakenJson = "application/json"),
            )),
    )
)]
async fn send_metrics(
    State(telemetry): State<ClientTelemetry>,
    client: Client,
    principal: Principal,
    limits: RateLimits,
    batch: Batch,
) -> Result<BatchTaken, Problem<ClientTelemetryError>> {
    // The turn the bytes hold lasts until `batch` is dropped, once the collector has answered
    // (PLATFORM-090).
    let batch = read(batch, &telemetry, &principal).await?;
    let sent = Sent {
        signal: Signal::Metrics,
        media_type: batch.encoding.media_type(),
        bytes: batch.bytes.clone(),
    };
    service::forward(&telemetry, &client, &principal, &limits, sent).await?;
    Ok(BatchTaken(batch.encoding))
}

/// Reads `batch`, which `principal`'s user sends, up to the largest batch's bytes, inflating it
/// in one of `telemetry`'s turns of CPU-heavy work if it came as gzip (CLIENT_TELEMETRY-006).
async fn read(
    batch: Batch,
    telemetry: &ClientTelemetry,
    principal: &Principal,
) -> Result<ReadBatch, ClientTelemetryError> {
    let limit = usize::try_from(MAX_BATCH_BYTES).unwrap_or(usize::MAX);
    Ok(batch
        .read(principal.user_id(), limit, &telemetry.workers)
        .await?)
}
