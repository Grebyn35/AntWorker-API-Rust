//! The settings of the API's web clients and of the requests that reach it: the origins of web
//! clients, the proxies in front of it, and the key of its keyed hashes.

use std::{fmt, net::IpAddr, str::FromStr};

use base64::{Engine, engine::general_purpose::STANDARD};
use ipnet::IpNet;
use url::{Host, Url};

use super::Environment;
use crate::hashing::{self, HmacKey};

/// The origin of a web client, such as `https://app.example.com` (PLATFORM-052): a scheme, a
/// host and a port, as a browser sends it in `Origin`.
#[derive(Clone, PartialEq, Eq)]
pub struct Origin(String);

impl Origin {
    /// The origin as a browser serializes it, with no default port and no trailing slash.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The origin in `value`, which must be `https`, or `http` with a loopback host in
    /// development, and name nothing but the origin. `*` and `null` are never origins.
    pub(super) fn parse(value: &str, environment: Environment) -> Option<Self> {
        let url = Url::parse(value).ok()?;
        let only_origin = url.username().is_empty()
            && url.password().is_none()
            && matches!(url.path(), "" | "/")
            && url.query().is_none()
            && url.fragment().is_none();
        let secure = match url.scheme() {
            "https" => true,
            "http" => environment == Environment::Development && is_loopback(url.host()),
            _ => false,
        };
        let origin = url.origin();
        (only_origin && secure && origin.is_tuple()).then(|| Self(origin.ascii_serialization()))
    }
}

impl fmt::Debug for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

fn is_loopback(host: Option<Host<&str>>) -> bool {
    match host {
        Some(Host::Domain(name)) => name == "localhost",
        Some(Host::Ipv4(address)) => address.is_loopback(),
        Some(Host::Ipv6(address)) => address.is_loopback(),
        None => false,
    }
}

/// The origins in `value`, separated by commas, or `None` if one isn't an origin.
pub(super) fn origins(value: &str, environment: Environment) -> Option<Vec<Origin>> {
    value
        .split(',')
        .map(|origin| Origin::parse(origin.trim(), environment))
        .collect()
}

/// The addresses and ranges in `value`, separated by commas, such as
/// `172.18.0.0/16, 203.0.113.7`, or `None` if one is neither (PLATFORM-047).
pub(super) fn proxies(value: &str) -> Option<Vec<IpNet>> {
    value
        .split(',')
        .map(|item| {
            let item = item.trim();
            IpNet::from_str(item)
                .ok()
                .or_else(|| IpAddr::from_str(item).ok().map(IpNet::from))
        })
        .collect()
}

/// The key in `value`, base64 of at least 32 bytes, or `None`.
pub(super) fn hmac_key(value: &str) -> Option<HmacKey> {
    let bytes = STANDARD.decode(value).ok()?;
    (bytes.len() >= hashing::MIN_KEY_BYTES)
        .then(|| HmacKey::new(&bytes).ok())
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn origin(value: &str, environment: Environment) -> Option<String> {
        Origin::parse(value, environment).map(|origin| origin.as_str().to_owned())
    }

    #[test]
    fn sec_19_an_origin_names_https_and_nothing_but_the_origin() {
        let production = Environment::Production;
        assert_eq!(
            origin("https://App.Example.com:443/", production).as_deref(),
            Some("https://app.example.com")
        );
        assert_eq!(
            origin("https://app.example.com:8443", production).as_deref(),
            Some("https://app.example.com:8443")
        );
        for refused in [
            "*",
            "null",
            "http://app.example.com",
            "https://app.example.com/path",
            "https://app.example.com?query",
            "https://user@app.example.com",
            "app.example.com",
        ] {
            assert_eq!(origin(refused, production), None, "{refused}");
        }
    }

    #[test]
    fn sec_19_plain_http_is_an_origin_only_on_a_loopback_host_in_development() {
        let development = Environment::Development;
        assert!(origin("http://localhost:5173", development).is_some());
        assert!(origin("http://127.0.0.1:5173", development).is_some());
        assert!(origin("http://[::1]:5173", development).is_some());
        assert!(origin("http://app.example.com", development).is_none());
        assert!(origin("http://localhost:5173", Environment::Test).is_none());
    }

    #[test]
    fn platform_047_trusted_proxies_are_addresses_or_ranges() {
        let parsed = proxies("172.18.0.0/16, 203.0.113.7,2001:db8::/32").unwrap();
        assert_eq!(parsed.len(), 3);
        assert!(proxies("172.18.0.0/16, proxy.example.com").is_none());
    }

    #[test]
    fn sec_03_the_hmac_key_is_base64_of_at_least_32_bytes() {
        assert!(hmac_key(&STANDARD.encode([1; 32])).is_some());
        assert!(hmac_key(&STANDARD.encode([1; 31])).is_none());
        assert!(hmac_key("not base64!").is_none());
    }
}
