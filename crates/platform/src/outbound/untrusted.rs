//! The untrusted client (SEC-10): it calls URLs that come from a third party, such as the
//! picture of a user that an OAuth provider names. It fetches over HTTPS only, from public
//! addresses only, which it checks once DNS has resolved a name, so a name that resolves to a
//! private address reaches nothing, re-checks every redirect and follows at most
//! [`MAX_REDIRECTS`], and caps each answer's size and time. It sends no credential, no cookie
//! and no proxy's request, so every address it reaches is one it checked.

use std::{
    error::Error,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use bytes::{Bytes, BytesMut};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tracing::Instrument;

use super::{OutboundConfig, OutboundError};
use crate::telemetry::names;

/// How long the untrusted client waits for a connection.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// How long the untrusted client waits for a whole answer, its redirects included.
const TIMEOUT: Duration = Duration::from_secs(10);

/// The most redirects the untrusted client follows for one fetch.
const MAX_REDIRECTS: usize = 3;

/// The client that calls URLs that come from a third party (SEC-10).
#[derive(Clone, Debug)]
pub struct Untrusted {
    client: reqwest::Client,
    test_origin: Option<url::Url>,
}

/// Why a fetch got nothing. Each holds its cause, for the logs alone (ERR-04).
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    /// The client never fetches the URL, or never takes its answer, so every attempt would end
    /// the same: a URL that isn't HTTPS, an address that isn't public, too many redirects, an
    /// answer over the size the fetch allows, or a status that says the URL gives nothing.
    #[error("{0}")]
    Refused(String),
    /// No answer came in time, or the server failed: another attempt may get one.
    #[error("{0}")]
    Failed(String),
}

/// Why the client refuses a URL, as the redirect policy and the resolver report it through
/// `reqwest`'s error, whose chain [`refusal`] finds it in.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Forbidden(String);

impl Untrusted {
    /// The untrusted client, which accepts `config`'s test origin, a loopback address over
    /// `http`, as the one address that isn't public, in the test environment alone (SEC-10).
    ///
    /// # Errors
    ///
    /// When the TLS configuration, which trusts the operating system's certificates, can't be
    /// built.
    pub fn new(config: &OutboundConfig) -> Result<Self, OutboundError> {
        let tls = crate::tls::client_config(true, None)
            .map_err(|error| OutboundError(format!("the TLS configuration: {error}")))?;
        let test_origin = config.test_origin.clone();
        let allowed = test_origin.clone();
        let redirects = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() > MAX_REDIRECTS {
                return attempt.error(Forbidden(format!("more than {MAX_REDIRECTS} redirects")));
            }
            match permitted(attempt.url(), allowed.as_ref()) {
                Ok(()) => attempt.follow(),
                Err(forbidden) => attempt.error(forbidden),
            }
        });
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TIMEOUT)
            .redirect(redirects)
            .dns_resolver(std::sync::Arc::new(PublicAddresses))
            .no_proxy()
            .tls_backend_preconfigured(tls)
            .build()
            .map_err(|error| OutboundError(format!("the client: {error}")))?;
        Ok(Self {
            client,
            test_origin,
        })
    }

    /// Fetches `url` with a `GET`, following its redirects, and gives the body of a success's
    /// answer, of at most `max_bytes`.
    ///
    /// # Errors
    ///
    /// [`FetchError::Refused`] when the client never fetches the URL or a redirect of it, or
    /// never takes its answer; [`FetchError::Failed`] when no answer came in time, or the server
    /// failed.
    pub async fn get(&self, url: &str, max_bytes: usize) -> Result<Bytes, FetchError> {
        let url = url::Url::parse(url)
            .map_err(|_| FetchError::Refused("the address isn't a URL".to_owned()))?;
        permitted(&url, self.test_origin.as_ref())
            .map_err(|forbidden| FetchError::Refused(forbidden.0))?;
        let host = url.host_str().unwrap_or_default().to_owned();
        let span = tracing::info_span!(
            "http",
            { names::OTEL_NAME } = "GET",
            { names::OTEL_KIND } = "client",
            { names::HTTP_REQUEST_METHOD } = "GET",
            { names::SERVER_ADDRESS } = %host,
            { names::HTTP_RESPONSE_STATUS_CODE } = tracing::field::Empty,
        );
        let fetched = self.fetch(url, &host, max_bytes, &span);
        fetched.instrument(span.clone()).await
    }

    /// Sends the `GET` of `url`, of the host `host`, and reads its answer, recording its status
    /// in `span`.
    async fn fetch(
        &self,
        url: url::Url,
        host: &str,
        max_bytes: usize,
        span: &tracing::Span,
    ) -> Result<Bytes, FetchError> {
        let failed = |error: reqwest::Error| match refusal(&error) {
            Some(reason) => FetchError::Refused(format!("GET of {host}: {reason}")),
            None => FetchError::Failed(format!("GET of {host}: {}", error.without_url())),
        };
        let mut response = self.client.get(url).send().await.map_err(failed)?;
        let status = response.status();
        span.record(names::HTTP_RESPONSE_STATUS_CODE, status.as_u16());
        if !status.is_success() {
            let answered = format!("GET of {host} answered {}", status.as_u16());
            // A server's failure, a timeout it reports, or a limit it sets may pass; any other
            // status says the URL gives nothing.
            return Err(
                if status.is_server_error() || matches!(status.as_u16(), 408 | 429) {
                    FetchError::Failed(answered)
                } else {
                    FetchError::Refused(answered)
                },
            );
        }
        let too_large = || {
            FetchError::Refused(format!(
                "GET of {host}: the answer is over {max_bytes} bytes"
            ))
        };
        let most = u64::try_from(max_bytes).unwrap_or(u64::MAX);
        if response
            .content_length()
            .is_some_and(|length| length > most)
        {
            return Err(too_large());
        }
        let mut body = BytesMut::new();
        while let Some(chunk) = response.chunk().await.map_err(failed)? {
            if body.len() + chunk.len() > max_bytes {
                return Err(too_large());
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body.freeze())
    }
}

/// Whether the client fetches `url`: an `https` URL without credentials, whose host, when it is
/// an address, is public. A name's addresses are checked once it resolves ([`PublicAddresses`]).
/// The one other URL it fetches is one of `test_origin`, the tests' loopback origin.
fn permitted(url: &url::Url, test_origin: Option<&url::Url>) -> Result<(), Forbidden> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(Forbidden("the URL holds credentials".to_owned()));
    }
    if test_origin.is_some_and(|origin| origin.origin() == url.origin()) {
        return Ok(());
    }
    if url.scheme() != "https" {
        return Err(Forbidden("the URL isn't HTTPS".to_owned()));
    }
    let address = match url.host() {
        Some(url::Host::Ipv4(address)) => Some(IpAddr::V4(address)),
        Some(url::Host::Ipv6(address)) => Some(IpAddr::V6(address)),
        Some(url::Host::Domain(_)) => None,
        None => return Err(Forbidden("the URL names no host".to_owned())),
    };
    match address {
        Some(address) if !public(address) => {
            Err(Forbidden("the URL's address isn't public".to_owned()))
        }
        _ => Ok(()),
    }
}

/// The reason the client itself refused a request, which `error`'s chain holds when the redirect
/// policy or the resolver refused it.
fn refusal(error: &reqwest::Error) -> Option<String> {
    let mut source: Option<&(dyn Error + 'static)> = Some(error);
    while let Some(cause) = source {
        if let Some(forbidden) = cause.downcast_ref::<Forbidden>() {
            return Some(forbidden.0.clone());
        }
        source = cause.source();
    }
    None
}

/// The resolver of the untrusted client: the operating system's, keeping the public addresses
/// alone, so the client connects to no private, loopback or link-local address, whatever name
/// the URL or a redirect gives (SEC-10).
struct PublicAddresses;

impl Resolve for PublicAddresses {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            // The port is the connector's to set.
            let resolved = tokio::net::lookup_host((name.as_str(), 0)).await?;
            let public: Vec<SocketAddr> = resolved.filter(|address| public(address.ip())).collect();
            if public.is_empty() {
                return Err(Forbidden("the name resolves to no public address".to_owned()).into());
            }
            Ok(Box::new(public.into_iter()) as Addrs)
        })
    }
}

/// Whether `address` is a public one: none of the private, loopback, link-local, shared,
/// documentation, benchmarking, multicast and reserved ranges of the Internet Assigned Numbers
/// Authority's special-purpose registries. An IPv6 address that carries an IPv4 one is as public
/// as the address it carries.
fn public(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => public_v4(address),
        IpAddr::V6(address) => public_v6(address),
    }
}

fn public_v4(address: Ipv4Addr) -> bool {
    let [a, b, c, _] = address.octets();
    !(address.is_unspecified()
        || address.is_private()
        || address.is_loopback()
        || address.is_link_local()
        || address.is_multicast()
        || address.is_broadcast()
        || address.is_documentation()
        // "This network".
        || a == 0
        // Shared address space, of carrier-grade translation.
        || (a == 100 && (64..128).contains(&b))
        // Protocol assignments.
        || (a == 192 && b == 0 && c == 0)
        // Benchmarking.
        || (a == 198 && (18..20).contains(&b))
        // Reserved.
        || a >= 240)
}

fn public_v6(address: Ipv6Addr) -> bool {
    if let Some(carried) = address.to_ipv4_mapped() {
        return public_v4(carried);
    }
    let segments = address.segments();
    // The well-known prefix of translated IPv4 addresses, 64:ff9b::/96.
    if segments[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [high, low] = [segments[6], segments[7]];
        return public_v4(Ipv4Addr::from((u32::from(high) << 16) | u32::from(low)));
    }
    // Global unicast addresses are 2000::/3. Within it, these aren't public: the documentation
    // prefixes, 2001:db8::/32 and 3fff::/20, and the protocol assignments of 2001::/23, which
    // hold Teredo's tunnels, and 2002::/16, whose addresses carry an IPv4 one.
    let global = segments[0] & 0xe000 == 0x2000;
    let documentation = (segments[0] == 0x2001 && segments[1] == 0xdb8)
        || (segments[0] == 0x3fff && segments[1] & 0xf000 == 0);
    let assigned = segments[0] == 0x2001 && segments[1] < 0x200;
    let six_to_four = segments[0] == 0x2002;
    global && !documentation && !assigned && !six_to_four
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refused(url: &str) -> bool {
        permitted(&url::Url::parse(url).unwrap(), None).is_err()
    }

    #[test]
    fn sec_10_the_untrusted_client_reaches_public_addresses_alone() {
        for address in [
            "0.0.0.0",
            "10.1.2.3",
            "100.64.0.1",
            "127.0.0.1",
            "169.254.169.254",
            "172.16.0.1",
            "192.0.0.8",
            "192.0.2.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.1",
            "203.0.113.1",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
            "::",
            "::1",
            "::ffff:10.0.0.1",
            "::ffff:127.0.0.1",
            "64:ff9b::a00:1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "ff02::1",
            "2001:db8::1",
            "2001::1",
            "2002:a00:1::1",
            "3fff::1",
        ] {
            assert!(!public(address.parse().unwrap()), "{address}");
        }
        for address in [
            "1.1.1.1",
            "8.8.8.8",
            "100.63.255.255",
            "172.32.0.1",
            "198.17.255.255",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
            "2606:4700:4700::1111",
            "2a00:1450:4001::1",
        ] {
            assert!(public(address.parse().unwrap()), "{address}");
        }
    }

    #[test]
    fn sec_10_the_untrusted_client_fetches_https_urls_without_credentials_alone() {
        assert!(!refused("https://pictures.example.com/ada.png"));
        assert!(!refused("https://8.8.8.8/ada.png"));
        assert!(refused("http://pictures.example.com/ada.png"));
        assert!(refused("ftp://pictures.example.com/ada.png"));
        assert!(refused("https://ada:secret@pictures.example.com/ada.png"));
        assert!(refused("https://127.0.0.1/ada.png"));
        assert!(refused("https://[::1]/ada.png"));
        assert!(refused("https://169.254.169.254/latest/meta-data"));
        assert!(refused("https://[fd00::1]/ada.png"));
    }

    #[test]
    fn sec_10_the_untrusted_client_accepts_the_tests_loopback_origin_alone() {
        let origin = url::Url::parse("http://127.0.0.1:4000").unwrap();
        let permits = |url: &str| permitted(&url::Url::parse(url).unwrap(), Some(&origin)).is_ok();
        assert!(permits("http://127.0.0.1:4000/ada.png"));
        assert!(!permits("http://127.0.0.1:4001/ada.png"));
        assert!(!permits("http://127.0.0.2:4000/ada.png"));
        assert!(!permits("http://pictures.example.com/ada.png"));
        assert!(permits("https://pictures.example.com/ada.png"));
    }
}
