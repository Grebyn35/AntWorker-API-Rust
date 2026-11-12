//! The OAuth providers that a user signs in through (the auth spec's "Signing in through a
//! provider"): one file for each, which an optional part owns, and its lines in this registry
//! (ARCH-07). What every provider shares is here, so it holds whichever providers a fork keeps:
//! reading the settings (AUTH-081), the exchange of a code for an access token that RFC 6749
//! defines (§4.1.3, §5.2), whose refusal of the code is `invalid_grant`, and the read of the
//! user's identity that OpenID Connect defines (Core, §5.3). A provider's file declares where the
//! provider is, and replaces only what the provider does differently.

use futures_util::future::BoxFuture;
use platform::{
    config::{ConfigError, Settings},
    outbound::{Configured, Request, Response},
    types::Secret,
};
use serde::Deserialize;

mod clients;
mod exchange;
// part: github
mod github;
// end part: github
// part: google
mod google;
// end part: google
// part: openid_connect
mod openid_connect;
// end part: openid_connect
#[cfg(feature = "test-support")]
mod stand_in;

pub(super) use clients::Client;
use clients::{client, matches, overlap};
pub(super) use exchange::Exchange;

/// Each provider of the API: its key, the settings it reads, and how it is built.
const REGISTRY: &[Entry] = &[
    // part: github
    github::ENTRY,
    // end part: github
    // part: google
    google::ENTRY,
    // end part: google
    // part: openid_connect
    openid_connect::ENTRY,
    // end part: openid_connect
];

/// The tests' stand-in provider, in the tests' build alone.
#[cfg(feature = "test-support")]
const STAND_IN: &[Entry] = &[stand_in::ENTRY];
#[cfg(not(feature = "test-support"))]
const STAND_IN: &[Entry] = &[];

/// A provider of the registry.
struct Entry {
    /// Its key, which a sign-in's `provider` names.
    key: &'static str,
    /// The prefix of each of its clients' settings, such as `STAND_IN_` for `STAND_IN_CLIENT_ID`,
    /// `STAND_IN_CLIENT_SECRET` and `STAND_IN_REDIRECT_URIS` (AUTH-081).
    clients: &'static [&'static str],
    /// The settings it reads beside its clients', such as the stand-in's `STAND_IN_NAME`: each
    /// is set while a client of its is, and only then (AUTH-081).
    settings: &'static [&'static str],
    /// The provider, from the values of `settings`, in their order.
    build: fn(&[String]) -> Built,
}

/// The provider that an entry builds, or the setting whose value it refuses.
type Built = Result<Box<dyn Provider>, ConfigError>;

/// What a provider declares: its name, its addresses, and the scopes it asks for.
struct Declared<'a> {
    /// Its name, as a client shows it.
    name: &'a str,
    /// The URL of its authorization endpoint, which a client opens.
    authorization_endpoint: &'a str,
    /// The URL of its token endpoint, which exchanges a code for an access token.
    token_endpoint: &'a str,
    /// The URL that gives the identity of a token's user: the userinfo endpoint of a provider
    /// that follows OpenID Connect.
    identity_endpoint: &'a str,
    /// The scopes it asks for, the least it needs (the auth spec's "The identity").
    scopes: &'a [&'a str],
}

/// An OAuth provider, as its file states it (ARCH-13: one implementation for each provider):
/// what it declares, and what it does differently from the standards, which the provided methods
/// follow. A provider that follows OpenID Connect states what it declares, and replaces only
/// what its own behavior adds, such as which addresses it proves. What the registry does for
/// every provider is a provided method here, or code that the service calls, so it stays live in
/// a fork that removes every provider, where a function that only providers' files call would
/// be dead (CODE-15).
trait Provider: Send + Sync {
    /// What it declares.
    fn declared(&self) -> Declared<'_>;

    /// The access token of the token endpoint's answer, whose status is a success: its
    /// `access_token`, as RFC 6749 gives it (§5.1), or why it holds none, as for a provider that
    /// answers a refusal with a success status.
    fn token(&self, answer: &Response) -> Result<Secret<String>, ProviderError> {
        #[derive(Deserialize)]
        struct Token {
            access_token: String,
        }
        answer
            .json::<Token>()
            .map(|token| Secret::new(token.access_token))
            .ok_or_else(|| {
                ProviderError::Unavailable("the token endpoint's answer holds no token".to_owned())
            })
    }

    /// Whether the token endpoint takes the client's ID and secret in HTTP's Basic scheme, which
    /// RFC 6749 asks every provider to take (§2.3.1), rather than in the request's body, where a
    /// provider's documentation asks for them.
    fn basic(&self) -> bool {
        false
    }

    /// Reads the identity of the user whose access token `token` is: as OpenID Connect's userinfo
    /// endpoint gives it, the `sub`, the `email` and the `name` of its answer (Core, §5.3), unless
    /// the provider reads it another way.
    fn identify<'a>(
        &'a self,
        outbound: &'a Configured,
        token: &'a Secret<String>,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>> {
        Box::pin(async move {
            let endpoint = self.declared().identity_endpoint;
            let answer = outbound
                .send(Request::get(endpoint).bearer(Secret::new(token.expose().clone())))
                .await
                .map_err(|error| ProviderError::Unavailable(error.to_string()))?;
            let unreadable = || {
                ProviderError::Unavailable(format!(
                    "the userinfo endpoint answered {}",
                    answer.status
                ))
            };
            let claims: serde_json::Value = answer
                .is_success()
                .then(|| answer.json())
                .flatten()
                .ok_or_else(unreadable)?;
            let info = UserInfo::deserialize(&claims).map_err(|_| unreadable())?;
            Ok(Identity {
                subject: self.subject(info.sub),
                email: info.email,
                proved: self.proves(&claims),
                name: info.name,
            })
        })
    }

    /// Whether the provider is the authority of the address in `claims`, its userinfo endpoint's
    /// answer, so its sign-in proves the address: of none, unless its file says of which
    /// (AUTH-075, D140).
    fn proves(&self, _claims: &serde_json::Value) -> bool {
        false
    }

    /// The provider's ID for the user whose `sub` its userinfo endpoint answers: the `sub`,
    /// which a provider gives one user alone, unless more than the `sub` names its user.
    fn subject(&self, sub: String) -> String {
        sub
    }

    /// Lets go of `token`, which `client`'s exchange got, once the identity is read or its
    /// reading failed: a provider whose tokens lapse does nothing, and one whose tokens don't
    /// deletes it, or says why it couldn't (AUTH-079, D139).
    fn discard<'a>(
        &'a self,
        _outbound: &'a Configured,
        _client: &'a Client,
        _token: &'a Secret<String>,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// The claims of a userinfo endpoint's answer that the registry reads (OpenID Connect Core,
/// §5.3.2).
#[derive(Deserialize)]
struct UserInfo {
    sub: String,
    email: Option<String>,
    name: Option<String>,
}

/// A provider that the deployment configures: its key, its clients, and what its file states.
pub(super) struct On {
    /// Its key, which a sign-in's `provider` names.
    pub(super) key: &'static str,
    clients: Vec<Client>,
    provider: Box<dyn Provider>,
}

impl On {
    /// The client of its that lists `redirect_uri` (AUTH-078).
    pub(super) fn client_for(&self, redirect_uri: &str) -> Option<&Client> {
        self.clients.iter().find(|client| {
            client
                .redirect_uris
                .iter()
                .any(|listed| matches(listed, redirect_uri))
        })
    }

    /// Reads the identity of the user whose access token `token` is.
    pub(super) fn identify<'a>(
        &'a self,
        outbound: &'a Configured,
        token: &'a Secret<String>,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>> {
        self.provider.identify(outbound, token)
    }

    /// Lets go of `token`, which `client`'s exchange got, as the provider asks, or says why it
    /// couldn't (AUTH-079).
    pub(super) fn discard<'a>(
        &'a self,
        outbound: &'a Configured,
        client: &'a Client,
        token: &'a Secret<String>,
    ) -> BoxFuture<'a, Result<(), String>> {
        self.provider.discard(outbound, client, token)
    }
}

/// A user's identity at a provider (the auth spec's "The identity"), personal data that no
/// `Debug` shows (OBS-04).
pub(crate) struct Identity {
    /// The provider's ID for the user, which it never gives another user.
    pub(crate) subject: String,
    /// The address it gives, as it gives it, if any.
    pub(crate) email: Option<String>,
    /// Whether the provider is the address's authority, so its sign-in proves it (AUTH-075,
    /// D140).
    pub(crate) proved: bool,
    /// The name it gives, as it gives it, if any.
    pub(crate) name: Option<String>,
}

/// Why a provider gave no identity (AUTH-077).
#[derive(Debug)]
pub(super) enum ProviderError {
    /// It refused the code, its verifier or its redirect URI.
    Rejected,
    /// It failed, or answered what the API can't read, or not in time, or refused the API's own
    /// client: the cause, for the logs alone.
    Unavailable(String),
}

/// A provider that the deployment configures, as the capabilities endpoint's
/// `sign_in_providers` lists it (AUTH-081).
pub(crate) struct Listed {
    /// Its key, which a sign-in's `provider` names.
    pub(crate) provider: &'static str,
    /// Its name, as a client shows it.
    pub(crate) name: String,
    /// The URL of its authorization endpoint.
    pub(crate) authorization_endpoint: String,
    /// The scopes that the authorization request asks for.
    pub(crate) scopes: Vec<String>,
    /// The deployment's clients: each one's ID, with the redirect URIs it lists.
    pub(crate) clients: Vec<(String, Vec<String>)>,
}

/// What a provider's key finds (AUTH-077).
pub(super) enum Found<'a> {
    /// A provider that the deployment configures.
    On(&'a On),
    /// A provider of the API that the deployment doesn't configure.
    Off,
    /// No provider of the API.
    Unknown,
}

/// The providers of the API, those the deployment configures among them (AUTH-081).
pub(crate) struct Providers {
    known: Vec<&'static str>,
    on: Vec<On>,
}

impl Providers {
    /// The providers that the deployment's `settings` configure, once no redirect URI that a
    /// request may name is matched by two clients' lists (AUTH-078, AUTH-081).
    pub(crate) fn from_settings<E, F>(settings: &Settings<E, F>) -> Result<Self, ConfigError>
    where
        E: Fn(&str) -> Option<std::ffi::OsString>,
        F: Fn(&std::path::Path) -> std::io::Result<String>,
    {
        let mut on: Vec<On> = Vec::new();
        for entry in REGISTRY.iter().chain(STAND_IN) {
            let mut clients = Vec::new();
            for prefix in entry.clients {
                clients.extend(client(settings, prefix)?);
            }
            if let Some(values) = values(settings, entry, !clients.is_empty())? {
                on.push(On {
                    key: entry.key,
                    clients,
                    provider: (entry.build)(&values)?,
                });
            }
        }
        let clients: Vec<&Client> = on.iter().flat_map(|provider| &provider.clients).collect();
        for (index, client) in clients.iter().enumerate() {
            let before = clients[..index]
                .iter()
                .flat_map(|earlier| &earlier.redirect_uris);
            for earlier in before {
                if client.redirect_uris.iter().any(|uri| overlap(uri, earlier)) {
                    return Err(ConfigError::invalid(
                        &client.setting,
                        "redirect URIs that no other client lists, nor matches as AUTH-078 \
                         matches them, so each names one provider (AUTH-081)",
                    ));
                }
            }
        }
        Ok(Self {
            known: REGISTRY
                .iter()
                .chain(STAND_IN)
                .map(|entry| entry.key)
                .collect(),
            on,
        })
    }

    /// Whether the clients of the provider that `provider` names list `redirect_uri`, as a
    /// request's conversion asks, so its 400 names every field; a provider that the deployment
    /// doesn't configure lists any, since the service answers it (AUTH-077, AUTH-078).
    pub(crate) fn lists(&self, provider: &str, redirect_uri: &str) -> bool {
        match self.find(provider) {
            Found::On(provider) => provider.client_for(redirect_uri).is_some(),
            Found::Off | Found::Unknown => true,
        }
    }

    /// The provider that `key` names (AUTH-077).
    pub(super) fn find(&self, key: &str) -> Found<'_> {
        if let Some(provider) = self.on.iter().find(|provider| provider.key == key) {
            return Found::On(provider);
        }
        if self.known.contains(&key) {
            Found::Off
        } else {
            Found::Unknown
        }
    }

    /// Each provider that the deployment configures, by its key, as `sign_in_providers` lists
    /// it (AUTH-081).
    pub(crate) fn listed(&self) -> Vec<Listed> {
        let mut listed: Vec<Listed> = self
            .on
            .iter()
            .map(|provider| {
                let declared = provider.provider.declared();
                Listed {
                    provider: provider.key,
                    name: declared.name.to_owned(),
                    authorization_endpoint: declared.authorization_endpoint.to_owned(),
                    scopes: declared
                        .scopes
                        .iter()
                        .map(|scope| (*scope).to_owned())
                        .collect(),
                    clients: provider
                        .clients
                        .iter()
                        .map(|client| (client.id.clone(), client.redirect_uris.clone()))
                        .collect(),
                }
            })
            .collect();
        listed.sort_by_key(|listed| listed.provider);
        listed
    }
}

/// The values of the settings that `entry` reads beside its clients', in their order, when a
/// client of its is set, which `configured` says, and `None` without one: the deployment sets
/// them with a client, and only then (AUTH-081).
fn values<E, F>(
    settings: &Settings<E, F>,
    entry: &Entry,
    configured: bool,
) -> Result<Option<Vec<String>>, ConfigError>
where
    E: Fn(&str) -> Option<std::ffi::OsString>,
    F: Fn(&std::path::Path) -> std::io::Result<String>,
{
    let mut values = Vec::with_capacity(entry.settings.len());
    for name in entry.settings {
        match settings.text_setting(name)? {
            Some(value) => values.push(value),
            None if configured => return Err(ConfigError::missing(name)),
            None => {}
        }
    }
    if configured {
        return Ok(Some(values));
    }
    // A setting of a provider without a client asks for the client.
    match entry.clients.first() {
        Some(prefix) if !values.is_empty() => {
            Err(ConfigError::missing(&format!("{prefix}CLIENT_ID")))
        }
        _ => Ok(None),
    }
}
