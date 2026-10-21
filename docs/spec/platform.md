# Spec: `platform`

## Purpose

`platform` holds the mechanisms every module uses: the unit of work, the proofs that authentication and authorization produce, background jobs, events and the real-time hub, the HTTP stack, errors, configuration and telemetry. It contains no product rules (ARCH-02). [architecture.md §4](../architecture.md#4-crates) lists its areas.

This spec covers what the first slice needs. Object storage, the telemetry proxy and the remaining areas are specified when the modules that use them are built.

## Owns

| Table | Holds |
|---|---|
| `events` | Published events, kept for replay |
| `jobs` | The job queue |
| `job_schedules` | One row per periodic job, holding its lease and next run |
| `idempotency_records` | Reserved and completed idempotency keys, with their stored results |
| `rate_limit_buckets` | Rate-limit counters, keyed by HMAC |

## Interface

- **Unit of work:** `UnitOfWork`, opened by the route (Design: Unit of work), with `commit`, `enqueue` and `publish`.
- **Proof types:** `Principal`, `Authorized<P>`, `Memberships` and `JobContext`.
- **Traits that modules implement** (ARCH-13):
  - `SessionVerifier`, implemented by `auth`.
  - `AccessPolicy`, implemented by `access`, with `authorize` and `memberships`.
  - `SecurityEventSink`, implemented by `audit`.
  - The personal data contract.
- **Traits that modules use:** `Permission`, `ErrorCode`, and the job handler trait.
- **Calls:**
  - `authz::refuse(&Authorized<P>, reason)`: the 403 `request.permission_denied` for a refusal inside an operation, recorded as a security event (PLATFORM-025).
  - `events::subscribe(&Principal, &Memberships, last_event_id)`: a stream of the caller's events (SEC-17).
- **Services:**
  - the job registry and scheduler
  - the event types' declaration
  - the outbound HTTP clients (SEC-09)
  - the clock and the random source (CODE-06)
  - the metric declaration macro (OBS-11)

## Behaviors

IDs 001 to 005, 007 and 008 are retired.

### Database and authorization

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-006 | A unit of work that records a security failure commits that record, and the response is the failure. A failed sign-in answers 401. | ARCH-11, DATA-27, D36 | `platform_006_` |
| PLATFORM-009 | Startup refuses an `app` role that is a superuser or owns any table. | DATA-13 | `platform_009_` |
| PLATFORM-010 | `bootstrap` creates the two roles with their attributes, and running it again changes nothing. | DATA-12 | `platform_010_` |
| PLATFORM-024 | On a workspace route, authorization runs inside the request's unit of work, so the operation runs under the membership it was authorized with. | ARCH-11, D2 | `platform_024_` |
| PLATFORM-025 | A rank refusal inside an operation answers 403 `request.permission_denied`, records the refusal, and rolls the operation back. | SEC-08, DATA-27 | `platform_025_` |
| PLATFORM-026 | A rate-limit count commits in its own short transaction, so a request that fails afterwards is still counted. | SEC-05, SEC-12 | `platform_026_` |

### Jobs

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-011 | A job enqueued in a unit of work exists only if that unit of work commits. | DATA-27, D1 | `platform_011_` |
| PLATFORM-012 | A job runs with the user or workspace it was enqueued for, and its queries filter by them. | DATA-22 | `platform_012_` |
| PLATFORM-013 | A failing job retries with backoff, then moves to a dead-letter state. A succeeded job keeps no payload. | OPS-10 | `platform_013_` |
| PLATFORM-014 | A periodic job runs once per interval across all replicas, and a long run keeps its lease. | OPS-11 | `platform_014_` |

### Events and streams

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-015 | A published event exists only if its unit of work commits, and each replica's hub loads it once. | DATA-27, D20 | `platform_015_` |
| PLATFORM-016 | A workspace event reaches only streams whose principal holds the permission its type requires. Replay excludes events from before the membership. | SEC-17 | `platform_016_` |
| PLATFORM-017 | A committed access change closes or re-filters the affected streams. A missed signal is caught by the periodic re-check. | SEC-06, D35 | `platform_017_` |

### HTTP

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-018 | Same key and fingerprint returns the stored result. A different fingerprint gets 422. A request still in flight gets 409. A committed change always has its record. | API-12 | `platform_018_` |
| PLATFORM-019 | The health probes bypass rate limits, load shedding and timeouts, and return a plain status body. | OPS-05 | `platform_019_` |
| PLATFORM-020 | Requests beyond the concurrency limit get 503 with `Retry-After`. | OPS-07 | `platform_020_` |
| PLATFORM-021 | Every error is an RFC 9457 problem with `code` and `trace_id`, and internal errors expose nothing. | ERR-07, ERR-08, API-07 | `platform_021_` |
| PLATFORM-022 | Every response except the health probes carries `traceresponse`. | API-16 | `platform_022_` |

### Configuration

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-023 | Settings are trimmed and an empty value counts as unset. A missing required setting fails startup in the one startup-failure format. | OPS-01, OPS-03 | `platform_023_` |

## Design

### Unit of work

A unit of work is one transaction on the `app` pool. The route opens it, and the service receives it:
- A workspace route opens it through `Authorized<P>`. The extractor opens the transaction, asks `AccessPolicy::authorize` inside it, and hands the open unit of work to the handler with the proof (PLATFORM-024).
- A user route opens it from its `Principal`.
- A job handler opens it with `for_job`, from the job's `JobContext`.

Every query in it filters by tenant itself (DATA-22). The unit of work sets no database context, because there is no row-level security (D4).

### Request authentication

`SessionVerifier` hashes the presented access token with SHA-256 (SEC-03), and reads the session through the unique index on `sessions.access_token_hash`. It checks the expiry in SQL against `now()`. It returns a `Principal` holding the user's ID and the session's ID. A missing or unknown token answers 401 `request.unauthenticated`, and an expired one 401 `request.access_token_expired`.

### Refusals

`AccessPolicy::authorize` answers one of three things:
- not a member, which becomes 404 `request.not_found`
- a member without the permission, which becomes 403 `request.permission_denied`
- the member's role, from which `Authorized<P>` is built

A refusal is reported to the `SecurityEventSink`, which records it in its own short transaction, because the refused operation has written nothing. A member's refusal goes to the workspace's log, and a non-member's to the caller's own log. A rank refusal found inside an operation goes through `authz::refuse`: the operation's unit of work rolls back, and the refusal is recorded the same way (PLATFORM-025).

### Rate limits

A rate-limit check counts the request in its own short transaction, before the operation's unit of work opens, so the count survives whatever happens next (PLATFORM-026). A bucket's key is an HMAC of what it counts, such as an email or an IP address.

### Jobs

- `uow.enqueue` inserts a `jobs` row in the unit of work's transaction. The row records the user and workspace the unit of work acted for, so `JobContext` can restore them.
- The dispatcher claims runnable jobs with `FOR UPDATE SKIP LOCKED`. It runs each handler in a unit of work opened with `for_job`.
- A handler that fails is retried with exponential backoff and jitter until its maximum attempts, then dead-lettered. A succeeded job's payload is deleted. A dead letter keeps its payload for the configured retention, then loses it (OPS-10).
- A periodic job claims its `job_schedules` row with `FOR UPDATE SKIP LOCKED`, runs once, sets its next run, and extends its lease with a heartbeat while it runs (OPS-11).

### Events and the real-time hub

- `uow.publish` inserts an `events` row and calls `pg_notify` with the event's ID, both in the unit of work's transaction. The row holds the event's scope (a user or a workspace), its type, the permission its type requires, its payload of IDs, and its trace context (OBS-07). Postgres delivers notifications only at commit, and no trigger is involved (DATA-07).
- Each replica's listener receives the notification. The hub loads the event once, and delivers it to each stream whose scope matches and whose principal holds the required permission. The hub caches each stream's `Memberships` from `AccessPolicy::memberships`, and refreshes them on access-change signals.
- On reconnect, a stream replays from its last event ID. The replay query filters on the caller's user ID and on the workspace IDs in their `Memberships`, and skips events created before the membership in their workspace (SEC-17).
- An access change sends a signal on the `access_changed` channel with `pg_notify` inside the change's transaction. The signal holds only IDs: the kind of change, the user, and where relevant the workspace or session. It isn't stored (D35). The hub closes the affected streams, or re-filters them against a fresh `AccessPolicy::memberships`.
- Every configured interval (60 seconds by default), the hub re-checks each open stream's session through `SessionVerifier` and its memberships through `AccessPolicy`, and closes or re-filters it on any change (SEC-06).

### Idempotency

1. Before the handler runs, the idempotency layer reserves the key. In its own short transaction, it inserts an `idempotency_records` row with the principal, the key, the fingerprint and the state `in_flight`. An existing row for the same key decides the response instead: the stored result, 422 or 409 (API-12).
2. When the handler's unit of work commits, it updates the reservation, in that same transaction, to the state `done`, and stores the operation's result. A committed change therefore always has its record.
3. If the operation fails without committing, the layer deletes the reservation, so a retry runs again.
4. A reservation still `in_flight` after the request timeout (OPS-07) is treated as abandoned, and a new request may take it over.
5. A replay renders the stored result through the route's response mapping.
6. An entry older than 24 hours is treated as absent when it is read, whether or not a sweep has removed it yet.

### Request pipeline

The middleware order, from the outside in, is in [architecture.md §6](../architecture.md#6-request-lifecycle). The health probes are answered before it (OPS-05).

## Open design questions

| Question | Settled by |
|---|---|
| Whether the hub refreshes a stream's cached memberships on every access-change signal for that user, or only on the signal's workspace | `platform_016_` and `platform_017_`, measured against a user in several workspaces |
| Whether a job enqueued in workspace context re-asks `AccessPolicy` when it runs, so a role change in between is respected | `platform_012_`, with a job enqueued before the enqueuing member's role changes |
| Whether one listener connection per replica is enough under load, or events and access changes need separate connections | a load test of the hub, recorded under OPS-18 |
