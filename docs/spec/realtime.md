# Spec: `realtime`

## Purpose

`realtime` gives each signed-in client one stream of server-sent events (SSE): the caller's own events, and the events of every workspace they may receive. Delivery, filtering, replay and stream closing are the work of `platform::events` (SEC-06, SEC-17). This module owns the endpoint and the shape of what it sends.

## Owns

Nothing. `platform` owns the `events` table.

## Interface

None.

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `GET /v1/users/me/events` | user | REALTIME-001 to REALTIME-005 |

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| REALTIME-001 | An authenticated caller opens a `text/event-stream` response that delivers their user events and the workspace events they may receive, until they disconnect or lose access. | SEC-17, PLATFORM-016 | `realtime_001_` |
| REALTIME-002 | Each event is one frame: `id` is the event's ID, `event` is its type, and `data` is `{"data": <payload>, "trace_id": <string>}`. | API-19, D10 | `realtime_002_` |
| REALTIME-003 | Each stream sends an SSE comment every 15 seconds, configurable, to keep the connection open. A comment has no `id`, so the client's `Last-Event-ID` never moves on it. | OPS-02 | `realtime_003_` |
| REALTIME-004 | A reconnect with `Last-Event-ID` first replays, in order, the events after that one that the caller may receive, then continues live. An unknown ID means live only. A replay is capped at 1,000 events, configurable. When the cap cuts it short, the stream sends `replay.truncated`, so the client reloads its state. | SEC-17, PLATFORM-016 | `realtime_004_` |
| REALTIME-005 | When the caller's session ends or their access changes, the stream sends `stream.closed` with the reason and then closes, or keeps running with the new filter. | SEC-06, PLATFORM-017 | `realtime_005_` |

## Design

The handler authenticates with the caller's `Principal`, asks `platform::events` for a stream subscribed with the caller's `Memberships`, and returns it as the SSE response. Browsers can't send an `Authorization` header on an `EventSource`, so web clients need the cookie source from D18. The desktop client sends the header.

## Open design questions

None.
