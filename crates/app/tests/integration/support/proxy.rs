//! A proxy between the API under test and Postgres, which can stop forwarding on the connections
//! it holds, as a database that stops answering on them does, while it still forwards new ones.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpListener, TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::watch,
};

/// A proxy on the loopback address that forwards each connection to Postgres.
pub struct Proxy {
    port: u16,
    accepted: Arc<AtomicU64>,
    /// The connections numbered below it forward nothing.
    frozen: watch::Sender<u64>,
}

impl Proxy {
    /// A proxy that forwards each connection it accepts to `address`, the host and the port of
    /// Postgres.
    pub async fn to(address: (String, u16)) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind a port");
        let port = listener.local_addr().expect("the port").port();
        let accepted = Arc::new(AtomicU64::new(0));
        let (frozen, _) = watch::channel(0);
        let forwarding = (accepted.clone(), frozen.clone());
        tokio::spawn(async move {
            let (accepted, frozen) = forwarding;
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
                ));
                tokio::spawn(forward(
                    server_reads,
                    client_writes,
                    number,
                    frozen.subscribe(),
                ));
            }
        });
        Self {
            port,
            accepted,
            frozen,
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

    /// Forwards on every connection again.
    pub fn thaw(&self) {
        self.frozen.send_replace(0);
    }
}

/// Copies what `from` reads to `to`, holding it while the connection `number` is frozen.
async fn forward(
    mut from: OwnedReadHalf,
    mut to: OwnedWriteHalf,
    number: u64,
    mut frozen: watch::Receiver<u64>,
) {
    let mut buffer = vec![0; 16 * 1024];
    loop {
        let read = match from.read(&mut buffer).await {
            Ok(0) | Err(_) => break,
            Ok(read) => read,
        };
        if frozen.wait_for(|below| number >= *below).await.is_err()
            || to.write_all(&buffer[..read]).await.is_err()
        {
            break;
        }
    }
    // Dropping `to` shuts its direction of the connection down.
}
