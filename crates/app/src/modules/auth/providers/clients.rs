//! The clients that a deployment registers with a provider: the settings of each, and the
//! redirect URIs it lists, which name it (AUTH-078, AUTH-081).

use platform::{
    config::{ConfigError, Settings},
    types::Secret,
};

/// A client that the deployment registered with a provider: its ID, its secret, and the
/// redirect URIs it lists (AUTH-081).
pub(in crate::modules::auth) struct Client {
    /// The setting that lists its redirect URIs, such as `STAND_IN_REDIRECT_URIS`.
    pub(super) setting: String,
    pub(super) id: String,
    pub(super) secret: Secret<String>,
    pub(super) redirect_uris: Vec<String>,
}

/// Whether `uri` is the redirect URI `listed`, compared as a string, but for a listed loopback
/// one without a port, `http://127.0.0.1/<path>` or `http://[::1]/<path>`, which matches the
/// same URI with any port, as RFC 8252 asks (AUTH-078).
pub(super) fn matches(listed: &str, uri: &str) -> bool {
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
pub(super) fn overlap(first: &str, second: &str) -> bool {
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
pub(super) fn client<E, F>(
    settings: &Settings<E, F>,
    prefix: &str,
) -> Result<Option<Client>, ConfigError>
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
