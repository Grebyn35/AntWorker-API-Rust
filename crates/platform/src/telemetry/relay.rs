//! The relay of clients' batches: it forwards a batch of telemetry that a client sent the API to
//! the collector's receiver of such batches, over OTLP/HTTP (PLATFORM-091). It is a telemetry
//! adapter, with a client of its own and its endpoint from configuration (SEC-10).

use std::time::Duration;

use bytes::Bytes;
use futures_util::future::{Either, select};
use tracing::Instrument;

use super::{OtlpEndpoint, names};
use crate::{clock::SharedClock, errors::Internal};

/// How long the relay waits for the collector's answer, the connection's time included, within
/// the 10 seconds of a route's timeout (PLATFORM-042).
const TIMEOUT: Duration = Duration::from_secs(5);

/// The signal a batch holds, which names its path on the receiver.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Signal {
    /// Spans.
    Traces,
    /// Log records.
    Logs,
    /// Metrics.
    Metrics,
}

impl Signal {
    /// The protocol's path of the signal.
    const fn path(self) -> &'static str {
        match self {
            Self::Traces => "v1/traces",
            Self::Logs => "v1/logs",
            Self::Metrics => "v1/metrics",
        }
    }
}

/// Why the collector didn't take a batch.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    /// The collector refused the batch, with a 4xx other than 404, 405 and 429, as it does a
    /// body that isn't the protocol's request.
    #[error("the collector refused the batch")]
    Refused,
    /// The collector couldn't take the batch: no connection opened, no answer came in time, or
    /// it answered anything else but 2xx.
    #[error("the collector didn't take the batch")]
    Unavailable(#[source] Internal),
}

/// The relay couldn't be built, such as when the certificate store can't be read.
#[derive(Debug, thiserror::Error)]
#[error("the relay's HTTP client couldn't be built")]
pub struct RelayBuildError(#[source] Box<dyn std::error::Error + Send + Sync>);

/// The relay to one receiver of the collector.
#[derive(Clone)]
pub struct Relay {
    client: reqwest::Client,
    clock: SharedClock,
    host: String,
    traces: String,
    logs: String,
    metrics: String,
}

impl std::fmt::Debug for Relay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Relay")
            .field("traces", &self.traces)
            .field("logs", &self.logs)
            .field("metrics", &self.metrics)
            .finish_non_exhaustive()
    }
}

impl Relay {
    /// The relay to the receiver at `endpoint`, whose waits are measured by `clock`. It reaches
    /// the collector only when it forwards, so a collector that doesn't answer at startup keeps
    /// no route from serving. An `https` endpoint is verified against the operating system's
    /// certificate store (D38).
    pub fn new(endpoint: &OtlpEndpoint, clock: SharedClock) -> Result<Self, RelayBuildError> {
        let tls = crate::tls::client_config(endpoint.is_https(), None).map_err(RelayBuildError)?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .tls_backend_preconfigured(tls)
            .build()
            .map_err(|error| RelayBuildError(Box::new(error)))?;
        Ok(Self {
            client,
            clock,
            host: endpoint.host(),
            traces: endpoint.url(Signal::Traces.path()),
            logs: endpoint.url(Signal::Logs.path()),
            metrics: endpoint.url(Signal::Metrics.path()),
        })
    }

    /// Sends `batch`, a body of `media_type`, to the receiver's path of `signal`, in a client
    /// span (OBS-06), with no other header of the request it came in, and waits for the answer
    /// for 5 seconds on the clock. The collector took the batch when it answers 2xx.
    pub async fn forward(
        &self,
        signal: Signal,
        media_type: &'static str,
        batch: Bytes,
    ) -> Result<(), RelayError> {
        let span = tracing::info_span!(
            "http",
            { names::OTEL_NAME } = "POST",
            { names::OTEL_KIND } = "client",
            { names::HTTP_REQUEST_METHOD } = "POST",
            { names::SERVER_ADDRESS } = %self.host,
            { names::HTTP_RESPONSE_STATUS_CODE } = tracing::field::Empty,
        );
        let url = match signal {
            Signal::Traces => &self.traces,
            Signal::Logs => &self.logs,
            Signal::Metrics => &self.metrics,
        };
        let sending = self
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, media_type)
            .body(batch)
            .send()
            .instrument(span.clone());
        let deadline = self.clock.sleep(TIMEOUT);
        let status = match select(std::pin::pin!(sending), deadline).await {
            Either::Left((Ok(response), _)) => response.status().as_u16(),
            Either::Left((Err(error), _)) => {
                return Err(RelayError::Unavailable(Internal::new(error.without_url())));
            }
            Either::Right(((), _)) => {
                return Err(RelayError::Unavailable(Internal::new(format!(
                    "the collector didn't answer within {} seconds",
                    TIMEOUT.as_secs()
                ))));
            }
        };
        span.record(names::HTTP_RESPONSE_STATUS_CODE, i64::from(status));
        match status {
            200..=299 => Ok(()),
            // A 404 and a 405 say that nothing takes batches at the address, and nothing of the
            // batch, so they are no refusal of it (PLATFORM-091).
            400..=403 | 406..=428 | 430..=499 => Err(RelayError::Refused),
            _ => Err(RelayError::Unavailable(Internal::new(Answered(status)))),
        }
    }
}

/// The collector answered with a status that isn't a 2xx.
#[derive(Debug, thiserror::Error)]
#[error("the collector answered {0}")]
struct Answered(u16);
