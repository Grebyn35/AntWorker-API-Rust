//! A client of the stream of server-sent events, in process: it opens `GET /v1/users/me/events`
//! and reads its frames one by one, as an `EventSource` does (REALTIME-001).

use axum::{body::Body, extract::ConnectInfo};
use http::{Method, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;

use super::{Api, api::Response, contract::Contract};

/// The stream's path.
pub const PATH: &str = "/v1/users/me/events";

/// One frame of a stream: an event, or a comment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    /// Its `id`, if it has one.
    pub id: Option<String>,
    /// Its `event`, if it has one.
    pub event: Option<String>,
    /// Its `data`, as JSON, if it has some.
    pub data: Option<Value>,
    /// Whether it is a comment.
    pub comment: bool,
}

impl Frame {
    /// The payload of its envelope (REALTIME-002).
    pub fn payload(&self) -> &Value {
        &self.data.as_ref().expect("a frame with data")["data"]
    }
}

/// An open stream.
pub struct Stream {
    contract: std::sync::Arc<Contract>,
    headers: http::HeaderMap,
    body: Body,
    buffer: String,
    /// The `Last-Event-ID` an `EventSource` would send on reconnecting.
    pub last_event_id: Option<String>,
}

/// What opening a stream came to: the stream, or the response when it isn't 200.
pub enum Opened {
    /// The stream.
    Stream(Box<Stream>),
    /// The response that refused it.
    Refused(Box<Response>),
}

impl Opened {
    /// The stream, if it opened.
    pub fn stream(self) -> Option<Stream> {
        match self {
            Self::Stream(stream) => Some(*stream),
            Self::Refused(_) => None,
        }
    }

    /// The response that refused the stream, if it didn't open.
    pub fn refused(self) -> Option<Response> {
        match self {
            Self::Stream(_) => None,
            Self::Refused(response) => Some(*response),
        }
    }
}

/// Opens a stream with `headers`, and returns it, or the response when it isn't 200. The
/// response's status and headers are checked against the document.
pub async fn open(api: &Api, headers: &[(&str, &str)]) -> Opened {
    let mut request = Request::builder()
        .method(Method::GET)
        .uri(PATH)
        .body(Body::empty())
        .unwrap();
    for (name, value) in headers {
        request.headers_mut().append(
            http::HeaderName::try_from(*name).unwrap(),
            http::HeaderValue::try_from(*value).unwrap(),
        );
    }
    request.extensions_mut().insert(ConnectInfo(
        super::api::PEER.parse::<std::net::SocketAddr>().unwrap(),
    ));
    let response = api.router().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    if status != StatusCode::OK {
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        api.check(&Method::GET, PATH, status, &headers, &body);
        return Opened::Refused(Box::new(Response {
            status,
            headers,
            body,
        }));
    }
    api.check(&Method::GET, PATH, status, &headers, &[]);
    Opened::Stream(Box::new(Stream {
        contract: api.contract(),
        headers,
        body: response.into_body(),
        buffer: String::new(),
        last_event_id: None,
    }))
}

/// Opens a stream as the caller whose access token is `token`, with the header `name` set to
/// `value`, bytes that need not be text, and returns what that came to.
pub async fn open_with_bytes(api: &Api, token: &str, name: &str, value: &[u8]) -> Opened {
    let mut request = Request::builder()
        .method(Method::GET)
        .uri(PATH)
        .header("authorization", format!("Bearer {token}"))
        .header(name, http::HeaderValue::from_bytes(value).unwrap())
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        super::api::PEER.parse::<std::net::SocketAddr>().unwrap(),
    ));
    let response = api.router().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    api.check(&Method::GET, PATH, status, &headers, &[]);
    Opened::Stream(Box::new(Stream {
        contract: api.contract(),
        headers,
        body: response.into_body(),
        buffer: String::new(),
        last_event_id: None,
    }))
}

/// Opens the stream of `token`'s caller on `router`, another application of `api`'s database,
/// as on another replica, which must answer 200, with `Last-Event-ID` set to `last_event_id`
/// when given.
pub async fn connect_on(
    api: &Api,
    router: axum::Router,
    token: &str,
    last_event_id: Option<&str>,
) -> Stream {
    let mut builder = Request::builder()
        .method(Method::GET)
        .uri(PATH)
        .header("authorization", format!("Bearer {token}"));
    if let Some(id) = last_event_id {
        builder = builder.header("last-event-id", id);
    }
    let mut request = builder.body(Body::empty()).unwrap();
    request.extensions_mut().insert(ConnectInfo(
        super::api::PEER.parse::<std::net::SocketAddr>().unwrap(),
    ));
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    assert_eq!(status, StatusCode::OK);
    api.check(&Method::GET, PATH, status, &headers, &[]);
    Stream {
        contract: api.contract(),
        headers,
        body: response.into_body(),
        buffer: String::new(),
        last_event_id: None,
    }
}

/// Opens a stream as the caller whose access token is `token`, and returns what that came to.
pub async fn open_as(api: &Api, token: &str) -> Opened {
    open(api, &[("authorization", &format!("Bearer {token}"))]).await
}

/// Opens the stream of `token`'s caller, which must answer 200, with `Last-Event-ID` set to
/// `last_event_id` when given.
pub async fn connect(api: &Api, token: &str, last_event_id: Option<&str>) -> Stream {
    let bearer = format!("Bearer {token}");
    let mut headers = vec![("authorization", bearer.as_str())];
    if let Some(id) = last_event_id {
        headers.push(("last-event-id", id));
    }
    open(api, &headers)
        .await
        .stream()
        .expect("the stream opened")
}

impl Stream {
    /// The trace ID of the request that opened the stream, from its `server-timing` (API-16).
    pub fn trace_id(&self) -> String {
        let timing = self.headers["server-timing"].to_str().unwrap();
        timing.split('-').nth(1).unwrap().to_owned()
    }

    /// The next frame, comments included, or `None` once the response has ended.
    pub async fn frame(&mut self) -> Option<Frame> {
        loop {
            if let Some(end) = self.buffer.find("\n\n") {
                let block: String = self.buffer.drain(..end + 2).collect();
                return Some(self.parse(&block));
            }
            let frame = self.body.frame().await?.unwrap();
            if let Ok(data) = frame.into_data() {
                self.buffer.push_str(std::str::from_utf8(&data).unwrap());
            }
        }
    }

    /// The next frame that isn't a comment, which must come. A response that ends first fails
    /// the test with what the stream held of a frame it never finished, and the `id` of the
    /// last event it sent.
    pub async fn event(&mut self) -> Frame {
        loop {
            let frame = self.frame().await;
            assert!(
                frame.is_some(),
                "the stream ended without stream.closed, holding {:?} after the event {:?}",
                self.buffer,
                self.last_event_id
            );
            let frame = frame.unwrap();
            if !frame.comment {
                return frame;
            }
        }
    }

    /// The next frame, which must be `stream.opened`.
    pub async fn opened(&mut self) -> Frame {
        let frame = self.event().await;
        assert_eq!(frame.event.as_deref(), Some("stream.opened"), "{frame:?}");
        frame
    }

    /// The events up to `stream.closed`, which must come, and the reason it gives, after which
    /// the response must end.
    pub async fn closed(&mut self) -> (Vec<Frame>, String) {
        let mut before = Vec::new();
        loop {
            let frame = self.event().await;
            if frame.event.as_deref() == Some("stream.closed") {
                let reason = frame.payload()["reason"].as_str().unwrap().to_owned();
                assert_eq!(self.frame().await, None, "the stream went on after closing");
                return (before, reason);
            }
            before.push(frame);
        }
    }

    fn parse(&mut self, block: &str) -> Frame {
        let mut frame = Frame::default();
        let mut data = Vec::new();
        for line in block.lines() {
            if line.starts_with(':') {
                frame.comment = true;
                continue;
            }
            let (field, value) = line.split_once(':').unwrap_or((line, ""));
            let value = value.strip_prefix(' ').unwrap_or(value);
            match field {
                "id" => frame.id = Some(value.to_owned()),
                "event" => frame.event = Some(value.to_owned()),
                "data" => data.push(value.to_owned()),
                _ => {}
            }
        }
        let data = (!data.is_empty()).then(|| data.join("\n"));
        if !frame.comment {
            self.contract
                .check_frame(frame.event.as_deref(), frame.id.as_deref(), data.as_deref());
        }
        if let Some(data) = data {
            frame.data = Some(serde_json::from_str(&data).unwrap());
        }
        if let Some(id) = &frame.id {
            self.last_event_id = Some(id.clone());
        }
        frame
    }
}
