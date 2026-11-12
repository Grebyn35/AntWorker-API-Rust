//! The tests' stand-in provider, which only the tests' build compiles, so the tests of the
//! behaviors that every provider shares hold in a fork that removes every provider (the auth
//! spec's "Signing in through a provider"). Its endpoints, which the configured client sends to
//! the tests' origin (SEC-10), answer a token for a code, and the identity for a token: its
//! subject, its address, whether it proves the address, and its name.

use futures_util::future::BoxFuture;
use platform::{
    outbound::{Configured, Request},
    types::Secret,
};
use serde::Deserialize;

use super::{Client, Entry, Identity, Provider, ProviderError};

/// The stand-in in the registry, with two clients, whose settings start with `STAND_IN_` and
/// `STAND_IN_SECOND_`.
pub(super) const ENTRY: Entry = Entry {
    id: "stand_in",
    clients: &["STAND_IN_", "STAND_IN_SECOND_"],
    build: |clients| Box::new(StandIn { clients }),
};

const IDENTITY_ENDPOINT: &str = "https://provider.test/identity";

struct StandIn {
    clients: Vec<Client>,
}

#[derive(Deserialize)]
struct Answer {
    subject: String,
    email: Option<String>,
    proved: bool,
    name: Option<String>,
}

impl Provider for StandIn {
    fn id(&self) -> &'static str {
        ENTRY.id
    }

    fn name(&self) -> &'static str {
        "Stand-in"
    }

    fn authorization_endpoint(&self) -> &'static str {
        "https://provider.test/authorize"
    }

    fn scopes(&self) -> &'static [&'static str] {
        &["identity"]
    }

    fn clients(&self) -> &[Client] {
        &self.clients
    }

    fn token_endpoint(&self) -> &'static str {
        "https://provider.test/token"
    }

    fn identify<'a>(
        &'a self,
        outbound: &'a Configured,
        _client: &'a Client,
        token: &'a Secret<String>,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>> {
        Box::pin(async move {
            let answer = outbound
                .send(Request::get(IDENTITY_ENDPOINT).bearer(Secret::new(token.expose().clone())))
                .await
                .map_err(|error| ProviderError::Unavailable(error.to_string()))?;
            let identity: Answer = answer
                .is_success()
                .then(|| answer.json())
                .flatten()
                .ok_or_else(|| {
                    ProviderError::Unavailable(format!(
                        "the identity endpoint answered {}",
                        answer.status
                    ))
                })?;
            Ok(Identity {
                subject: identity.subject,
                email: identity.email,
                proved: identity.proved,
                name: identity.name,
            })
        })
    }
}
