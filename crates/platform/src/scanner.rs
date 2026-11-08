//! The client of ClamAV's daemon, clamd, which scans uploads for malware (the files spec's
//! "Scanning", the platform spec's Scanner). It reports a scanner that doesn't answer as
//! unavailable, and every other failure as failed, with a cause written here, so no error of the
//! connection leaves it (ERR-04).

use std::{io, sync::Arc, time::Duration};

use futures_util::future::{Either, select};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};
use tracing::Instrument;

use crate::{
    clock::SharedClock,
    errors::Internal,
    telemetry::{metrics::Counter, names},
};

/// How long opening the connection to clamd may take.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a scan may take from its start until clamd's answer, the connection included: enough for the 19
/// seconds that 20 MiB of a Word document take on the development server, within the upload
/// route's 60 seconds (the files spec's "Scanning").
const SCAN_TIMEOUT: Duration = Duration::from_secs(30);

/// The most bytes a chunk of the stream holds.
const CHUNK_BYTES: usize = 1 << 20;

/// The most bytes of clamd's answer the client reads.
const ANSWER_BYTES: usize = 4096;

/// The command that starts a scan of a stream: `INSTREAM`, whose `z` prefix asks for an answer
/// that ends in a zero byte.
const INSTREAM: &[u8] = b"zINSTREAM\0";

static SCANS: Counter<1> = Counter::new(
    "platform.scans",
    "{scan}",
    "Scans by the malware scanner, by how each ended: clean, found, when it named a signature, \
     failed, when it answered with an error or with what the client couldn't read, or \
     unavailable, when it couldn't be reached, closed the connection, or didn't answer in time \
     (PLATFORM-077).",
    [names::SCANNER_OUTCOME],
);

/// What reaching the scanner takes (PLATFORM-077).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScannerConfig {
    /// clamd's host, such as `scanner`.
    pub host: String,
    /// clamd's port, such as 3310.
    pub port: u16,
}

/// The scanner.
#[derive(Clone)]
pub struct Scanner {
    host: Arc<str>,
    port: u16,
    clock: SharedClock,
}

impl std::fmt::Debug for Scanner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Scanner")
            .field("host", &self.host)
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

/// What a scan found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// No signature: clamd answered `OK`.
    Clean,
    /// The signature clamd named, such as `Eicar-Test-Signature`.
    Found(String),
}

/// A scan that ended without clamd's verdict.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// clamd couldn't be reached, closed the connection before its answer, or didn't answer in
    /// time.
    #[error("the scanner didn't answer")]
    Unavailable(#[source] Internal),
    /// clamd answered with an error, or with what the client couldn't read.
    #[error("the scanner's scan failed")]
    Failed(#[source] Internal),
}

impl Scanner {
    /// The client of the scanner that `config` names, whose waits are measured by `clock`. It
    /// reaches clamd only when asked to scan, so a scanner that doesn't answer at startup keeps no
    /// route from serving.
    pub fn new(config: &ScannerConfig, clock: SharedClock) -> Self {
        Self {
            host: config.host.as_str().into(),
            port: config.port,
            clock,
        }
    }

    /// Has clamd scan `bytes`, on a connection of its own, in the scan's client span (OBS-06),
    /// within 30 seconds of its start on the clock, and counts how it ended.
    pub async fn scan(&self, bytes: &[u8]) -> Result<Verdict, ScanError> {
        let deadline = self.clock.sleep(SCAN_TIMEOUT);
        let scanning = async {
            let stream = self.connect().await?;
            exchange(stream, bytes).await
        };
        let scanned = async {
            match select(std::pin::pin!(scanning), deadline).await {
                Either::Left((scanned, _)) => scanned,
                Either::Right(((), _)) => Err(ScanError::Unavailable(Internal::new(format!(
                    "the scanner didn't answer within {} seconds",
                    SCAN_TIMEOUT.as_secs()
                )))),
            }
        }
        .instrument(self.span())
        .await;
        count(&scanned);
        scanned
    }

    /// Opens the connection to clamd, within 2 seconds on the clock.
    async fn connect(&self) -> Result<TcpStream, ScanError> {
        let wait = self.clock.sleep(CONNECT_TIMEOUT);
        let connecting = TcpStream::connect((&*self.host, self.port));
        match select(std::pin::pin!(connecting), wait).await {
            Either::Left((Ok(stream), _)) => Ok(stream),
            Either::Left((Err(error), _)) => Err(ScanError::Unavailable(Internal::new(format!(
                "the connection to the scanner failed: {:?}",
                error.kind()
            )))),
            Either::Right(((), _)) => Err(ScanError::Unavailable(Internal::new(format!(
                "the connection to the scanner didn't open within {} seconds",
                CONNECT_TIMEOUT.as_secs()
            )))),
        }
    }

    /// The scan's client span (OBS-06), which names the scanner's address.
    fn span(&self) -> tracing::Span {
        tracing::info_span!(
            "clamd",
            { names::OTEL_NAME } = "clamd INSTREAM",
            { names::OTEL_KIND } = "client",
            { names::SERVER_ADDRESS } = &*self.host,
            { names::SERVER_PORT } = self.port,
        )
    }
}

/// Sends `bytes` on `stream` as clamd's `INSTREAM` takes them, and reads its answer. clamd may
/// answer before the stream ends, as it does a stream longer than it takes, and close the
/// connection, so a write that fails still reads what clamd answered.
async fn exchange(mut stream: TcpStream, bytes: &[u8]) -> Result<Verdict, ScanError> {
    let sent = send(&mut stream, bytes).await;
    let answer = read_answer(&mut stream).await;
    match (sent, answer) {
        (_, Ok(answer)) if !answer.is_empty() => verdict(&answer),
        (Err(error), _) | (Ok(()), Err(error)) => Err(ScanError::Unavailable(Internal::new(
            format!("the connection to the scanner broke: {:?}", error.kind()),
        ))),
        (Ok(()), Ok(_)) => Err(ScanError::Unavailable(Internal::new(
            "the scanner closed the connection before its answer",
        ))),
    }
}

/// Writes the command, then `bytes` in chunks, each after its length as 4 bytes in network
/// order, then a chunk of no bytes, which ends the stream.
async fn send(stream: &mut TcpStream, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(INSTREAM).await?;
    for chunk in bytes.chunks(CHUNK_BYTES) {
        let length = u32::try_from(chunk.len()).map_err(io::Error::other)?;
        stream.write_all(&length.to_be_bytes()).await?;
        stream.write_all(chunk).await?;
    }
    stream.write_all(&0_u32.to_be_bytes()).await?;
    stream.flush().await
}

/// clamd's answer, up to its zero byte, or to the end of the connection, and at most
/// `ANSWER_BYTES`.
async fn read_answer(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut answer = Vec::new();
    let mut buffer = [0_u8; 512];
    while answer.len() < ANSWER_BYTES {
        let read = stream.read(&mut buffer).await?;
        let Some(read) = buffer.get(..read).filter(|read| !read.is_empty()) else {
            break;
        };
        if let Some(end) = read.iter().position(|byte| *byte == 0) {
            answer.extend_from_slice(read.get(..end).unwrap_or_default());
            return Ok(answer);
        }
        answer.extend_from_slice(read);
    }
    answer.truncate(ANSWER_BYTES);
    Ok(answer)
}

/// The verdict that clamd's `answer` gives: `stream: OK`, or `stream: <signature> FOUND`. Any
/// other answer, such as one that ends in `ERROR`, is a failure, whose cause holds the answer,
/// which names neither the bytes nor a signature.
fn verdict(answer: &[u8]) -> Result<Verdict, ScanError> {
    let text = String::from_utf8_lossy(answer);
    let text = text.trim_end();
    if text == "stream: OK" {
        return Ok(Verdict::Clean);
    }
    if let Some(signature) = text
        .strip_prefix("stream: ")
        .and_then(|rest| rest.strip_suffix(" FOUND"))
        .filter(|signature| !signature.is_empty())
    {
        return Ok(Verdict::Found(signature.to_owned()));
    }
    if text.ends_with("ERROR") {
        return Err(ScanError::Failed(Internal::new(format!(
            "the scanner answered with an error: {}",
            text.chars()
                .filter(|character| !character.is_control())
                .take(200)
                .collect::<String>()
        ))));
    }
    Err(ScanError::Failed(Internal::new(
        "the scanner answered what the client couldn't read",
    )))
}

/// Counts a scan that ended as `scanned`.
fn count(scanned: &Result<Verdict, ScanError>) {
    let outcome = match scanned {
        Ok(Verdict::Clean) => "clean",
        Ok(Verdict::Found(_)) => "found",
        Err(ScanError::Failed(_)) => "failed",
        Err(ScanError::Unavailable(_)) => "unavailable",
    };
    SCANS.add(1, [outcome.into()]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_077_clamd_s_answers_give_a_verdict_or_a_failure() {
        assert_eq!(verdict(b"stream: OK").unwrap(), Verdict::Clean);
        assert_eq!(
            verdict(b"stream: Eicar-Test-Signature FOUND").unwrap(),
            Verdict::Found("Eicar-Test-Signature".to_owned())
        );
        assert!(matches!(
            verdict(b"INSTREAM size limit exceeded. ERROR"),
            Err(ScanError::Failed(_))
        ));
        assert!(matches!(
            verdict(b"stream:  FOUND"),
            Err(ScanError::Failed(_))
        ));
        assert!(matches!(verdict(b"PONG"), Err(ScanError::Failed(_))));
    }
}
