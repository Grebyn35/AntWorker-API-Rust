//! Who sent each request: its client IP, from its connection's peer or from the trusted proxies
//! in front of the API (PLATFORM-047), and its user agent.

use std::{
    net::{IpAddr, SocketAddr},
    str::FromStr,
    sync::Arc,
};

use axum::{
    extract::{ConnectInfo, FromRequestParts, Request, State},
    http::{HeaderMap, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
};
use ipnet::IpNet;

use crate::{
    errors::{Internal, Problem},
    types::Client,
};

/// The most characters of a `User-Agent` that a request keeps, so an audit entry can't be made
/// to hold an unbounded value.
const USER_AGENT_MAX_CHARS: usize = 512;

/// The proxies whose `X-Forwarded-For` names the client (`TRUSTED_PROXIES`).
pub(super) type TrustedProxies = Arc<Vec<IpNet>>;

/// Finds each request's [`Client`] and adds it to the request, before anything else reads it.
pub(super) async fn client(
    State(trusted): State<TrustedProxies>,
    mut request: Request,
    next: Next,
) -> Response {
    let Some(ConnectInfo(peer)) = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .copied()
    else {
        return Problem(Internal::new("the request carries no peer address")).into_response();
    };
    let client = Client {
        ip: client_ip(peer.ip(), request.headers(), &trusted),
        user_agent: user_agent(request.headers()),
    };
    request.extensions_mut().insert(client);
    next.run(request).await
}

/// The client IP of a request from `peer` (PLATFORM-047). Behind trusted proxies, it is the
/// rightmost address of `X-Forwarded-For` that isn't one of them: every proxy appends the address
/// it received the request from, so only the entries on the right were written by proxies the
/// API trusts.
fn client_ip(peer: IpAddr, headers: &HeaderMap, trusted: &[IpNet]) -> IpAddr {
    let is_trusted = |address: &IpAddr| trusted.iter().any(|range| range.contains(address));
    if !is_trusted(&peer) {
        return peer;
    }
    let mut client = peer;
    let forwarded = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .collect::<Vec<_>>();
    for entry in forwarded.into_iter().rev() {
        let Ok(address) = IpAddr::from_str(entry.trim()) else {
            return client;
        };
        if !is_trusted(&address) {
            return address;
        }
        client = address;
    }
    client
}

fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(|value| value.chars().take(USER_AGENT_MAX_CHARS).collect())
}

impl<S: Send + Sync> FromRequestParts<S> for Client {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<Self>()
            .cloned()
            .ok_or_else(|| Problem(Internal::new("the request carries no client")).into_response())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(forwarded: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", forwarded.parse().unwrap());
        headers
    }

    fn ranges(values: &[&str]) -> Vec<IpNet> {
        values.iter().map(|value| value.parse().unwrap()).collect()
    }

    const PROXY: &str = "172.18.0.5";

    #[test]
    fn platform_047_an_untrusted_peer_is_the_client_whatever_it_forwards() {
        let ip = client_ip(
            "198.51.100.9".parse().unwrap(),
            &headers("203.0.113.1"),
            &ranges(&["172.18.0.0/16"]),
        );
        assert_eq!(ip.to_string(), "198.51.100.9");
    }

    #[test]
    fn platform_047_behind_a_trusted_proxy_the_client_is_the_rightmost_untrusted_address() {
        let trusted = ranges(&["172.18.0.0/16", "192.0.2.0/24"]);
        let ip = client_ip(
            PROXY.parse().unwrap(),
            &headers("203.0.113.66, 198.51.100.7, 192.0.2.10"),
            &trusted,
        );
        assert_eq!(ip.to_string(), "198.51.100.7");
    }

    #[test]
    fn platform_047_a_walk_that_finds_no_untrusted_address_stops_at_the_last_trusted_proxy() {
        let trusted = ranges(&["172.18.0.0/16", "192.0.2.0/24"]);
        let peer: IpAddr = PROXY.parse().unwrap();
        assert_eq!(
            client_ip(peer, &headers("192.0.2.10"), &trusted).to_string(),
            "192.0.2.10"
        );
        assert_eq!(
            client_ip(peer, &headers("unknown, 192.0.2.10"), &trusted).to_string(),
            "192.0.2.10"
        );
        assert_eq!(client_ip(peer, &HeaderMap::new(), &trusted), peer);
    }
}
