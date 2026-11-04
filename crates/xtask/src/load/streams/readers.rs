//! The measured replica's streams, each read on a thread of its own, and what they received
//! (OPS-16).

use std::{
    collections::HashSet,
    sync::{Arc, Mutex, mpsc},
    thread,
};

use super::{STREAMS_PER_USER, client::Stream};

/// What a stream's thread tells the task.
pub(super) enum News {
    /// The stream sent `stream.opened`.
    Opened,
    /// The stream sent `stream.closed`, or ended, or failed.
    Ended(usize, String),
}

/// The events each stream received, by their IDs, in order.
pub(super) type Received = Arc<Mutex<Vec<Vec<String>>>>;

/// The open streams: what each received, and what their threads tell.
pub(super) struct Streams {
    pub(super) received: Received,
    pub(super) news: mpsc::Receiver<News>,
}

/// Opens 10 streams for each of `tokens`' users on the hub at `address`, each read on a thread of
/// its own, one after another, as clients connect over time rather than all at once, which the
/// concurrency limit would shed (PLATFORM-039), and returns once every one has sent
/// `stream.opened`. A stream that the replica doesn't answer ends at its socket's timeout, so each
/// one tells.
pub(super) fn open_streams(address: &str, tokens: &[String]) -> Result<Streams, String> {
    let count = tokens.len() * STREAMS_PER_USER;
    let received: Received = Arc::new(Mutex::new(vec![Vec::new(); count]));
    let (tell, news) = mpsc::channel();
    for index in 0..count {
        let token = tokens[index / STREAMS_PER_USER].clone();
        let address = address.to_owned();
        let received = received.clone();
        let tell = tell.clone();
        thread::spawn(move || {
            let ended = read(&address, &token, index, &received, &tell);
            // The task may have finished, and the receiver gone with it.
            tell.send(News::Ended(index, ended)).unwrap_or_default();
        });
        match news.recv() {
            Ok(News::Opened) => {}
            Ok(News::Ended(index, why)) => {
                return Err(format!("stream {index} ended before it opened: {why}"));
            }
            Err(error) => return Err(format!("the streams didn't all open: {error}")),
        }
    }
    Ok(Streams { received, news })
}

/// Reads the stream `index` of `token`'s caller until it ends, and says why it ended.
fn read(
    address: &str,
    token: &str,
    index: usize,
    received: &Received,
    tell: &mpsc::Sender<News>,
) -> String {
    let mut stream = match Stream::open(address, token) {
        Ok(stream) => stream,
        Err(error) => return error,
    };
    loop {
        match stream.next() {
            Ok(Some(frame)) => match frame.event.as_deref() {
                Some("stream.opened") => {
                    if tell.send(News::Opened).is_err() {
                        return "the task stopped".to_owned();
                    }
                }
                Some("stream.closed") => return "stream.closed".to_owned(),
                _ => {
                    if let (Some(id), Ok(mut received)) = (frame.id, received.lock()) {
                        received[index].push(id);
                    }
                }
            },
            Ok(None) => return "the response ended".to_owned(),
            Err(error) => return error,
        }
    }
}

/// The events the streams received since `before`, counted once for each user, and whether each
/// user's streams received the same ones, in the same order, none twice.
pub(super) fn delivered(received: &[Vec<String>], before: &[usize]) -> (u64, bool) {
    let mut delivered = 0;
    let mut consistent = true;
    for (user, streams) in received.chunks(STREAMS_PER_USER).enumerate() {
        let new: Vec<&[String]> = streams
            .iter()
            .enumerate()
            .map(|(n, events)| &events[before[user * STREAMS_PER_USER + n].min(events.len())..])
            .collect();
        let first = new.first().copied().unwrap_or_default();
        let unique: HashSet<&String> = first.iter().collect();
        consistent &= unique.len() == first.len() && new.iter().all(|events| *events == first);
        delivered += first.len() as u64;
    }
    (delivered, consistent)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|id| (*id).to_owned()).collect()
    }

    #[test]
    fn ops_16_a_user_s_streams_must_receive_the_same_events_once_each_in_order() {
        let same = vec![ids(&["a", "b"]); STREAMS_PER_USER];
        let mut missing = same.clone();
        missing[3] = ids(&["a"]);
        let mut twice = vec![ids(&["a", "a"]); STREAMS_PER_USER];
        twice[0] = ids(&["a", "a"]);
        let mut reordered = same.clone();
        reordered[9] = ids(&["b", "a"]);
        let before = vec![0; STREAMS_PER_USER];

        assert_eq!(delivered(&same, &before), (2, true));
        assert!(!delivered(&missing, &before).1);
        assert!(!delivered(&twice, &before).1);
        assert!(!delivered(&reordered, &before).1);
        assert_eq!(delivered(&same, &[1; STREAMS_PER_USER]), (1, true));
    }
}
