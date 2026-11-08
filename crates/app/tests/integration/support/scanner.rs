//! The tests' malware scanner: the clamd that `just services` starts, which knows one signature,
//! of the EICAR test file (the files spec's "Scanning"), and a stand-in that answers as a test
//! needs (TEST-04).

use std::sync::Arc;

use platform::{
    clock::TestClock,
    scanner::{Scanner, ScannerConfig},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
    task::JoinHandle,
};

/// The address of the clamd that `just services` starts.
pub const ADDRESS: &str = "127.0.0.1:3310";

/// The signature that the clamd of `just services` finds in [`eicar`]: deploy/scanner/eicar.ndb's,
/// which clamd names unofficial, since its own signatures don't hold it.
pub const EICAR_SIGNATURE: &str = "Eicar-Test-Signature.UNOFFICIAL";

/// The EICAR test file, the antivirus industry's standard harmless file that every scanner finds,
/// put together here so that no file of the repository holds it, and no scanner on a developer's
/// machine takes this one for it.
pub fn eicar() -> Vec<u8> {
    let halves = [
        "X5O!P%@AP[4\\PZX54(P^)7CC)7}$EICAR",
        "-STANDARD-ANTIVIRUS-TEST-FILE!$H+H*",
    ];
    halves.concat().into_bytes()
}

/// The client of the clamd that `just services` starts, as the API builds it, on a clock that
/// never moves, so it waits for each answer as long as clamd takes.
pub fn scanner() -> Scanner {
    client(ADDRESS, Arc::new(TestClock::default()))
}

/// The client of the scanner at `address`, whose waits `clock` measures, with the tests'
/// telemetry installed, which counts its scans.
pub fn client(address: &str, clock: Arc<TestClock>) -> Scanner {
    platform::telemetry::testing::install(super::api::SERVICE).unwrap();
    let (host, port) = address.rsplit_once(':').unwrap();
    Scanner::new(
        &ScannerConfig {
            host: host.to_owned(),
            port: port.parse().unwrap(),
        },
        clock,
    )
}

/// How a stand-in scanner answers a stream once it has read it whole.
#[derive(Clone, Copy, Debug)]
pub enum Script {
    /// With this line, ended by a zero byte, as clamd ends its answers.
    Answer(&'static str),
    /// Never, holding the connection open until the stand-in is dropped.
    Never,
    /// By closing the connection.
    Close,
}

/// A stand-in for clamd, in place of the one `just services` starts (TEST-04).
pub struct Scripted {
    /// Its address, for `CLAMD_ADDRESS`.
    pub address: String,
    /// Notified each time it has read a stream whole.
    pub read: Arc<Notify>,
    server: JoinHandle<()>,
}

impl Drop for Scripted {
    fn drop(&mut self) {
        self.server.abort();
    }
}

/// A scanner that reads each stream whole, as clamd's `INSTREAM` sends it, and answers it as
/// `script` says.
pub async fn scripted(script: Script) -> Scripted {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let read = Arc::new(Notify::new());
    let notified = read.clone();
    let server = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            tokio::spawn(answering(stream, script, notified.clone()));
        }
    });
    Scripted {
        address,
        read,
        server,
    }
}

/// Reads the command and the stream's chunks on `stream` up to the chunk of no bytes, notifies
/// `read`, then answers as `script` says.
async fn answering(mut stream: TcpStream, script: Script, read: Arc<Notify>) {
    let mut command = [0; 10];
    if stream.read_exact(&mut command).await.is_err() {
        return;
    }
    loop {
        let mut length = [0; 4];
        if stream.read_exact(&mut length).await.is_err() {
            return;
        }
        let length = usize::try_from(u32::from_be_bytes(length)).unwrap();
        if length == 0 {
            break;
        }
        let mut chunk = vec![0; length];
        if stream.read_exact(&mut chunk).await.is_err() {
            return;
        }
    }
    read.notify_one();
    match script {
        Script::Answer(line) => {
            let mut answer = line.as_bytes().to_vec();
            answer.push(0);
            // A client that left before the answer reads nothing of it.
            stream.write_all(&answer).await.unwrap_or(());
        }
        Script::Never => std::future::pending::<()>().await,
        Script::Close => drop(stream),
    }
}
