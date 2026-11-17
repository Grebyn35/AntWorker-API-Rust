# Spec: `realtime`

## Purpose

`realtime` gives each signed-in client one stream of server-sent events (SSE): the caller's own events, and the events of every workspace they may receive. Delivery, filtering, replay and stream closing are the work of the hub in `platform::events` (SEC-06, SEC-19). This module owns the endpoint, the shape of what it sends, and the event catalog, which documents every frame in `openapi.json`.

## Owns

None. `platform` owns the `events` table.

## Interface

None.

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `GET /v1/users/me/events` | user | REALTIME-001 to REALTIME-008 |

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| REALTIME-001 | An authenticated caller, by the Bearer header or a browser's access cookie, opens a `text/event-stream` response that delivers their user events and the workspace events they may receive, in the order their units of work committed. Its first frame is `stream.opened`, and every event that commits after the response starts reaches the stream, until the client disconnects or the stream closes. A stream that starts with what commits next gives its `stream.opened` an `id` that a reconnect resumes from: the newest event its caller may receive from before it started, or the nil UUID when there is none. | SEC-19, D61, PLATFORM-029, PLATFORM-028 | `realtime_001_` |
| REALTIME-002 | Each event is one frame: `id` is the event's ID, `event` is its name, and `data` is `{"data": <payload>, "trace_id": <string or null>}`, with the trace ID of the request or job that published it, or null for one published outside a trace. The stream's own frames, `stream.opened`, `stream.beat` and `stream.closed`, carry the trace ID of the request that opened it. | API-19, D10 | `realtime_002_` |
| REALTIME-003 | Each stream sends the frame `stream.beat`, its heartbeat, every `STREAM_HEARTBEAT_SECONDS` (5 to 300, default 15), whether or not events come meanwhile, and its `stream.opened` gives that interval as `heartbeat_seconds`. The frame's payload is `{}`, and it has no `id`, so the client's `Last-Event-ID` never moves on it. | OPS-02, D172 | `realtime_003_` |
| REALTIME-004 | A reconnect with `Last-Event-ID` first replays, in order, every event committed after that one that the caller may receive, then continues live, so a client that reconnects with the last `id` it received misses no event and receives none twice, on whichever replica it reconnects to. When more than 1,000 events wait, or the ID names no event the API holds, such as one past the 24 hours that events are kept, or isn't text, the stream sends none of them: its `stream.opened` says `"truncated": true`, so the client reloads its state, and it continues live. | SEC-19, PLATFORM-029, PLATFORM-028 | `realtime_004_` |
| REALTIME-005 | When the hub closes a stream, the stream sends `stream.closed`, whose payload is `{"reason": <reason>}`, and ends the response: `session_ended` when the caller's session ended, `access_changed` when their memberships changed, `lagging` when the client read too slowly to keep up, `shutdown` when the instance shuts down, and `interrupted` when the replica lost the listener, its own connection to the database, for a while. A client reconnects with `Last-Event-ID`, which a session that ended refuses with 401. | SEC-06, OPS-06, PLATFORM-031 | `realtime_005_` |
| REALTIME-006 | A user holds at most 10 open streams on each replica. Opening another answers 409 `realtime.too_many_streams`, with no `Retry-After`, and opens nothing. | D11 | `realtime_006_` |
| REALTIME-007 | Once the replica stops accepting connections, opening a stream answers 503 `realtime.unavailable`, with `Retry-After: 1`, and opens nothing. | OPS-06, API-07 | `realtime_007_` |
| REALTIME-008 | `openapi.json` documents every frame the stream sends, in its event catalog: each event that a module publishes, with its scope, the permission a workspace event requires, and the schema of its payload, and the stream's own frames, `stream.opened`, `stream.beat` and `stream.closed`. Every frame a test receives matches the catalog's entry that its `event` names. | API-17, API-19, D8 | `realtime_008_` |

## Design

The handler authenticates the caller with `Principal`, through the Bearer header from a client that sets its own headers, or the session's cookie from a browser, whose `EventSource` can't send an `Authorization` header (D61). It asks the hub to subscribe the caller with the request's `Last-Event-ID`, and turns what the subscription yields into frames: `stream.opened`, each event, and the `stream.closed` that ends it, with `stream.beat` between them at every heartbeat. The response starts once the hub has subscribed the stream, so a client that reads its state over REST once `stream.opened` arrives misses no change: what committed before then is in the state, and what commits after reaches the stream.

`stream.opened`'s payload is `{"truncated": <bool>, "heartbeat_seconds": <seconds>}`. A stream that starts with what commits next, because the request named no `Last-Event-ID` or its replay was truncated, gives its `stream.opened` an `id`: the ID of the newest event its caller may receive from before the stream started, which a reconnect resumes from, since no event the caller may receive lies between the two. With no such event, the `id` is the nil UUID, which names no event, so a reconnect is truncated and the client reloads. A stream that closes before it sends any event, such as when the caller's own action changed their memberships, is resumed by its client all the same. A resumed stream's `stream.opened` has no `id`, so the client's own stays. `stream.closed` has no `id`.

`stream.beat` comes every `STREAM_HEARTBEAT_SECONDS` from the stream's start, whatever else the stream sends. It keeps the proxies in front of the API from closing a connection that carries nothing else, so the setting stays below the idle timeout of each of them. And it tells the client that the connection is alive: a client that receives no frame for longer than the `heartbeat_seconds` of its `stream.opened`, with a margin for the network, such as twice the interval, takes the connection for dead, closes it, and opens the stream again. The heartbeat is a frame, rather than the comment that the HTML standard suggests against proxies, because that standard has a parser of server-sent events ignore a comment, as a browser's `EventSource` does, so the code of a client that reads the stream through one never sees it, and couldn't tell a dead connection from a quiet one (D172).

A frame's `trace_id` tells a client which events its own requests published. An event carries the trace ID of the request that published it (REALTIME-002), which that request's response carries in `server-timing` (API-16), and which is the trace's of the `traceparent` that the request sent, when it sent one (PLATFORM-041), so a client that sends its own knows the ID before any frame arrives. A client that keeps the trace IDs of its recent requests can so skip reading again what a response just gave it, where the user's other devices read it.

A browser's `EventSource` reconnects by itself when the response ends, but gives up on a response that isn't 200, such as a 401, a 409 or a 503: the client then signs in again, closes a stream it doesn't need, or opens one again after the `Retry-After`. A cap on streams is a count, not a rate, so its refusal is a conflict with the streams the user holds, with no `Retry-After`, since no window ends it; it carries the IP and user limits' headers, as every response they counted does (PLATFORM-056).

### The event catalog

A compatible client learns from `openapi.json` every frame the stream can send, as it learns every response of the other routes (D8). The catalog is the component `Frame`, a `oneOf` with one schema per frame type, each an object that describes the frame once a client's parser has read it: `event`, the type, as an `enum` of that one value; `id`, the event's ID, where the frame has one; and `data`, a string whose `contentMediaType` is `application/json` and whose `contentSchema` is the envelope of REALTIME-002, `{"data": <payload>, "trace_id": <string or null>}`, whose payload is a component of its own, named after the payload's type, so a generated client has a type for it. An event's schema says in its description whether it is a user event, sent to its user's streams, or a workspace event, and then which permission its members need (SEC-19). `stream.opened`, `stream.beat` and `stream.closed` are entries too, with their payloads: `{"truncated": <bool>, "heartbeat_seconds": <seconds>}`, `{}`, and `{"reason": <reason>}`, whose reasons are an `enum`.

This is the shape OpenAPI 3.2 gives each item of a `text/event-stream` in its `itemSchema`. The document stays at OpenAPI 3.1, since the generators most clients are built with, such as openapi-generator and openapi-typescript, don't read a 3.2 document, and 3.1 has no field for the schema of a stream's items. So the stream's media type names `Frame` in the extension `x-itemSchema`, which moving to 3.2 renames, and every generator reads the frames' schemas among the components.

The catalog comes from the code. A module declares each event in its `events.rs` with the type of its payload (platform spec, "Events and the real-time hub"), whose schema is the payload's, and registers it with its routes, so a part's events leave the document with the part (ARCH-07). `check-openapi` reads the catalog as the components that `Frame` refers to, and the code's frame types as each string that follows `UserEvent::new(` or `WorkspaceEvent::new(` in a module's `events.rs`, or `StreamFrame::new(` in a file of a module. It fails while the catalog lacks one of those names, as it does when a module declares an event and doesn't register it; while two of them are the same, since the catalog keeps one frame type of each name, so a user event and a workspace event of one name would document one of the two; and while the document marks a component as a frame type's that `Frame` doesn't refer to. A type of the code that derives `ToSchema`, or implements it by hand, under the name `Frame` or that of a frame type's component fails it too (API-17). The test support checks every frame a test reads against the catalog's entry for its `event`, as it checks every response against the document (API-17), so an event whose payload drifts from its schema fails the test that receives it.

## Open design questions

None.
