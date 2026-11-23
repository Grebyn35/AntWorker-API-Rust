//! A proxy between the API under test and Postgres, which can stop forwarding on the connections
//! it holds, as a database that stops answering on them does, while it still forwards new ones,
//! and can hold back what Postgres sends from a `BEGIN` on, as a slow network would.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpListener, TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::watch,
};

/// The message that opens a transaction, as sqlx sends it: a simple query, `Q`, of the length
/// 10, which counts itself, and the text `BEGIN`, ended by a zero.
const BEGIN: &[u8] = b"Q\0\0\0\x0aBEGIN\0";

/// A proxy on the loopback address that forwards each connection to Postgres.
pub struct Proxy {
    port: u16,
    accepted: Arc<AtomicU64>,
    /// The connections numbered below it forward nothing.
    frozen: watch::Sender<u64>,
    /// Whether the next `BEGIN` that a connection sends starts the hold of what Postgres sends.
    armed: Arc<AtomicBool>,
    /// Whether every connection holds what Postgres sends it.
    holding: watch::Sender<bool>,
}

impl Proxy {
    /// A proxy that forwards each connection it accepts to `address`, the host and the port of
    /// Postgres.
    pub async fn to(address: (String, u16)) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind a port");
        let port = listener.local_addr().expect("the port").port();
        let accepted = Arc::new(AtomicU64::new(0));
        let (frozen, _) = watch::channel(0);
        let armed = Arc::new(AtomicBool::new(false));
        let (holding, _) = watch::channel(false);
        let forwarding = (
            accepted.clone(),
            frozen.clone(),
            armed.clone(),
            holding.clone(),
        );
        tokio::spawn(async move {
            let (accepted, frozen, armed, holding) = forwarding;
            while let Ok((client, _)) = listener.accept().await {
                let number = accepted.fetch_add(1, Ordering::SeqCst);
                let Ok(server) = TcpStream::connect((address.0.as_str(), address.1)).await else {
                    continue;
                };
                let (client_reads, client_writes) = client.into_split();
                let (server_reads, server_writes) = server.into_split();
                tokio::spawn(forward(
                    client_reads,
                    server_writes,
                    number,
                    frozen.subscribe(),
                    Side::Client(armed.clone(), holding.clone()),
                ));
                tokio::spawn(forward(
                    server_reads,
                    client_writes,
                    number,
                    frozen.subscribe(),
                    Side::Postgres(holding.subscribe()),
                ));
            }
        });
        Self {
            port,
            accepted,
            frozen,
            armed,
            holding,
        }
    }

    /// The port the proxy listens on.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Stops forwarding, both ways, on every connection accepted so far.
    pub fn freeze(&self) {
        self.frozen
            .send_replace(self.accepted.load(Ordering::SeqCst));
    }

    /// Holds what Postgres sends on every connection, from just before the proxy forwards the
    /// next `BEGIN` that one of the proxy's reads holds whole, until [`Proxy::thaw`]: Postgres
    /// then opens the transaction, and the connection that sent the `BEGIN` doesn't hear so.
    pub fn hold_answers_from_the_next_begin(&self) {
        self.armed.store(true, Ordering::SeqCst);
    }

    /// Forwards on every connection again, both ways, and holds nothing from a `BEGIN` on.
    pub fn thaw(&self) {
        self.armed.store(false, Ordering::SeqCst);
        self.holding.send_replace(false);
        self.frozen.send_replace(0);
    }
}

/// The side of a connection that a direction of it reads.
enum Side {
    /// The API's side, which starts the hold of what Postgres sends when it sends a `BEGIN`
    /// while the proxy is armed.
    Client(Arc<AtomicBool>, watch::Sender<bool>),
    /// Postgres's side, whose bytes wait while the proxy holds them.
    Postgres(watch::Receiver<bool>),
}

/// Copies what `from` reads to `to`, holding it while the connection `number` is frozen, and,
/// on Postgres's side, while the proxy holds what Postgres sends.
async fn forward(
    mut from: OwnedReadHalf,
    mut to: OwnedWriteHalf,
    number: u64,
    mut frozen: watch::Receiver<u64>,
    mut side: Side,
) {
    let mut buffer = vec![0; 16 * 1024];
    loop {
        let read = match from.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        let forwarded = match &mut side {
            // The hold starts before the `BEGIN` reaches Postgres, so its answer is held.
            Side::Client(armed, holding) => {
                if buffer[..read]
                    .windows(BEGIN.len())
                    .any(|bytes| bytes == BEGIN)
                    && armed.swap(false, Ordering::SeqCst)
                {
                    holding.send_replace(true);
                }
                true
            }
            Side::Postgres(holding) => holding.wait_for(|holding| !*holding).await.is_ok(),
        };
        if !forwarded
            || frozen.wait_for(|below| number >= *below).await.is_err()
            || to.write_all(&buffer[..read]).await.is_err()
        {
            break;
        }
    }
    // Dropping `to` shuts its direction of the connection down.
}
