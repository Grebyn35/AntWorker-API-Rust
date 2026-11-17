//! Each user's consent to the telemetry of their clients: reading it, and choosing it (the
//! client_telemetry spec's "The consent").

use platform::{
    authn::Principal,
    db::Database,
    errors::RequestError,
    types::{Client, Version},
};

use super::{
    audit::{self, TelemetryConsentChoice},
    error::ClientTelemetryError,
    events::{TELEMETRY_CONSENT_UPDATED, TelemetryConsentUpdated},
    model::{Current, TelemetryConsent},
    repo,
};
use crate::modules::audit::{self as log, Entry};

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
        version: Version::from_database(0),
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
