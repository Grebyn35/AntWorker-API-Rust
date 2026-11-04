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
| REALTIME-002 | Each event is one frame: `id` is the event's ID, `event` is its name, and `data` is `{"data": <payload>, "trace_id": <string or null>}`, with the trace ID of the request or job that published it, or null for one published outside a trace. The stream's own frames, `stream.opened` and `stream.closed`, carry the trace ID of the request that opened it. | API-19, D10 | `realtime_002_` |
| REALTIME-003 | Each stream sends an SSE comment every `STREAM_HEARTBEAT_SECONDS` (5 to 300, default 15), to keep the connection open through proxies. A comment has no `id`, so the client's `Last-Event-ID` never moves on it. | OPS-02 | `realtime_003_` |
| REALTIME-004 | A reconnect with `Last-Event-ID` first replays, in order, every event committed after that one that the caller may receive, then continues live, so a client that reconnects with the last `id` it received misses no event and receives none twice, on whichever replica it reconnects to. When more than 1,000 events wait, or the ID names no event the API holds, such as one past the 24 hours that events are kept, or isn't text, the stream sends none of them: its `stream.opened` says `{"truncated": true}`, so the client reloads its state, and it continues live. | SEC-19, PLATFORM-029, PLATFORM-028 | `realtime_004_` |
| REALTIME-005 | When the hub closes a stream, the stream sends `stream.closed`, whose payload is `{"reason": <reason>}`, and ends the response: `session_ended` when the caller's session ended, `access_changed` when their memberships changed, `lagging` when the client read too slowly to keep up, `shutdown` when the instance shuts down, and `interrupted` when the replica lost the listener, its own connection to the database, for a while. A client reconnects with `Last-Event-ID`, which a session that ended refuses with 401. | SEC-06, OPS-06, PLATFORM-031 | `realtime_005_` |
| REALTIME-006 | A user holds at most 10 open streams on each replica. Opening another answers 409 `realtime.too_many_streams`, with no `Retry-After`, and opens nothing. | D11 | `realtime_006_` |
| REALTIME-007 | Once the replica stops accepting connections, opening a stream answers 503 `realtime.unavailable`, with `Retry-After: 1`, and opens nothing. | OPS-06, API-07 | `realtime_007_` |
| REALTIME-008 | `openapi.json` documents every frame the stream sends, in its event catalog: each event that a module publishes, with its scope, the permission a workspace event requires, and the schema of its payload, and `stream.opened` and `stream.closed`. Every frame a test receives matches the catalog's entry that its `event` names. | API-17, API-19, D8 | `realtime_008_` |

## Design

The handler authenticates the caller with `Principal`, through the Bearer header from a client that sets its own headers, or the session's cookie from a browser, whose `EventSource` can't send an `Authorization` header (D61). It asks the hub to subscribe the caller with the request's `Last-Event-ID`, and turns what the subscription yields into frames: `stream.opened`, each event, and the `stream.closed` that ends it. The response starts once the hub has subscribed the stream, so a client that reads its state over REST once `stream.opened` arrives misses no change: what committed before then is in the state, and what commits after reaches the stream.

`stream.opened`'s payload is `{"truncated": <bool>}`. A stream that starts with what commits next, because the request named no `Last-Event-ID` or its replay was truncated, gives its `stream.opened` an `id`: the ID of the newest event its caller may receive from before the stream started, which a reconnect resumes from, since no event the caller may receive lies between the two. With no such event, the `id` is the nil UUID, which names no event, so a reconnect is truncated and the client reloads. A stream that closes before it sends any event, such as when the caller's own action changed their memberships, is resumed by its client all the same. A resumed stream's `stream.opened` has no `id`, so the client's own stays. `stream.closed` has no `id`.

A browser's `EventSource` reconnects by itself when the response ends, but gives up on a response that isn't 200, such as a 401, a 409 or a 503: the client then signs in again, closes a stream it doesn't need, or opens one again after the `Retry-After`. A cap on streams is a count, not a rate, so its refusal is a conflict with the streams the user holds, with no `Retry-After`, since no window ends it; it carries the IP and user limits' headers, as every response they counted does (PLATFORM-056).

### The event catalog

A compatible client learns from `openapi.json` every frame the stream can send, as it learns every response of the other routes (D8). The catalog is the component `Frame`, a `oneOf` with one schema per frame type, each an object that describes the frame once a client's parser has read it: `event`, the type, as an `enum` of that one value; `id`, the event's ID, where the frame has one; and `data`, a string whose `contentMediaType` is `application/json` and whose `contentSchema` is the envelope of REALTIME-002, `{"data": <payload>, "trace_id": <string or null>}`, whose payload is a component of its own, named after the payload's type, so a generated client has a type for it. An event's schema says in its description whether it is a user event, sent to its user's streams, or a workspace event, and then which permission its members need (SEC-19). `stream.opened` and `stream.closed` are entries too, with their payloads, `{"truncated": <bool>}` and `{"reason": <reason>}`, whose reasons are an `enum`.

This is the shape OpenAPI 3.2 gives each item of a `text/event-stream` in its `itemSchema`. The document stays at OpenAPI 3.1, since the generators most clients are built with, such as openapi-generator and openapi-typescript, don't read a 3.2 document, and 3.1 has no field for the schema of a stream's items. So the stream's media type names `Frame` in the extension `x-itemSchema`, which moving to 3.2 renames, and every generator reads the frames' schemas among the components.

The catalog comes from the code. A module declares each event in its `events.rs` with the type of its payload (platform spec, "Events and the real-time hub"), whose schema is the payload's, and registers it with its routes, so a part's events leave the document with the part (ARCH-07). `check-openapi` fails while an event that a module's `events.rs` declares is missing from the catalog. The test support checks every frame a test reads against the catalog's entry for its `event`, as it checks every response against the document (API-17), so an event whose payload drifts from its schema fails the test that receives it.

## Open design questions

None.
