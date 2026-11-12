//! The OAuth providers as the capabilities endpoint lists them, from the registry of `auth`,
//! which `system` may not name (ARCH-05).

use crate::modules::{auth, system};

/// The providers that `providers` configures, as the capabilities endpoint lists them
/// (AUTH-081).
pub(super) fn sign_in_providers(providers: &auth::Providers) -> Vec<system::SignInProvider> {
    providers
        .listed()
        .into_iter()
        .map(|listed| system::SignInProvider {
            provider: listed.provider,
            name: listed.name,
            authorization_endpoint: listed.authorization_endpoint,
            scopes: listed.scopes,
            clients: listed
                .clients
                .into_iter()
                .map(|(client_id, redirect_uris)| system::SignInClient {
                    client_id,
                    redirect_uris,
                })
                .collect(),
        })
        .collect()
}
