//! The tests' stand-in provider, which only the tests' build compiles, so the tests of the
//! behaviors that every provider shares hold in a fork that keeps no provider (the auth
//! spec's "Signing in through a provider"). It follows OpenID Connect, so it is a declaration:
//! its endpoints, which the configured client sends to the tests' origin (SEC-10), answer a token
//! for a code, and, for a token, the claims of the user, whose `proved` says whether it proves
//! the address. It reads its name from a setting, so the tests prove what the registry does with
//! a setting that a provider reads beside its clients' (AUTH-081). The tokens of its second
//! client don't lapse, as those of a provider's client of another kind: the stand-in deletes
//! one, so the tests prove what the registry does with a deletion (AUTH-079).

use futures_util::future::BoxFuture;
use platform::{
    config::ConfigError,
    outbound::{Configured, Request},
    types::Secret,
};

use super::{Built, Client, Declared, Entry, Provider};

const NAME: &str = "STAND_IN_NAME";

/// The prefix of the settings of the client whose tokens don't lapse.
const SECOND: &str = "STAND_IN_SECOND_";

/// The endpoint that gives a token for a code, and deletes a token of the second client.
const TOKEN: &str = "https://provider.test/token";

/// The stand-in in the registry, with two clients, whose settings start with `STAND_IN_` and
/// `STAND_IN_SECOND_`, and its name.
pub(super) const ENTRY: Entry = Entry {
    key: "stand_in",
    clients: &["STAND_IN_", SECOND],
    settings: &[NAME],
    build,
    // part: files
    pictures: super::pictures::Pictures {
        hosts: &[],
        setting: Some("STAND_IN_PICTURE_HOSTS"),
    },
    // end part: files
};

struct StandIn {
    name: String,
}

/// The stand-in whose name `settings` holds, the value of the entry's one setting.
fn build(settings: &[String]) -> Built {
    let [name] = settings else {
        return Err(ConfigError::missing(NAME));
    };
    Ok(Box::new(StandIn { name: name.clone() }))
}

impl Provider for StandIn {
    fn declared(&self) -> Declared<'_> {
        Declared {
            name: &self.name,
            authorization_endpoint: "https://provider.test/authorize",
            token_endpoint: TOKEN,
            identity_endpoint: "https://provider.test/identity",
            scopes: &["identity"],
        }
    }

    fn proves(&self, claims: &serde_json::Value) -> bool {
        claims.get("proved").and_then(serde_json::Value::as_bool) == Some(true)
    }

    /// Deletes a token of the second client, with a `DELETE` of the token endpoint that the
    /// token authenticates, which answers 204; a token of the first lapses.
    fn discard<'a>(
        &'a self,
        outbound: &'a Configured,
        client: &'a Client,
        token: &'a Secret<String>,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if !client.setting.starts_with(SECOND) {
                return Ok(());
            }
            let request = Request::delete(TOKEN).bearer(Secret::new(token.expose().clone()));
            match outbound.send(request).await {
                Ok(answer) if answer.status == 204 => Ok(()),
                Ok(answer) => Err(format!("DELETE of the token answered {}", answer.status)),
                Err(error) => Err(error.to_string()),
            }
        })
    }
}
