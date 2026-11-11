//! Confirming the request of an export of the caller's data, which `accounts` then builds
//! (ACCOUNTS-020, D137).

use platform::{authn::Principal, rate_limit::RateLimits, types::Client};

use super::{Auth, reauthentication::require_recent_proof};
use crate::modules::{
    accounts::{Export, Exports},
    auth::error::AuthError,
};

impl Auth {
    /// Requests an export of the caller's data through `exports`, once the session that
    /// authenticates them has proved who they are within the last 5 minutes. The proof is read
    /// before the export's bucket counts the request, in a unit of work of its own that writes
    /// nothing, since it decides whether the bucket counts it: a request without it costs the
    /// user nothing of their one export a day (ACCOUNTS-020, ACCOUNTS-021, ARCH-11).
    pub(crate) async fn request_export(
        &self,
        exports: &Exports,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
    ) -> Result<Export, AuthError> {
        let mut read = self.database.begin(client).await?;
        require_recent_proof(&mut read, principal).await?;
        drop(read);
        Ok(exports.request(limits, client, principal).await?)
    }
}
