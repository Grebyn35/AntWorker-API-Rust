//! A client of the API over plain HTTP/1.1, for the hub's load test: JSON requests, each on a
//! connection of its own, and streams of server-sent events, read frame by frame as a client's
//! parser reads them. The task runs synchronously, with a thread for each stream, and k6 has no
//! server-sent events of its own.

use std::{
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    time::Duration,
};

use serde_json::Value;

/// How long a request waits for its answer, and a stream for its next frame, which its heartbeat
/// sends every 15 seconds (REALTIME-003).
const ANSWER_TIMEOUT: Duration = Duration::from_secs(60);

/// Sends `method` to `path` of the API at `address`, as `token`'s caller when given, with `body`
/// as JSON when given, and returns the answer's status and JSON.
pub(super) fn request(
    address: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<&Value>,
) -> Result<(u16, Value), String> {
    let mut connection = TcpStream::connect(address)
        .map_err(|error| format!("can't connect to {address}: {error}"))?;
    connection
        .set_read_timeout(Some(ANSWER_TIMEOUT))
        .map_err(|error| error.to_string())?;
    let body = body.map(Value::to_string).unwrap_or_default();
    let mut head = format!(
        "{method} {path} HTTP/1.1\r\nHost: {address}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    if !body.is_empty() {
        head.push_str("Content-Type: application/json\r\n");
    }
    if let Some(token) = token {
        head.push_str(&format!("Authorization: Bearer {token}\r\n"));
    }
    head.push_str("\r\n");
    connection
        .write_all(format!("{head}{body}").as_bytes())
        .map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(connection);
    let (status, chunked) = read_head(&mut reader)?;
    let mut answer = Vec::new();
    if chunked {
        while let Some(chunk) = read_chunk(&mut reader)? {
            answer.extend(chunk);
        }
    } else {
        reader
            .read_to_end(&mut answer)
            .map_err(|error| error.to_string())?;
    }
    let json = if answer.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&answer)
            .map_err(|error| format!("{method} {path} answered {status}, not JSON: {error}"))?
    };
    Ok((status, json))
}

/// One frame of a stream, as a client's parser reads it.
#[derive(Debug, Default)]
pub(super) struct Frame {
    /// Its `event`.
    pub(super) event: Option<String>,
    /// Its `id`.
    pub(super) id: Option<String>,
}

/// An open stream of server-sent events, read from `R`.
pub(super) struct Stream<R> {
    reader: BufReader<R>,
    buffer: String,
}

impl Stream<TcpStream> {
    /// Opens the stream of `token`'s caller on the API at `address`.
    pub(super) fn open(address: &str, token: &str) -> Result<Self, String> {
        let mut connection = TcpStream::connect(address)
            .map_err(|error| format!("can't connect to {address}: {error}"))?;
        connection
            .set_read_timeout(Some(ANSWER_TIMEOUT))
            .map_err(|error| error.to_string())?;
        connection
            .write_all(
                format!(
                    "GET /v1/users/me/events HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {token}\r\n\r\n"
                )
                .as_bytes(),
            )
            .map_err(|error| error.to_string())?;
        let mut reader = BufReader::new(connection);
        let (status, chunked) = read_head(&mut reader)?;
        if status != 200 || !chunked {
            return Err(format!("opening a stream answered {status}"));
        }
        Ok(Self {
            reader,
            buffer: String::new(),
        })
    }
}

impl<R: Read> Stream<R> {
    /// The next frame that isn't a comment, or `None` once the response has ended.
    pub(super) fn next(&mut self) -> Result<Option<Frame>, String> {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let block: String = self.buffer.drain(..end + 2).collect();
                let mut frame = Frame::default();
                for line in block.lines() {
                    let (field, value) = line.split_once(':').unwrap_or((line, ""));
                    let value = value.strip_prefix(' ').unwrap_or(value).to_owned();
                    match field {
                        "event" => frame.event = Some(value),
                        "id" => frame.id = Some(value),
                        _ => {}
                    }
                }
                if frame.event.is_some() {
                    return Ok(Some(frame));
                }
                continue;
            }
            let Some(chunk) = read_chunk(&mut self.reader)? else {
                return Ok(None);
            };
            self.buffer.push_str(&String::from_utf8_lossy(&chunk));
        }
    }
}

/// Reads an answer's status line and headers, and returns its status and whether its body is
/// chunked.
fn read_head(reader: &mut impl BufRead) -> Result<(u16, bool), String> {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    let status = line
        .split_whitespace()
        .nth(1)
        .and_then(|status| status.parse().ok())
        .ok_or_else(|| format!("not an HTTP answer: {line:?}"))?;
    let mut chunked = false;
    loop {
        line.clear();
        reader
            .read_line(&mut line)
            .map_err(|error| error.to_string())?;
        let header = line.trim_end();
        if header.is_empty() {
            return Ok((status, chunked));
        }
        if let Some((name, value)) = header.split_once(':') {
            chunked |= name.eq_ignore_ascii_case("transfer-encoding")
                && value.trim().eq_ignore_ascii_case("chunked");
        }
    }
}

/// Reads one chunk of a chunked body, or `None` at its end, or at the connection's.
fn read_chunk(reader: &mut impl BufRead) -> Result<Option<Vec<u8>>, String> {
    let mut size = String::new();
    let read = reader
        .read_line(&mut size)
        .map_err(|error| error.to_string())?;
    if read == 0 {
        return Ok(None);
    }
    let size = usize::from_str_radix(size.trim(), 16)
        .map_err(|_| format!("not a chunk's size: {size:?}"))?;
    if size == 0 {
        return Ok(None);
    }
    let mut chunk = vec![0; size + 2];
    reader
        .read_exact(&mut chunk)
        .map_err(|error| error.to_string())?;
    chunk.truncate(size);
    Ok(Some(chunk))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_16_a_chunked_stream_s_frames_are_read_as_a_client_s_parser_reads_them() {
        // Chunks that split frames anywhere, as a server's writes may, with a heartbeat between.
        let chunks = [
            "event: stream.opened\ndata: {}\n\n",
            ": \n\n",
            "id: 1\nevent: invitation.rec",
            "eived\ndata: {}\n",
            "\n",
        ];
        let mut body = String::new();
        for chunk in chunks {
            body.push_str(&format!("{:x}\r\n{chunk}\r\n", chunk.len()));
        }
        body.push_str("0\r\n\r\n");
        let mut stream = Stream {
            reader: BufReader::new(body.as_bytes()),
            buffer: String::new(),
        };
        let mut frames = Vec::new();
        while let Some(frame) = stream.next().unwrap() {
            frames.push((frame.event.unwrap(), frame.id));
        }

        assert_eq!(
            frames,
            [
                ("stream.opened".to_owned(), None),
                ("invitation.received".to_owned(), Some("1".to_owned()))
            ]
        );
    }
}
