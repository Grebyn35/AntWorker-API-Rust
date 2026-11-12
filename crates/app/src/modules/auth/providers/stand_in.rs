//! The tests' stand-in provider, which only the tests' build compiles, so the tests of the
//! behaviors that every provider shares hold in a fork that removes every provider (the auth
//! spec's "Signing in through a provider"). It follows OpenID Connect, so it is a declaration:
//! its endpoints, which the configured client sends to the tests' origin (SEC-10), answer a token
//! for a code, and, for a token, the claims of the user, whose `proved` says whether it proves
//! the address. It reads its name from a setting, so the tests prove what the registry does with
//! a setting that a provider reads beside its clients' (AUTH-081).

use platform::config::ConfigError;

use super::{Built, Declared, Entry, Provider};

const NAME: &str = "STAND_IN_NAME";

/// The stand-in in the registry, with two clients, whose settings start with `STAND_IN_` and
/// `STAND_IN_SECOND_`, and its name.
pub(super) const ENTRY: Entry = Entry {
    key: "stand_in",
    clients: &["STAND_IN_", "STAND_IN_SECOND_"],
    settings: &[NAME],
    build,
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
            token_endpoint: "https://provider.test/token",
            identity_endpoint: "https://provider.test/identity",
            scopes: &["identity"],
        }
    }

    fn proves(&self, claims: &serde_json::Value) -> bool {
        claims.get("proved").and_then(serde_json::Value::as_bool) == Some(true)
    }
}
