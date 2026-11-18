//! Each user's consent to the telemetry of their clients: reading it, and choosing it (the
//! client_telemetry spec's "The consent"); and the proxy, which forwards a consenting user's
//! batches to the collector (the spec's "The proxy").

use platform::{
    authn::Principal,
    blocking::Workers,
    db::Database,
    errors::RequestError,
    rate_limit::RateLimits,
    telemetry::{Relay, RelayError},
    types::{Client, Version},
};

use super::{
    audit::{self, TelemetryConsentChoice},
    error::ClientTelemetryError,
    events::{TELEMETRY_CONSENT_UPDATED, TelemetryConsentUpdated},
    model::{Current, Sent, Settings, TelemetryConsent},
    repo,
};
use crate::modules::audit::{self as log, Entry};

/// What the routes of `client_telemetry` share: the database that holds each consent, the turns
/// of the work that inflates a batch, and the proxy.
#[derive(Clone)]
pub(crate) struct ClientTelemetry {
    pub(super) database: Database,
    pub(super) workers: Workers,
    /// The proxy, or `None` while the deployment forwards no client's telemetry
    /// (CLIENT_TELEMETRY-010).
    proxy: Option<Proxy>,
}

/// What the proxy forwards a batch with: the relay to the collector, the two budgets that count
/// the batch's bytes first, and what the replica's budget counts for.
#[derive(Clone)]
pub(crate) struct Proxy {
    relay: Relay,
    budgets: Settings,
    /// The ID that this replica drew as it started, which its budget counts for
    /// (CLIENT_TELEMETRY-008).
    replica: [u8; 16],
}

impl Proxy {
    /// The proxy that forwards through `relay`, once `budgets` have counted a batch's bytes: the
    /// user's for the batch's caller, and then the replica's for `replica`, an ID that this
    /// replica alone holds.
    pub(crate) fn new(relay: Relay, budgets: Settings, replica: [u8; 16]) -> Self {
        Self {
            relay,
            budgets,
            replica,
        }
    }
}

impl ClientTelemetry {
    /// The module's state, on `database`, with `proxy` if the deployment forwards its clients'
    /// telemetry.
    pub(crate) fn new(database: Database, workers: Workers, proxy: Option<Proxy>) -> Self {
        Self {
            database,
            workers,
            proxy,
        }
    }
}

/// The caller's consent, read by their own ID: `false` at version 0 for a caller who never chose
/// (CLIENT_TELEMETRY-001).
pub(super) async fn consent(
    database: &Database,
    client: &Client,
    principal: &Principal,
) -> Result<Current, ClientTelemetryError> {
    let mut uow = database.begin(client).await?;
    let current = repo::current(&mut uow, principal.user_id()).await?;
    Ok(current.unwrap_or(Current {
        consent: TelemetryConsent { consented: false },
        version: Version::UNWRITTEN,
    }))
}

/// Stores `consented` as the caller's consent, in a unit of work of `client`'s request, when its
/// version is one of `versions`, or whatever it is when there are none, and answers the consent
/// as it then is: the update, its audit entry in the caller's own log and its event commit
/// together (CLIENT_TELEMETRY-002, PLATFORM-071).
pub(super) async fn choose(
    database: &Database,
    client: &Client,
    principal: &Principal,
    consented: bool,
    versions: Option<&[Version]>,
) -> Result<Current, ClientTelemetryError> {
    let mut uow = database.begin(client).await?;
    let user = principal.user_id();
    // The row that the update changes, which a caller who never chose has once the first
    // statement inserts it, locked before the update reads its version, so that a deletion of
    // the account that committed first refuses the request as one sent after it (DATA-20).
    if !repo::insert_unchosen(&mut uow, user).await? || !repo::lock(&mut uow, user).await? {
        return Err(RequestError::Unauthenticated.into());
    }
    let Some(current) = repo::set(&mut uow, user, consented, versions).await? else {
        return Err(RequestError::PreconditionFailed.into());
    };
    let entry = Entry::detailed(
        audit::TELEMETRY_CONSENT_UPDATED,
        audit::USER,
        user.uuid(),
        TelemetryConsentChoice { consented },
    );
    log::record(&mut uow, principal.audit_scope(), entry).await?;
    uow.publish_to_user(
        &TELEMETRY_CONSENT_UPDATED,
        user,
        &TelemetryConsentUpdated { consented },
    )
    .await?;
    uow.commit().await?;
    Ok(current)
}

/// Forwards `batch`, which one of the caller's clients sent, to the collector, and says how the
/// collector took it (CLIENT_TELEMETRY-004, CLIENT_TELEMETRY-009): once the deployment forwards
/// clients' telemetry at all (CLIENT_TELEMETRY-010), the caller consents, as their consent reads
/// now (CLIENT_TELEMETRY-005), and the caller's budget, then the replica's, have counted the
/// batch's bytes (CLIENT_TELEMETRY-007, CLIENT_TELEMETRY-008).
pub(super) async fn forward(
    telemetry: &ClientTelemetry,
    client: &Client,
    principal: &Principal,
    limits: &RateLimits,
    batch: Sent,
) -> Result<(), ClientTelemetryError> {
    let Some(proxy) = &telemetry.proxy else {
        return Err(RequestError::FeatureDisabled.into());
    };
    let current = consent(&telemetry.database, client, principal).await?;
    if !current.consent.consented {
        return Err(ClientTelemetryError::ConsentRequired);
    }
    // A batch holds at most a mebibyte, which a `u32` holds.
    let bytes = u32::try_from(batch.bytes.len()).unwrap_or(u32::MAX);
    let user = principal.user_id();
    limits
        .spend(proxy.budgets.user_budget, user.uuid().as_bytes(), bytes)
        .await?;
    limits
        .spend(proxy.budgets.replica_budget, &proxy.replica, bytes)
        .await?;
    proxy
        .relay
        .forward(batch.signal, batch.media_type, batch.bytes)
        .await
        .map_err(|error| match error {
            RelayError::Refused => ClientTelemetryError::BatchRefused,
            RelayError::Unavailable(cause) => {
                ClientTelemetryError::CollectorUnavailable(Some(cause))
            }
        })
}
