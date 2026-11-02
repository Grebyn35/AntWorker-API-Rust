//! The stream of server-sent events (REALTIME-001 to REALTIME-006): the hub's deliveries, turned
//! into frames, with a comment at every heartbeat.

use std::{convert::Infallible, time::Duration};

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, header},
    response::{
        IntoResponse, Response,
        sse::{Event as Frame, Sse},
    },
};
use futures_util::Stream;
use platform::{
    authn::Principal,
    clock::{SharedClock, Sleep},
    errors::Problem,
    events::{Closed, Delivery, FrameId, Hub, StreamFrame, Subscription},
    http::Routes,
    telemetry,
};
use serde::Serialize;
use serde_json::{Value, json};
use utoipa::ToSchema;
use utoipa_axum::routes;

use super::error::RealtimeError;

/// The header a reconnecting client names its last event in (REALTIME-004).
const LAST_EVENT_ID: &str = "last-event-id";

/// What the route takes: the replica's hub, the clock its heartbeats wait on, and their interval.
#[derive(Clone)]
pub(crate) struct Realtime {
    /// The replica's hub.
    pub(crate) hub: Hub,
    /// The clock of the heartbeats (CODE-06).
    pub(crate) clock: SharedClock,
    /// How often a stream sends a comment (REALTIME-003).
    pub(crate) heartbeat: Duration,
}

/// The stream's first frame (REALTIME-001).
const STREAM_OPENED: StreamFrame<StreamOpened> = StreamFrame::new(
    "stream.opened",
    FrameId::Sometimes,
    "`stream.opened`, the stream's first frame. Its `id`, when it has one, is where a reconnect \
     resumes a stream that started with what commits next (REALTIME-001, REALTIME-004).",
);

/// The stream's last frame (REALTIME-005).
const STREAM_CLOSED: StreamFrame<StreamClosed> = StreamFrame::new(
    "stream.closed",
    FrameId::Never,
    "`stream.closed`, the stream's last frame, which says why it closed; the client reconnects \
     (REALTIME-005).",
);

/// The payload of `stream.opened`.
#[derive(Serialize, ToSchema)]
struct StreamOpened {
    /// Whether the replay was truncated, so the client reloads its state (REALTIME-004).
    truncated: bool,
}

/// The payload of `stream.closed`.
#[derive(Serialize, ToSchema)]
struct StreamClosed {
    /// Why the stream closed.
    reason: Closed,
}

/// The module's routes, which take the hub, and the stream's own frames.
pub(crate) fn routes() -> Routes<Realtime> {
    Routes::new()
        .events(&[&STREAM_OPENED, &STREAM_CLOSED])
        .user(
            routes!(events),
            &[&RealtimeError::TooManyStreams, &RealtimeError::Unavailable],
        )
}

/// Opens the caller's stream of events: their own, and those of their workspaces that their role
/// may receive, in the order they committed. A client that reconnects names the last event it
/// received in `Last-Event-ID`, and receives every event after it first.
#[utoipa::path(
    get,
    path = "/v1/users/me/events",
    tag = "realtime",
    params(
        ("Last-Event-ID" = Option<String>, Header,
            description = "The ID of the last event the client received, to resume after it."),
    ),
    responses(
        (status = 200, content_type = "text/event-stream", body = String,
            description = "Server-sent events, each a frame that `Frame`, the event catalog, \
            describes, which `x-itemSchema` names. The first is `stream.opened`, and \
            `stream.closed` ends the stream. A comment comes at every heartbeat."),
    )
)]
async fn events(
    State(realtime): State<Realtime>,
    principal: Principal,
    headers: HeaderMap,
) -> Response {
    // A header that isn't text names no event the API holds, so its replay is truncated
    // (REALTIME-004).
    let last_event_id = headers
        .get(LAST_EVENT_ID)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
    match realtime
        .hub
        .subscribe(&principal, last_event_id.as_deref())
        .await
    {
        Ok(subscription) => {
            let frames = Frames {
                beat: realtime.clock.sleep(realtime.heartbeat),
                subscription,
                clock: realtime.clock,
                heartbeat: realtime.heartbeat,
                trace_id: telemetry::trace_id(),
                ended: false,
            };
            Sse::new(frames.into_stream()).into_response()
        }
        Err(error) => {
            let error = RealtimeError::from(error);
            let unavailable = matches!(error, RealtimeError::Unavailable);
            let mut response = Problem(error).into_response();
            if unavailable {
                // The replica has stopped accepting connections, so the next request reaches
                // another (OPS-07).
                response
                    .headers_mut()
                    .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
            }
            response
        }
    }
}

/// A stream's frames: what the subscription delivers, and a comment at every heartbeat.
struct Frames {
    subscription: Subscription,
    clock: SharedClock,
    heartbeat: Duration,
    beat: Sleep,
    /// The trace ID of the request that opened the stream, which the frames of the stream's own
    /// carry (REALTIME-002).
    trace_id: String,
    ended: bool,
}

impl Frames {
    fn into_stream(self) -> impl Stream<Item = Result<Frame, Infallible>> + Send {
        futures_util::stream::unfold(self, |mut frames| async move {
            let frame = frames.next().await?;
            Some((Ok(frame), frames))
        })
    }

    /// The next frame, or `None` once the stream has sent why it closed.
    async fn next(&mut self) -> Option<Frame> {
        if self.ended {
            return None;
        }
        tokio::select! {
            delivery = self.subscription.next() => delivery.map(|delivery| self.frame(delivery)),
            () = &mut self.beat => {
                self.beat = self.clock.sleep(self.heartbeat);
                // A comment carries no `id`, so the client's `Last-Event-ID` stays (REALTIME-003).
                Some(Frame::default().comment(""))
            }
        }
    }

    fn frame(&mut self, delivery: Delivery) -> Frame {
        match delivery {
            Delivery::Event(event) => Frame::default()
                .id(event.id().to_string())
                .event(event.name())
                .data(envelope(event.payload().clone(), event.trace_id())),
            Delivery::Opened { truncated, resume } => {
                let frame = Frame::default().event(STREAM_OPENED.name()).data(envelope(
                    json!(StreamOpened { truncated }),
                    Some(&self.trace_id),
                ));
                match resume {
                    Some(resume) => frame.id(resume.to_string()),
                    None => frame,
                }
            }
            Delivery::Closed(closed) => {
                self.ended = true;
                Frame::default().event(STREAM_CLOSED.name()).data(envelope(
                    json!(StreamClosed { reason: closed }),
                    Some(&self.trace_id),
                ))
            }
        }
    }
}

/// An event's `data`: its payload, and the trace ID of what published it (REALTIME-002).
fn envelope(payload: Value, trace_id: Option<&str>) -> String {
    json!({ "data": payload, "trace_id": trace_id }).to_string()
}
