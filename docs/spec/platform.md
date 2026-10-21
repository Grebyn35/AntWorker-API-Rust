# Spec: `platform`

## Purpose

`platform` holds the mechanisms every module uses: the unit of work and its row-level security (RLS) context, the proofs that authentication and authorization produce, background jobs, events and the real-time hub, the HTTP stack, errors, configuration and telemetry. It contains no product rules (ARCH-02). [architecture.md §4](../architecture.md#4-crates) lists its areas.

This spec covers what the first slice needs. Object storage, the telemetry proxy and the remaining areas are specified when the modules that use them are built.

## Owns

| Table | Holds |
|---|---|
| `events` | Published events, kept for replay |
| `jobs` | The job queue |
| `job_schedules` | One row per periodic job, holding its lease and next run |
| `idempotency_records` | Reserved and completed idempotency keys, with their stored results |
| `rate_limit_buckets` | Rate-limit counters, keyed by hash |

## Interface

- **Unit of work:** `UnitOfWork`, its ways of opening (Design: Proofs and contexts), `commit`, `enqueue` and `publish`.
- **Proof types:** `Principal`, `Authorized<P>`, `NewWorkspace`, `Memberships`, `Admission`, `PresentedKey`, `SecurityEvent`, `Refusal`, `JobContext` and `ScheduledRun`.
- **Traits that modules implement** (ARCH-13): `SessionVerifier`, implemented by `auth`; `AccessPolicy`, implemented by `access`; `SecurityEventSink`, implemented by `audit`; and the personal data contract.
- **Traits that modules use:** `Permission`, `ErrorCode`, and the job handler trait.
- **Services:**
  - the job registry and scheduler
  - the event types' declaration
  - the outbound HTTP clients (SEC-09)
  - the clock and the random source (CODE-06)
  - the metric declaration macro (OBS-11)

## Behaviors

### Context and row-level security

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-001 | A transaction opened without context sees no rows in any table that has RLS, and raises no error. | DATA-15 | `platform_001_` |
| PLATFORM-002 | Each proof type opens a unit of work with exactly the context Design lists for it, and with no other. | DATA-16 | `platform_002_` |
| PLATFORM-003 | Context ends with its transaction, including on a pooled connection that is reused. | DATA-16 | `platform_003_` |
| PLATFORM-004 | A presented key exposes only its matching row, whichever active key version that row was stored under, and the lookup uses the matched column's unique index. | DATA-17, D32 | `platform_004_` |
| PLATFORM-005 | Sign-in moves a key-scoped unit of work to the signed-in user's context within the same transaction. | DATA-16, ARCH-11 | `platform_005_` |
| PLATFORM-006 | A recorded failure commits its audit entry, the unit of work can write nothing else, and the response is the failure: a failed sign-in answers 401. | DATA-16, DATA-27, D36 | `platform_006_` |
| PLATFORM-007 | A new workspace and its owner's membership are created in one unit of work, under RLS. | DATA-08, D33 | `platform_007_` |
| PLATFORM-008 | A user sees exactly the invitations addressed to their email, in every workspace. Joining moves the unit of work into the invitation's workspace, within the same transaction. | DATA-15, D34 | `platform_008_` |
| PLATFORM-009 | Startup refuses unsafe database roles. | DATA-13 | `platform_009_` |
| PLATFORM-010 | `bootstrap` creates the three roles with their attributes, and running it again changes nothing. | DATA-12 | `platform_010_` |

### Jobs

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-011 | A job enqueued in a unit of work exists only if that unit of work commits. | DATA-27, D1 | `platform_011_` |
| PLATFORM-012 | A job runs with the context of the unit of work that enqueued it. | DATA-16, DATA-18 | `platform_012_` |
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

### Context values

The unit of work sets these transaction-local settings. The SQL accessors `app.ctx_uuid(name)`, `app.ctx_uuid_array(name)` and `app.ctx_bytea_array(name)` read them, returning NULL or an empty array when a setting is missing or empty, so a reused pooled connection never raises a cast error.

| Setting | Holds |
|---|---|
| `app.user_id` | The acting user's ID |
| `app.user_email_hashes` | The acting user's email hash, under every active key version |
| `app.workspace_id` | The workspace the operation acts in |
| `app.workspace_ids` | All of the acting user's workspaces |
| `app.<key>` | A presented key: its hash under every active key version, or a public file's ID |

### Policy forms

Every policy is one or more of these comparisons, joined by `OR` (DATA-15):

| Form | Comparison | Used on |
|---|---|---|
| Workspace | `workspace_id = app.ctx_uuid('app.workspace_id')` | workspace tables; on `workspaces` the column is `id` |
| User | `user_id = app.ctx_uuid('app.user_id')` | user tables; on `users` the column is `id` |
| Across workspaces | `workspace_id = ANY(app.ctx_uuid_array('app.workspace_ids'))` | tables read across the caller's workspaces, for example `events` for replay |
| Hashed key | `<key_column> = ANY(app.ctx_bytea_array('app.<key>'))` | the tables in Keys |
| Public file | `id = app.ctx_uuid('app.public_file_id') AND visibility = 'public'` | file tables |
| Addressed to the caller | `email_hash = ANY(app.ctx_bytea_array('app.user_email_hashes'))` | `invitations` |
| Co-member | `id IN (SELECT user_id FROM workspace_memberships WHERE workspace_id = app.ctx_uuid('app.workspace_id'))` | `users` |

A key-scoped comparison may add a constant condition on the same row, such as `AND visibility = 'public'`.

### Proofs and contexts

| Opened with | Proof, and who builds it | Context |
|---|---|---|
| `for_user` | `Principal`: `platform::authn`, from a verified session | user and email hashes |
| `for_workspace` | `Authorized<P>`: `platform::authz`, after `AccessPolicy` allows the request | user and workspace |
| `for_new_workspace` | `NewWorkspace`: `platform::authz`, from a `Principal`, with a freshly generated UUIDv7 workspace ID | user and the new workspace |
| `for_memberships` | `Principal` and `Memberships`: `platform::authz`, from `AccessPolicy` | user and all of their workspaces |
| `for_refusal` | `Refusal`: `platform::authz`, when `AccessPolicy` confirms the caller is a member without the route's permission, or the rank rule refuses them | user and workspace, able only to write audit entries |
| `for_presented_key` | `PresentedKey`: the module listed in Keys | one key |
| `for_job` | `JobContext`: `platform::jobs`, from the job row | the enqueuing unit of work's context |
| `for_scheduled_job` | `ScheduledRun`: `platform::jobs` | none, on the `worker` pool |

A unit of work can change its context once, inside the same transaction:

| Change | Proof, and who builds it | From, and to |
|---|---|---|
| `sign_in` | `Principal`: `auth`, when a credential check succeeds | from key-scoped to user context |
| `record_failure` | `SecurityEvent`: `auth`, for a failed or suspicious attempt against an identified user | from key-scoped to audit-only for that user |
| `join` | `Admission`: `invitations`, from an invitation the caller's context can see | from user context to the invitation's workspace |

### Keys

| Key | Built by | Context setting | Matched against |
|---|---|---|---|
| Access token | `auth` | `app.access_token_hashes` | `sessions.access_token_hash` |
| Refresh token | `auth` | `app.refresh_token_hashes` | `sessions.refresh_token_hash` |
| Sign-in email | `auth` | `app.lookup_email_hashes` | `users.email_hash` |
| Verification code | `auth` | `app.code_hashes` | `verification_codes.code_hash` |
| Provider subject | `auth`, after a verified OAuth exchange | `app.provider_subject_hashes` | `identities.provider_subject_hash`, the hash of provider and subject together |
| Public file ID | `files` | `app.public_file_id` | the file's `id`, with `visibility = 'public'` |

Every matched column has a unique index. Each hash is an HMAC-SHA-256 under a versioned key (SEC-03). A row stores the hash under the version current when it was written. The context holds the hash under every active version, so a row matches during a rotation (D32).

### Jobs

- `uow.enqueue` inserts a `jobs` row in the unit of work's transaction. The row records the context kind and IDs, so `JobContext` can restore them.
- The dispatcher claims runnable jobs with `FOR UPDATE SKIP LOCKED` on the `worker` pool. It runs each handler on the `app` pool in a unit of work opened with `for_job`.
- A handler that fails is retried with exponential backoff and jitter until its maximum attempts, then dead-lettered. A succeeded job's payload is deleted. A dead letter keeps its payload for the configured retention, then loses it (OPS-10).
- A periodic job claims its `job_schedules` row with `FOR UPDATE SKIP LOCKED`, runs once, sets its next run, and extends its lease with a heartbeat while it runs (OPS-11). A periodic job that spans tenants opens its unit of work with `for_scheduled_job`.

### Events and the real-time hub

- `uow.publish` inserts an `events` row and calls `pg_notify` with the event's ID, both in the unit of work's transaction. The row holds the event's scope (a user or a workspace), its type, the permission its type requires, its payload of IDs, and its trace context (OBS-07). Postgres delivers notifications only at commit, and no trigger is involved (DATA-07).
- Each replica's listener receives the notification. The hub loads the event once through the `worker` pool, and delivers it to each stream whose scope matches and whose principal holds the required permission. The hub caches each stream's memberships from `AccessPolicy`, and refreshes them on access-change signals.
- On reconnect, a stream replays from its last event ID, in a unit of work opened with `for_memberships`. Events created before the stream's membership in their workspace are skipped (SEC-17).
- An access change sends a signal on the `access_changed` channel with `pg_notify` inside the change's transaction. The signal holds only IDs: the kind of change, the user, and where relevant the workspace or session. It isn't stored (D35). The hub closes the affected streams, or re-filters them against a fresh `AccessPolicy` result.
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
| How `JobContext` stores a workspace context's permission set, or whether a job re-asks `AccessPolicy` when it runs | `platform_012_`, with a job enqueued before the enqueuing member's role changes |
| Whether one listener connection per replica is enough under load, or events and access changes need separate connections | a load test of the hub, recorded under OPS-18 |
