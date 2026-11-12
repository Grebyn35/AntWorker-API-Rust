//! The OAuth providers that a user signs in through (the auth spec's "Signing in through a
//! provider"): one file for each, which an optional part owns, and its lines in this registry
//! (ARCH-07). What every provider shares is here, so it holds whichever providers a fork keeps:
//! reading each client's settings (AUTH-081), and the exchange of a code for an access token that
//! RFC 6749 defines (§4.1.3, §5.2), whose refusal of the code is `invalid_grant`.

use futures_util::future::BoxFuture;
use platform::{
    config::{ConfigError, Settings},
    outbound::{Configured, Request, Response},
    types::Secret,
};
use serde::Deserialize;

// part: github
mod github;
// end part: github
// part: google
mod google;
// end part: google
#[cfg(feature = "test-support")]
mod stand_in;

/// Each provider of the API: its key, its clients' settings, and how it is built.
const REGISTRY: &[Entry] = &[
    // part: github
    github::ENTRY,
    // end part: github
    // part: google
    google::ENTRY,
    // end part: google
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
    /// The provider, with the clients that the deployment configures, at least one.
    build: fn(Vec<Client>) -> Box<dyn Provider>,
}

/// An OAuth provider: its endpoints, the scopes it asks for, its clients, and how it reads a
/// user's identity (ARCH-13: one implementation per provider).
pub(super) trait Provider: Send + Sync {
    /// Its key, which a sign-in's `provider` names.
    fn key(&self) -> &'static str;

    /// Its name, as a client shows it.
    fn name(&self) -> &'static str;

    /// The URL of its authorization endpoint, which a client opens.
    fn authorization_endpoint(&self) -> &'static str;

    /// The scopes it asks for, the least it needs (the auth spec's "The identity").
    fn scopes(&self) -> &'static [&'static str];

    /// The clients that the deployment registered with it.
    fn clients(&self) -> &[Client];

    /// The URL of its token endpoint, which exchanges a code for an access token.
    fn token_endpoint(&self) -> &'static str;

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

    /// Reads the identity of the user whose access token `token` is.
    fn identify<'a>(
        &'a self,
        outbound: &'a Configured,
        client: &'a Client,
        token: &'a Secret<String>,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>>;

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

/// Exchanges `exchange`'s code at `provider` for an access token, as RFC 6749 asks (§4.1.3)
/// (AUTH-073, AUTH-077). A refusal of the code, its verifier or its redirect URI is the token
/// endpoint's `invalid_grant` (§5.2); any other refusal is of the API's own client or request,
/// which no user can fix, so it is an unavailable provider.
pub(super) async fn token(
    provider: &dyn Provider,
    outbound: &Configured,
    exchange: &Exchange<'_>,
) -> Result<Secret<String>, ProviderError> {
    let answer = outbound
        .send(
            Request::post(provider.token_endpoint())
                .header("accept", "application/json")
                .form(&[
                    ("grant_type", "authorization_code"),
                    ("client_id", &exchange.client.id),
                    ("client_secret", exchange.client.secret.expose()),
                    ("code", exchange.code.expose()),
                    ("code_verifier", exchange.verifier.expose()),
                    ("redirect_uri", exchange.redirect_uri),
                ]),
        )
        .await
        .map_err(|error| ProviderError::Unavailable(error.to_string()))?;
    if !answer.is_success() {
        return Err(refusal(&answer));
    }
    provider.token(&answer)
}

/// Why the token endpoint refused the exchange, from its answer `answer` (RFC 6749, §5.2).
fn refusal(answer: &Response) -> ProviderError {
    #[derive(Deserialize)]
    struct Refusal {
        error: String,
    }
    match answer.json::<Refusal>() {
        Some(refusal) if refusal.error == "invalid_grant" => ProviderError::Rejected,
        Some(refusal) => ProviderError::Unavailable(format!(
            "the token endpoint refused the API's request with {}, and status {}",
            error_code(&refusal.error),
            answer.status
        )),
        None => {
            ProviderError::Unavailable(format!("the token endpoint answered {}", answer.status))
        }
    }
}

/// The code of a token endpoint's refusal when it is one that RFC 6749 defines (§5.2), or
/// `another code`, so the logs name a refusal, such as `invalid_client` for a client's secret set
/// wrong, but nothing else that the provider sent (AUTH-077).
fn error_code(error: &str) -> &'static str {
    const CODES: [&str; 6] = [
        "invalid_request",
        "invalid_client",
        "invalid_grant",
        "unauthorized_client",
        "unsupported_grant_type",
        "invalid_scope",
    ];
    CODES
        .into_iter()
        .find(|code| *code == error)
        .unwrap_or("another code")
}

/// A client that the deployment registered with a provider: its ID, its secret, and the
/// redirect URIs it lists (AUTH-081).
pub(super) struct Client {
    /// The setting that lists its redirect URIs, such as `STAND_IN_REDIRECT_URIS`.
    setting: String,
    pub(super) id: String,
    pub(super) secret: Secret<String>,
    pub(super) redirect_uris: Vec<String>,
}

/// What a request exchanges at a provider: its code, its verifier, and its redirect URI, with
/// the client that lists the URI (AUTH-073, AUTH-078).
pub(super) struct Exchange<'a> {
    pub(super) client: &'a Client,
    pub(super) code: &'a Secret<String>,
    pub(super) verifier: &'a Secret<String>,
    pub(super) redirect_uri: &'a str,
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
    pub(crate) name: &'static str,
    /// The URL of its authorization endpoint.
    pub(crate) authorization_endpoint: &'static str,
    /// The scopes that the authorization request asks for.
    pub(crate) scopes: &'static [&'static str],
    /// The deployment's clients: each one's ID, with the redirect URIs it lists.
    pub(crate) clients: Vec<(String, Vec<String>)>,
}

/// What a provider's key finds (AUTH-077).
pub(super) enum Found<'a> {
    /// A provider that the deployment configures.
    On(&'a dyn Provider),
    /// A provider of the API that the deployment doesn't configure.
    Off,
    /// No provider of the API.
    Unknown,
}

/// The providers of the API, those the deployment configures among them (AUTH-081).
pub(crate) struct Providers {
    known: Vec<&'static str>,
    on: Vec<Box<dyn Provider>>,
}

impl Providers {
    /// The providers that the deployment's `settings` configure, once no redirect URI that a
    /// request may name is matched by two clients' lists (AUTH-078, AUTH-081).
    pub(crate) fn from_settings<E, F>(settings: &Settings<E, F>) -> Result<Self, ConfigError>
    where
        E: Fn(&str) -> Option<std::ffi::OsString>,
        F: Fn(&std::path::Path) -> std::io::Result<String>,
    {
        let mut on: Vec<Box<dyn Provider>> = Vec::new();
        for entry in REGISTRY.iter().chain(STAND_IN) {
            let mut clients = Vec::new();
            for prefix in entry.clients {
                clients.extend(client(settings, prefix)?);
            }
            if !clients.is_empty() {
                on.push((entry.build)(clients));
            }
        }
        let clients: Vec<&Client> = on.iter().flat_map(|provider| provider.clients()).collect();
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
            Found::On(provider) => client_for(provider, redirect_uri).is_some(),
            Found::Off | Found::Unknown => true,
        }
    }

    /// The provider that `key` names (AUTH-077).
    pub(super) fn find(&self, key: &str) -> Found<'_> {
        if let Some(provider) = self.on.iter().find(|provider| provider.key() == key) {
            return Found::On(provider.as_ref());
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
            .map(|provider| Listed {
                provider: provider.key(),
                name: provider.name(),
                authorization_endpoint: provider.authorization_endpoint(),
                scopes: provider.scopes(),
                clients: provider
                    .clients()
                    .iter()
                    .map(|client| (client.id.clone(), client.redirect_uris.clone()))
                    .collect(),
            })
            .collect();
        listed.sort_by_key(|listed| listed.provider);
        listed
    }
}

/// The client of `provider` that lists `redirect_uri` (AUTH-078).
pub(super) fn client_for<'a>(provider: &'a dyn Provider, redirect_uri: &str) -> Option<&'a Client> {
    provider.clients().iter().find(|client| {
        client
            .redirect_uris
            .iter()
            .any(|listed| matches(listed, redirect_uri))
    })
}

/// Whether `uri` is the redirect URI `listed`, compared as a string, but for a listed loopback
/// one without a port, `http://127.0.0.1/<path>` or `http://[::1]/<path>`, which matches the
/// same URI with any port, as RFC 8252 asks (AUTH-078).
fn matches(listed: &str, uri: &str) -> bool {
    if listed == uri {
        return true;
    }
    let (Ok(parsed), Ok(mut uri)) = (url::Url::parse(listed), url::Url::parse(uri)) else {
        return false;
    };
    let any_port = parsed.scheme() == "http" && is_loopback_ip(parsed.host()) && !has_port(listed);
    if !any_port || uri.port_or_known_default().is_none() || uri.set_port(None).is_err() {
        return false;
    }
    parsed == uri
}

/// Whether two listed redirect URIs name one URI that a request may give: they are the same, or
/// one matches the other as [`matches`] matches a request's (AUTH-081).
fn overlap(first: &str, second: &str) -> bool {
    matches(first, second) || matches(second, first)
}

/// Whether `host` is `127.0.0.1` or `[::1]`, the loopback addresses that RFC 8252 names.
fn is_loopback_ip(host: Option<url::Host<&str>>) -> bool {
    matches!(host, Some(url::Host::Ipv4(address)) if address == std::net::Ipv4Addr::LOCALHOST)
        || matches!(host, Some(url::Host::Ipv6(address)) if address == std::net::Ipv6Addr::LOCALHOST)
}

/// Whether the URI `uri`, as written, names a port, which parsing drops when it is the scheme's
/// default, such as `:80` for `http`.
fn has_port(uri: &str) -> bool {
    let authority = uri
        .split_once("://")
        .map_or("", |(_, rest)| rest.split(['/', '?']).next().unwrap_or(""));
    authority
        .rsplit_once(':')
        .is_some_and(|(host, port)| !host.ends_with(':') && !port.is_empty() && !port.contains(']'))
}

/// A client of a provider from the settings `<prefix>CLIENT_ID`, `<prefix>CLIENT_SECRET` and
/// `<prefix>REDIRECT_URIS`, such as `STAND_IN_CLIENT_ID`, or `None` when none of them is set; the
/// deployment sets all three or none (AUTH-081).
fn client<E, F>(settings: &Settings<E, F>, prefix: &str) -> Result<Option<Client>, ConfigError>
where
    E: Fn(&str) -> Option<std::ffi::OsString>,
    F: Fn(&std::path::Path) -> std::io::Result<String>,
{
    let id_name = format!("{prefix}CLIENT_ID");
    let secret_name = format!("{prefix}CLIENT_SECRET");
    let uris_name = format!("{prefix}REDIRECT_URIS");
    let id = settings.text_setting(&id_name)?;
    let secret = settings.secret_setting(&secret_name)?;
    let uris = settings.text_setting(&uris_name)?;
    let (id, secret, uris) = match (id, secret, uris) {
        (None, None, None) => return Ok(None),
        (Some(id), Some(secret), Some(uris)) => (id, secret, uris),
        (id, secret, _) => {
            let missing = if id.is_none() {
                id_name
            } else if secret.is_none() {
                secret_name
            } else {
                uris_name
            };
            return Err(ConfigError::missing(&missing));
        }
    };
    let redirect_uris = uris
        .split(',')
        .map(str::trim)
        .map(|uri| redirect_uri(uri).ok_or_else(|| ConfigError::invalid(&uris_name, URIS_EXPECTED)))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Some(Client {
        setting: uris_name,
        id,
        secret,
        redirect_uris,
    }))
}

const URIS_EXPECTED: &str = "redirect URIs separated by commas, each https, or http to \
     127.0.0.1, [::1] or localhost, with no fragment (AUTH-081)";

/// `uri`, as a redirect URI that a client may list: `https`, or `http` to a loopback host, with
/// no fragment (AUTH-081).
fn redirect_uri(uri: &str) -> Option<String> {
    let parsed = url::Url::parse(uri).ok()?;
    let loopback = matches!(parsed.host(), Some(url::Host::Domain("localhost")))
        || is_loopback_ip(parsed.host());
    let allowed = match parsed.scheme() {
        "https" => parsed.host().is_some(),
        "http" => loopback,
        _ => false,
    };
    (allowed && parsed.fragment().is_none() && !uri.contains('#')).then(|| uri.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_078_a_loopback_redirect_uri_without_a_port_matches_any_port() {
        assert!(matches(
            "http://127.0.0.1/callback",
            "http://127.0.0.1:51234/callback"
        ));
        assert!(matches(
            "http://[::1]/callback",
            "http://[::1]:8000/callback"
        ));
        assert!(!matches(
            "http://127.0.0.1/callback",
            "http://127.0.0.1:51234/other"
        ));
        assert!(!matches(
            "http://127.0.0.1:4000/callback",
            "http://127.0.0.1:4001/callback"
        ));
        assert!(!matches(
            "http://localhost/callback",
            "http://localhost:4001/callback"
        ));
        assert!(!matches(
            "https://app.example.com/callback",
            "https://app.example.com:8443/callback"
        ));
        assert!(matches(
            "https://app.example.com/callback",
            "https://app.example.com/callback"
        ));
    }

    #[test]
    fn auth_078_only_127_0_0_1_and_ipv6_s_loopback_match_any_port() {
        assert!(!matches(
            "http://127.0.0.2/callback",
            "http://127.0.0.2:4000/callback"
        ));
        assert!(matches(
            "http://127.0.0.1/callback",
            "http://127.0.0.1:80/callback"
        ));
        assert!(!matches(
            "http://127.0.0.1:80/callback",
            "http://127.0.0.1:81/callback"
        ));
    }

    #[test]
    fn auth_081_two_listed_redirect_uris_overlap_when_one_matches_the_other() {
        assert!(overlap(
            "http://127.0.0.1/callback",
            "http://127.0.0.1:5000/callback"
        ));
        assert!(overlap(
            "https://app.example.com/a",
            "https://app.example.com/a"
        ));
        assert!(!overlap(
            "http://127.0.0.1:4000/callback",
            "http://127.0.0.1:5000/callback"
        ));
        assert!(!overlap("http://127.0.0.1/a", "http://127.0.0.1/b"));
    }

    #[test]
    fn auth_081_a_redirect_uri_is_https_or_http_to_a_loopback_host_without_a_fragment() {
        assert!(redirect_uri("https://app.example.com/callback").is_some());
        assert!(redirect_uri("http://127.0.0.1/callback").is_some());
        assert!(redirect_uri("http://[::1]/callback").is_some());
        assert!(redirect_uri("http://localhost:5173/callback").is_some());
        assert!(redirect_uri("http://app.example.com/callback").is_none());
        assert!(redirect_uri("https://app.example.com/callback#top").is_none());
        assert!(redirect_uri("app://callback").is_none());
        assert!(redirect_uri("http://127.0.0.2/callback").is_none());
    }
}
