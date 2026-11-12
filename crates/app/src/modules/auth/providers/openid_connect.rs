//! An OpenID Connect provider that the deployment names in its settings, such as an
//! organization's own (AUTH-084, D142, the auth spec's "Signing in through a provider"). Its
//! addresses differ between deployments, so they are settings, where another provider's are
//! constants of its file (OPS-02).

use platform::{
    config::ConfigError,
    outbound::Response,
    types::{Secret, one_line},
};
use serde::Deserialize;

use super::{Built, Declared, Entry, Provider, ProviderError};

const NAME: &str = "OPENID_CONNECT_NAME";
const ISSUER: &str = "OPENID_CONNECT_ISSUER";
const AUTHORIZATION_ENDPOINT: &str = "OPENID_CONNECT_AUTHORIZATION_ENDPOINT";
const TOKEN_ENDPOINT: &str = "OPENID_CONNECT_TOKEN_ENDPOINT";
const USERINFO_ENDPOINT: &str = "OPENID_CONNECT_USERINFO_ENDPOINT";

/// The provider in the registry, with one client, and the settings that name it: the name that
/// a client shows, its issuer, and the addresses of its endpoints (AUTH-084).
pub(super) const ENTRY: Entry = Entry {
    key: "openid_connect",
    clients: &["OPENID_CONNECT_"],
    settings: &[
        NAME,
        ISSUER,
        AUTHORIZATION_ENDPOINT,
        TOKEN_ENDPOINT,
        USERINFO_ENDPOINT,
    ],
    build,
};

/// The longest the provider's name is, in characters.
const NAME_MAX_CHARS: usize = 100;

const NAME_EXPECTED: &str = "a name of 1 to 100 characters (AUTH-084)";
const ISSUER_EXPECTED: &str = "an https URL without a query or a fragment (AUTH-084)";
const ENDPOINT_EXPECTED: &str = "an https URL without a fragment (AUTH-084)";

/// The provider that the settings name.
struct OpenIdConnect {
    name: String,
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    userinfo_endpoint: String,
}

/// The provider that `settings` name, the values of the entry's settings in their order, or the
/// setting whose value it refuses (AUTH-084).
fn build(settings: &[String]) -> Built {
    // The registry gives a value for each setting of the entry.
    let [
        name,
        issuer,
        authorization_endpoint,
        token_endpoint,
        userinfo_endpoint,
    ] = settings
    else {
        return Err(ConfigError::missing(NAME));
    };
    let name = one_line(name, NAME_MAX_CHARS, NAME_EXPECTED, NAME_EXPECTED)
        .map_err(|_| ConfigError::invalid(NAME, NAME_EXPECTED))?;
    if !https(issuer, false) {
        return Err(ConfigError::invalid(ISSUER, ISSUER_EXPECTED));
    }
    let endpoint = |setting: &str, address: &String| {
        https(address, true)
            .then(|| address.clone())
            .ok_or_else(|| ConfigError::invalid(setting, ENDPOINT_EXPECTED))
    };
    Ok(Box::new(OpenIdConnect {
        name,
        issuer: issuer.clone(),
        authorization_endpoint: endpoint(AUTHORIZATION_ENDPOINT, authorization_endpoint)?,
        token_endpoint: endpoint(TOKEN_ENDPOINT, token_endpoint)?,
        userinfo_endpoint: endpoint(USERINFO_ENDPOINT, userinfo_endpoint)?,
    }))
}

/// Whether `address` is an `https` URL with a host, no fragment, no white space, and, unless
/// `query` allows one, no query.
fn https(address: &str, query: bool) -> bool {
    !address.contains(char::is_whitespace)
        && url::Url::parse(address).is_ok_and(|url| {
            url.scheme() == "https"
                && url.host().is_some()
                && url.fragment().is_none()
                && (query || url.query().is_none())
        })
}

impl Provider for OpenIdConnect {
    fn declared(&self) -> Declared<'_> {
        Declared {
            name: &self.name,
            authorization_endpoint: &self.authorization_endpoint,
            token_endpoint: &self.token_endpoint,
            identity_endpoint: &self.userinfo_endpoint,
            scopes: &["openid", "email", "profile"],
        }
    }

    /// OpenID Connect makes the Basic scheme a client's default (Core, §9), so every provider
    /// that follows it takes it.
    fn basic(&self) -> bool {
        true
    }

    /// The access token of the token endpoint's answer, when the answer says that it lapses, by
    /// an `expires_in` above zero (RFC 6749, §5.1): the API knows no way of such a provider's to
    /// delete a token, so it takes none that may not lapse (AUTH-079, D139).
    fn token(&self, answer: &Response) -> Result<Secret<String>, ProviderError> {
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
            expires_in: Option<u64>,
        }
        match answer.json::<Token>() {
            Some(Token {
                access_token,
                expires_in: Some(1..),
            }) => Ok(Secret::new(access_token)),
            Some(_) => Err(ProviderError::Unavailable(
                "the token endpoint's answer doesn't say when its token lapses".to_owned(),
            )),
            None => Err(ProviderError::Unavailable(
                "the token endpoint's answer holds no token".to_owned(),
            )),
        }
    }

    /// The issuer and the `sub`, since OpenID Connect names a user by the two together (Core,
    /// §5.7): the `sub` that another provider gives, which the deployment names next, may be
    /// another user's (AUTH-084).
    fn subject(&self, sub: String) -> String {
        format!("{} {sub}", self.issuer)
    }
}
