# Spec: `realtime`

## Purpose

`realtime` gives each signed-in client one stream of server-sent events (SSE): the caller's own events, and the events of every workspace they may receive. Delivery, filtering, replay and stream closing are the work of the hub in `platform::events` (SEC-06, SEC-17). This module owns the endpoint and the shape of what it sends.

## Owns

Nothing. `platform` owns the `events` table.

## Interface

None.

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `GET /v1/users/me/events` | user | REALTIME-001 to REALTIME-006 |

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| REALTIME-001 | An authenticated caller, by the Bearer header or a browser's access cookie, opens a `text/event-stream` response that delivers their user events and the workspace events they may receive, in the order their units of work committed. Every event that commits after the response starts reaches the stream, until the client disconnects or the stream closes. | SEC-17, D61, PLATFORM-016, PLATFORM-071 | `realtime_001_` |
| REALTIME-002 | Each event is one frame: `id` is the event's ID, `event` is its type, and `data` is `{"data": <payload>, "trace_id": <string>}`, the trace ID of the request or job that published it. | API-19, D10 | `realtime_002_` |
| REALTIME-003 | Each stream sends an SSE comment every `STREAM_HEARTBEAT_SECONDS` (5 to 300, default 15), to keep the connection open through proxies. A comment has no `id`, so the client's `Last-Event-ID` never moves on it. | OPS-02 | `realtime_003_` |
| REALTIME-004 | A reconnect with `Last-Event-ID` first replays, in order, every event committed after that one that the caller may receive, then continues live, so a client that reconnects with the ID of the last event it received misses none and receives none twice, on whichever replica it reconnects to. When more than 1,000 events wait, or the ID names no event the API holds, such as one past the 24 hours that events are kept, the stream sends `replay.truncated` instead of any of them, and continues live, so the client reloads its state. | SEC-17, PLATFORM-016, PLATFORM-071 | `realtime_004_` |
| REALTIME-005 | When the hub closes a stream, the stream sends `stream.closed`, whose `data` is `{"reason": <reason>}`, and ends the response: `session_ended` when the caller's session ended, `access_changed` when their memberships changed, `lagging` when the client read too slowly to keep up, and `shutdown` when the instance shuts down. A client reconnects with `Last-Event-ID`, which a session that ended refuses with 401. | SEC-06, OPS-06, PLATFORM-017 | `realtime_005_` |
| REALTIME-006 | A user holds at most 10 open streams on each replica. Opening another answers 429 `realtime.too_many_streams`, and opens nothing. | D11 | `realtime_006_` |

## Design

The handler authenticates the caller with `Principal`, through the Bearer header from a client that sets its own headers, or the session's cookie from a browser, whose `EventSource` can't send an `Authorization` header (D61). It asks the hub to subscribe the caller with the request's `Last-Event-ID`, and turns what the subscription yields into frames: each event, `replay.truncated`, and the `stream.closed` that ends it. The response starts once the hub has subscribed the stream, so a client that reads its state over REST once the response starts misses no change: what committed before then is in the state, and what commits after reaches the stream.

`replay.truncated` and `stream.closed` carry no `id`, so they don't move the client's `Last-Event-ID`: after `replay.truncated`, a reconnect replays from the same event, and gets `replay.truncated` again until the client has received a live event. Their `data` holds `{"data": ..., "trace_id": ...}` like any event's, with the request's trace ID, so every frame parses the same way.

A browser's `EventSource` reconnects by itself when the response ends, but gives up on a response that isn't 200, such as a 401, a 429 or a 503: the client then signs in again, closes a stream it doesn't need, or opens one again after the `Retry-After`.

## Open design questions

| Question | Settled by |
|---|---|
| How `openapi.json` documents the event catalog, so a compatible client knows every event type, its scope, the permission a workspace event requires, and its payload's schema, which typed payloads would give | the spec of step 2.4's second part |
