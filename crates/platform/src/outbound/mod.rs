//! The clients through which application code sends HTTP requests to other systems (SEC-10). The
//! configured client calls URLs that come from configuration or from the code, such as an OAuth
//! provider's endpoints: it requires HTTPS, unless the environment is development or test, follows
//! no redirect, and caps each answer's size and time. The untrusted client, in `untrusted.rs`,
//! calls URLs that come from a third party.

mod untrusted;

use std::time::Duration;

use serde::de::DeserializeOwned;
use tracing::Instrument;

pub use untrusted::{FetchError, Untrusted};

use crate::{config::Environment, telemetry::names, types::Secret};

/// How long the configured client waits for a connection (the auth spec's "The requests").
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

/// How long the configured client waits for a whole answer, the connection's time included,
/// within the 10 seconds of a route's timeout (PLATFORM-042).
const TIMEOUT: Duration = Duration::from_secs(6);

/// The most bytes of an answer's body that the configured client reads.
const MAX_BODY_BYTES: usize = 1 << 20;

/// The settings of the outbound clients.
#[derive(Clone, Debug, Default)]
pub struct OutboundConfig {
    /// In the test environment only, the loopback origin that the configured client sends every
    /// request to, keeping its path, and that the untrusted client accepts, so tests reach a
    /// stand-in of another system, such as an OAuth provider (SEC-10). Startup refuses it in any
    /// other environment.
    pub test_origin: Option<url::Url>,
}

/// The client that calls URLs that come from configuration or from the code (SEC-10).
#[derive(Clone, Debug)]
pub struct Configured {
    client: reqwest::Client,
    https_only: bool,
    test_origin: Option<url::Url>,
}

/// A request that the configured client sends. Its `Debug` shows its method and its URL's host
/// alone: its body may hold a client's secret or a code, and its credentials are secrets
/// (SEC-01).
pub struct Request {
    method: reqwest::Method,
    url: String,
    headers: Vec<(&'static str, String)>,
    basic: Option<(String, Secret<String>)>,
    bearer: Option<Secret<String>>,
    body: Option<(&'static str, Vec<u8>)>,
}

impl Request {
    /// A `GET` of `url`.
    #[must_use]
    pub fn get(url: impl Into<String>) -> Self {
        Self::new(reqwest::Method::GET, url.into())
    }

    /// A `POST` to `url`.
    #[must_use]
    pub fn post(url: impl Into<String>) -> Self {
        Self::new(reqwest::Method::POST, url.into())
    }

    /// A `DELETE` of `url`.
    #[must_use]
    pub fn delete(url: impl Into<String>) -> Self {
        Self::new(reqwest::Method::DELETE, url.into())
    }

    fn new(method: reqwest::Method, url: String) -> Self {
        Self {
            method,
            url,
            headers: Vec::new(),
            basic: None,
            bearer: None,
            body: None,
        }
    }

    /// The request, with the header `name` set to `value`.
    #[must_use]
    pub fn header(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    /// The request, authenticated by `token` in a Bearer header.
    #[must_use]
    pub fn bearer(mut self, token: Secret<String>) -> Self {
        self.bearer = Some(token);
        self
    }

    /// The request, authenticated as `user` with `password` through HTTP's Basic scheme.
    #[must_use]
    pub fn basic(mut self, user: &str, password: &Secret<String>) -> Self {
        self.basic = Some((user.to_owned(), Secret::new(password.expose().clone())));
        self
    }

    /// The request, with `fields` as its body, as an HTML form encodes them.
    #[must_use]
    pub fn form(mut self, fields: &[(&str, &str)]) -> Self {
        let encoded = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(fields)
            .finish();
        self.body = Some(("application/x-www-form-urlencoded", encoded.into_bytes()));
        self
    }

    /// The request, with `value` as its JSON body.
    #[must_use]
    pub fn json(mut self, value: &serde_json::Value) -> Self {
        self.body = Some(("application/json", value.to_string().into_bytes()));
        self
    }
}

impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let host = url::Url::parse(&self.url)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default();
        f.debug_struct("Request")
            .field("method", &self.method)
            .field("host", &host)
            .finish_non_exhaustive()
    }
}

/// An answer that the configured client read whole. Its `Debug` shows its status and its body's
/// length alone, since the body may hold a token (SEC-01).
pub struct Response {
    /// Its status code.
    pub status: u16,
    /// Its body, of at most a mebibyte.
    pub body: Vec<u8>,
}

impl std::fmt::Debug for Response {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Response")
            .field("status", &self.status)
            .field("body_bytes", &self.body.len())
            .finish()
    }
}

impl Response {
    /// Whether its status is a success, 2xx.
    #[must_use]
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Its body as JSON of the type `T`, or `None` when it isn't.
    #[must_use]
    pub fn json<T: DeserializeOwned>(&self) -> Option<T> {
        serde_json::from_slice(&self.body).ok()
    }
}

/// Why a request got no answer that the client read whole. It holds its cause, for the logs
/// alone (ERR-04): never the request's credentials or the answer's body.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct OutboundError(String);

impl Configured {
    /// The configured client of a deployment in `environment`, with `config`'s settings.
    ///
    /// # Errors
    ///
    /// When the TLS configuration, which trusts the operating system's certificates, can't be
    /// built.
    pub fn new(environment: Environment, config: &OutboundConfig) -> Result<Self, OutboundError> {
        let tls = crate::tls::client_config(true, None)
            .map_err(|error| OutboundError(format!("the TLS configuration: {error}")))?;
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .tls_backend_preconfigured(tls)
            .build()
            .map_err(|error| OutboundError(format!("the client: {error}")))?;
        Ok(Self {
            client,
            https_only: environment == Environment::Production,
            test_origin: config.test_origin.clone(),
        })
    }

    /// Sends `request`, and reads its answer whole. A redirect is an answer like any other, whose
    /// status the caller reads.
    ///
    /// # Errors
    ///
    /// When the URL is invalid, or isn't HTTPS in production; when no answer comes within the
    /// client's time limits; or when its body is over a mebibyte.
    pub async fn send(&self, request: Request) -> Result<Response, OutboundError> {
        let url = self.target(&request.url)?;
        let host = url.host_str().unwrap_or_default().to_owned();
        let span = tracing::info_span!(
            "http",
            { names::OTEL_NAME } = %request.method,
            { names::OTEL_KIND } = "client",
            { names::HTTP_REQUEST_METHOD } = %request.method,
            { names::SERVER_ADDRESS } = %host,
            { names::HTTP_RESPONSE_STATUS_CODE } = tracing::field::Empty,
        );
        let answered = self
            .answer(request, url, host)
            .instrument(span.clone())
            .await;
        if let Ok(response) = &answered {
            span.record(names::HTTP_RESPONSE_STATUS_CODE, response.status);
        }
        answered
    }

    /// Sends `request` to `url`, of the host `host`, and reads its answer whole.
    async fn answer(
        &self,
        request: Request,
        url: url::Url,
        host: String,
    ) -> Result<Response, OutboundError> {
        let mut builder = self.client.request(request.method.clone(), url);
        for (name, value) in &request.headers {
            builder = builder.header(*name, value);
        }
        if let Some((user, password)) = &request.basic {
            builder = builder.basic_auth(user, Some(password.expose()));
        }
        if let Some(token) = &request.bearer {
            builder = builder.bearer_auth(token.expose());
        }
        if let Some((content_type, body)) = request.body {
            builder = builder
                .header(reqwest::header::CONTENT_TYPE, content_type)
                .body(body);
        }
        let failed = |error: reqwest::Error| {
            OutboundError(format!(
                "{} to {host}: {}",
                request.method,
                error.without_url()
            ))
        };
        let mut response = builder.send().await.map_err(failed)?;
        let status = response.status().as_u16();
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(failed)? {
            if body.len() + chunk.len() > MAX_BODY_BYTES {
                return Err(OutboundError(format!(
                    "{} to {host}: the answer is over {MAX_BODY_BYTES} bytes",
                    request.method
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(Response { status, body })
    }

    /// The URL that `url` is sent to: itself, or, with a test origin, the origin with its path
    /// and query.
    fn target(&self, url: &str) -> Result<url::Url, OutboundError> {
        let mut parsed =
            url::Url::parse(url).map_err(|_| OutboundError(format!("{url} isn't a URL")))?;
        if let Some(origin) = &self.test_origin {
            let mut moved = origin.clone();
            moved.set_path(parsed.path());
            moved.set_query(parsed.query());
            parsed = moved;
        }
        if self.https_only && parsed.scheme() != "https" {
            return Err(OutboundError(format!(
                "{url} isn't HTTPS, which the configured client requires in production (SEC-10)"
            )));
        }
        Ok(parsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client(environment: Environment, origin: Option<&str>) -> Configured {
        Configured::new(
            environment,
            &OutboundConfig {
                test_origin: origin.map(|origin| url::Url::parse(origin).unwrap()),
            },
        )
        .unwrap()
    }

    #[test]
    fn sec_10_the_configured_client_refuses_plain_http_in_production() {
        let production = client(Environment::Production, None);
        assert!(production.target("http://example.com/token").is_err());
        assert!(production.target("https://example.com/token").is_ok());
        let development = client(Environment::Development, None);
        assert!(development.target("http://127.0.0.1:8080/token").is_ok());
    }

    #[test]
    fn sec_10_the_test_origin_keeps_each_request_s_path_and_query() {
        let test = client(Environment::Test, Some("http://127.0.0.1:4000"));
        let target = test
            .target("https://provider.test/user/emails?page=1")
            .unwrap();
        assert_eq!(target.as_str(), "http://127.0.0.1:4000/user/emails?page=1");
    }
}
