//! The stream of server-sent events across a deploy of the reference deployment (REALTIME-004,
//! PLATFORM-062, OPS-14): the old replicas close their streams once their shutdown delay ends, and
//! a client that reconnects with its `Last-Event-ID` misses no event and receives none twice.

use std::{
    io::{BufRead, BufReader},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use serde_json::{Value, json};

use crate::{
    deployment::{Deployment, delete_as, post, streaming_client},
    docker::lossy,
};

/// How many members the workspace holds, more than a deploy's removals take.
const MEMBERS: u32 = 20_000;

/// How many removals may wait for the stream at once, fewer than a replay sends (REALTIME-004).
const AHEAD: usize = 50;

/// How many times the client opens its stream before it gives up.
const CONNECTIONS: usize = 30;

/// What the client of the stream saw.
#[derive(Debug, Default)]
struct Seen {
    /// The removed member of each `member.removed`, in the order they came.
    removed: Vec<String>,
    /// Each `stream.closed`'s reason.
    closed: Vec<String>,
    /// Whether a `stream.opened` said that its replay was truncated.
    truncated: bool,
    /// Each failure to open or read the stream.
    failures: Vec<String>,
}

/// A client of the stream, as an `EventSource` is: it reads the stream's frames, and reopens it
/// with the last `id` it received when it ends, on a thread of its own.
struct StreamClient {
    seen: Arc<Mutex<Seen>>,
    stop: Arc<AtomicBool>,
    thread: JoinHandle<()>,
}

impl StreamClient {
    /// Starts the client, which sends on `opened` once its first stream is open, and on
    /// `received` each member whose removal it received.
    fn start(
        root: String,
        url: String,
        token: String,
        opened: mpsc::Sender<()>,
        received: mpsc::Sender<String>,
    ) -> Self {
        let seen = Arc::new(Mutex::new(Seen::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (shared, stopping) = (seen.clone(), stop.clone());
        let thread = thread::spawn(move || {
            // Each read waits at most two heartbeats.
            let client = streaming_client(&root, Duration::from_secs(30));
            let mut last: Option<String> = None;
            for _ in 0..CONNECTIONS {
                if stopping.load(Ordering::Relaxed) {
                    return;
                }
                let mut request = client.get(&url).bearer_auth(&token);
                if let Some(id) = &last {
                    request = request.header("last-event-id", id);
                }
                let response = match request.send() {
                    Ok(response) if response.status() == 200 => response,
                    Ok(response) => {
                        shared
                            .lock()
                            .unwrap()
                            .failures
                            .push(response.status().to_string());
                        continue;
                    }
                    Err(error) => {
                        shared.lock().unwrap().failures.push(format!("{error:?}"));
                        continue;
                    }
                };
                let mut frame = (None::<String>, None::<String>, String::new());
                for line in BufReader::new(response).lines() {
                    let Ok(line) = line else {
                        break;
                    };
                    if !line.is_empty() {
                        match line.split_once(": ") {
                            Some(("id", id)) => frame.0 = Some(id.to_owned()),
                            Some(("event", event)) => frame.1 = Some(event.to_owned()),
                            Some(("data", data)) => frame.2.push_str(data),
                            _ => {}
                        }
                        continue;
                    }
                    let (id, event, data) = std::mem::take(&mut frame);
                    if id.is_some() {
                        last = id;
                    }
                    let data: Value = serde_json::from_str(&data).unwrap_or(Value::Null);
                    let mut seen = shared.lock().unwrap();
                    match event.as_deref() {
                        Some("member.removed") => {
                            let member = data["data"]["user_id"].as_str().unwrap().to_owned();
                            seen.removed.push(member.clone());
                            drop(seen);
                            // A remover that has stopped waits for nothing more.
                            received.send(member).unwrap_or_default();
                        }
                        Some("stream.opened") => {
                            seen.truncated |= data["data"]["truncated"] == true;
                            // Only the first opening has a test that waits for it.
                            opened.send(()).unwrap_or_default();
                        }
                        Some("stream.closed") => {
                            seen.closed
                                .push(data["data"]["reason"].as_str().unwrap().to_owned());
                        }
                        _ => {}
                    }
                    if stopping.load(Ordering::Relaxed) {
                        return;
                    }
                }
            }
        });
        Self { seen, stop, thread }
    }

    /// Stops the client, at its next frame, and returns what it saw.
    fn stop(self) -> Seen {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.join().expect("the client of the stream");
        Arc::try_unwrap(self.seen)
            .expect("the client's record")
            .into_inner()
            .unwrap()
    }
}

/// Removes the workspace's members one after another, from a thread of its own, until stopped,
/// with at most [`AHEAD`] of them not yet received by the stream, then waits until the stream has
/// received them all, and returns them in the order it removed them.
struct Remover {
    stop: Arc<AtomicBool>,
    thread: JoinHandle<Vec<String>>,
}

impl Remover {
    fn start(
        root: String,
        url: String,
        token: String,
        members: Vec<String>,
        received: mpsc::Receiver<String>,
    ) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let thread = thread::spawn(move || {
            let mut removed = Vec::new();
            let mut waiting = 0;
            for member in members {
                if stopping.load(Ordering::Relaxed) {
                    break;
                }
                while waiting >= AHEAD {
                    // A client that gave up drops its sender, which ends the wait.
                    if received.recv().is_err() {
                        return removed;
                    }
                    waiting -= 1;
                }
                let status = delete_as(root.clone(), format!("{url}/{member}"), token.clone());
                assert_eq!(status, 204, "the removal of {member}");
                removed.push(member);
                waiting += 1;
            }
            for _ in 0..waiting {
                if received.recv().is_err() {
                    break;
                }
            }
            removed
        });
        Self { stop, thread }
    }

    fn stop(self) -> Vec<String> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.join().expect("the removals")
    }
}

#[tokio::test]
async fn realtime_004_a_deploy_with_an_open_stream_loses_no_event() {
    let deployment = Deployment::managed(&[("RATE_LIMIT_USER_REQUESTS", "1000000")]);
    let first = deployment.apply(&deployment.api, &[]).await;
    assert!(first.status.success(), "{}", lossy(&first));
    let root = deployment.server.caddy_root();
    let (registered, session) = post(
        root.clone(),
        deployment.url("https", 443, "/v1/users"),
        json!({ "email": "ada@example.com", "password": "plum-tractor-41-quiet", "name": "Ada" })
            .to_string(),
    );
    assert_eq!(registered, 201, "{session}");
    let token = serde_json::from_str::<Value>(&session).unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let workspace = deployment.owned_workspace("Analytical", "ada@example.com");
    let members = deployment.sql(&format!(
        "WITH added AS (INSERT INTO users (email, name) \
         SELECT 'member' || n || '@example.com', 'Member ' || n \
         FROM generate_series(1, {MEMBERS}) AS n RETURNING id) \
         INSERT INTO workspace_memberships (workspace_id, user_id, role) \
         SELECT '{workspace}', id, 'member' FROM added RETURNING user_id"
    ));
    let members: Vec<String> = members.lines().map(str::to_owned).collect();

    let (sender, receiver) = mpsc::channel();
    let (opening, opened) = mpsc::channel();
    let client = StreamClient::start(
        root.clone(),
        deployment.url("https", 443, "/v1/users/me/events"),
        token.clone(),
        opening,
        sender,
    );
    opened.recv().expect("the stream opened");
    let remover = Remover::start(
        root,
        deployment.url("https", 443, &format!("/v1/workspaces/{workspace}/members")),
        token,
        members,
        receiver,
    );
    let second = deployment.apply(&deployment.api, &[]).await;
    let removed = remover.stop();
    let seen = client.stop();

    assert!(second.status.success(), "{}", lossy(&second));
    assert!(removed.len() > AHEAD, "only {} removals", removed.len());
    assert!(seen.failures.is_empty(), "{:?}", seen.failures);
    assert!(!seen.truncated);
    // The old replicas closed the stream as they shut down, and the client reconnected.
    assert!(
        seen.closed.iter().any(|reason| reason == "shutdown"),
        "{:?}",
        seen.closed
    );
    assert_eq!(seen.removed.len(), removed.len());
    assert!(
        seen.removed == removed,
        "the stream's events differ from the removals"
    );
}
