# Inventory: Real-time events and multi-instance coordination (`RT`)

Reference: `AntWorker-API` @ `0210895` (2026-10-20). This file records what the old API does. It does not prescribe what the new one should do.

## Scope

**Covered.** Part 1 is the SSE stream `GET /v1/users/me/events`: how a connection is authenticated, heartbeats, `Last-Event-ID` replay and its limits, live filtering by workspace and user, connection limits, and shutdown. It also covers how events are published (the `sse_events` table, an `AFTER INSERT` trigger, `pg_notify`), how they reach clients connected to other instances (one shared `PgListener` per instance, then an in-process broadcast), retention cleanup of `sse_events`, and what a live stream does when membership changes or the session is revoked. `## Event catalog` lists every event type. Part 2 is multi-instance coordination: the per-process instance id, the background-task runner, the table-based distributed lock, background-task shutdown, the process shutdown sequence, and which state is shared or instance-local.

**Boundaries.**
- Emitting services own the business action. Here they appear only as triggers in the Event catalog.
- AUTH owns session validation. RT only records that the stream is authenticated once, at connect time.
- DATA owns what each background task does (except SSE retention, which is here). RT owns how tasks are scheduled and serialized across instances.
- HTTP owns rate-limit and idempotency storage (`rate_limits`, `idempotency_cache`). They are named here only as shared state.
- OBS owns the metric pipeline. OPS owns Compose, Caddy and the deploy flow.

**Files read in full:** `src/handlers/sse_notifications.rs`, `src/services/sse_event_publisher.rs`, `src/services/sse_notification_service.rs`, `src/repositories/postgresql/sse_event_repository.rs`, `src/repositories/postgresql/sse_notification_repository.rs`, `src/repositories/postgresql/background_task_lock_repository.rs`, `src/repositories/background_task_lock_repository.rs`, `src/utils/distributed_lock.rs`, `src/background.rs`, `src/shutdown.rs`, `src/app_state.rs`, `documentation/MULTI_INSTANCE.md`, `tests/sse_event_repository_live.rs`, `tests/sse_notification_repository_live.rs`, `tests/background_task_lock.rs`.

**Read in part:**
- Code: `src/server.rs` (lines 1-740, 740-2360 and the tests), `src/config/{server,environment,validation}.rs` (the SSE and lock settings), `src/services/traits.rs:960-1125` and `src/repositories/traits.rs:1820-2010` (SSE traits), `src/auth/middleware.rs`, `src/middleware/rate_limit.rs:124-150`, `src/handlers/{mod,auth,identities,health}.rs`, `src/utils/{request,trace,retry}.rs`, `src/services/telemetry_proxy_service.rs:100-235`, `src/migrate.rs:90-125`.
- Migrations: `migrations/0002`, `0003`, `0004`, `0006`, `0007` and `0008` (the SSE and lock objects).
- Every SSE emitter call site in `src/services/{workspace,workspace_member,user_invitation,user,identity,session,public_auth}_service.rs`.
- Tests and deployment files: the five `tests/*_fanout_live.rs` files (headers and test lists), `tests/rate_limit_middleware_http.rs:161-200,569-587`, `Caddyfile`, the relevant parts of `docker-compose.yml`, `.env.example:519-552,765-797`, and `documentation/LOGGING.md:195-240`.

`.env` and `alertmanager-secrets/` exist and were not opened.

## Behaviors

### RT-001 Open the user event stream

- **Behavior:** An authenticated user opens a long-lived `text/event-stream` response. It delivers events for every workspace the user belongs to and every event addressed to the user, until the client disconnects or the instance shuts down.
- **Kind:** `capability`
- **Trigger:** `GET /v1/users/me/events`
- **Auth:** Session only, through `user_scoped_auth_middleware` (Bearer access token in the `Authorization` header; no query-parameter token). No `X-Workspace-Id` header is needed. `enforce_email_verification_middleware` applies, so an unverified credentials user is blocked when `REQUIRE_EMAIL_VERIFICATION` is on.
- **Input & validation:** An optional `Last-Event-ID` header (see RT-008 and RT-010). There is no body or query.
- **Effects:**
  - One `sp_public_get_user_accessible_workspaces(user_id)` query runs at connect time, and the workspace names it returns are decrypted.
  - One broadcast receiver is created; no DB connection is held per stream.
  - The handler runs inside the `handler_events_stream` span.
  - The stream is wrapped in axum's default `KeepAlive`, which sends comment frames while idle (library behaviour, not in this repo).
- **Errors:**
  - `missing_auth_header`, `invalid_auth_header_format` or `invalid_access_token` → 401.
  - `email_verification_required` → 403.
  - `dependency_unavailable` → 503 when SSE is disabled (RT-002).
  - A repository error while loading workspaces or running replay is mapped by the HTTP domain's error envelope.
  - Once streaming has started there are no errors: every later failure is swallowed (RT-013, RT-014, RT-024).
- **Guarantees:** The order is: parse `Last-Event-ID`, check that SSE is enabled, load workspaces, subscribe to shutdown, build the stream. Response headers come only from axum's `Sse`; the handler adds none.
- **Source:** `src/handlers/mod.rs:235`, `src/handlers/sse_notifications.rs:88-140`, `src/auth/middleware.rs:27-41,85-112,284-313`, `src/server.rs:627-645`, `src/services/public_auth_service.rs:1540-1548`, `migrations/0005_stored_procedures.sql:770-786`
- **Tests:** None found for the streaming path. Every HTTP test builds the app with `sse_notification_service: None`.

### RT-002 Disabled backend rejects the stream

- **Behavior:** With `SSE_NOTIFICATION_BACKEND=disabled`, the endpoint returns 503 and no stream opens. No listener is spawned, no publisher exists, and no event is ever written by this instance.
- **Kind:** `rule` (a deployment switch that gates the capability)
- **Trigger:** `GET /v1/users/me/events` on an instance with the backend disabled.
- **Auth:** Same as RT-001. Auth and email-verification failures are returned before the 503.
- **Errors:** `dependency_unavailable` → 503, with the message "SSE notifications are disabled. Set SSE_NOTIFICATION_BACKEND=postgres to enable."
- **Effects:** Every emitter receives `None` and skips its publish. The `sse_event_cleanup` task is not spawned (RT-028).
- **Source:** `src/handlers/sse_notifications.rs:105-110`, `src/server.rs:1221-1359`, `src/config/environment.rs:253-265`, `src/config/server.rs:25-55`
- **Tests:** `tests/rate_limit_middleware_http.rs::rate_limit_exempt_sse_path_bypasses_exhausted_bucket` (uses the 503 as its signal).

### RT-003 Event frame and envelope

- **Behavior:** Each delivered event, live or replayed, is one SSE frame with three fields:
  - `id:` the `sse_events.id` UUID.
  - `event:` the `event_type` string.
  - `data:` the JSON envelope `{"data": <payload JSONB>, "trace_id": <string|null>}`.
- **Kind:** `capability` (the wire contract clients depend on)
- **Trigger:** Delivery of any stored event.
- **Effects:** None. The frame is built in memory.
- **Guarantees:**
  - Live and replay frames have the same shape.
  - A frame whose envelope fails to serialize is skipped at debug level. This cannot happen for valid `serde_json::Value` input.
  - Keepalive frames (RT-004) have no `id:`, so they never move the client's `Last-Event-ID`.
- **Source:** `src/services/sse_notification_service.rs:382-449,476-508`
- **Tests:** None found.

### RT-004 Heartbeats

- **Behavior:** Each stream emits a named `keepalive` event with data `{"timestamp": "<RFC3339 now>"}` every `SSE_KEEPALIVE_INTERVAL_SECS` seconds (default 15). The first one goes out immediately after any replay events. Axum's own idle `KeepAlive` comment frames run alongside.
- **Kind:** `guarantee` (connection-liveness promise to proxies and clients)
- **Trigger:** A timer per stream.
- **Input & validation:** The interval must be greater than 0. A value above 60 logs a warning (RT-026).
- **Effects:** None. Keepalive events are not stored and carry no id.
- **Guarantees:** Keepalives are merged with live events by `futures::stream::select`, which alternates between the two. They keep flowing even when live delivery has died (RT-024).
- **Source:** `src/services/sse_notification_service.rs:188-195,330-335`, `src/handlers/sse_notifications.rs:139`
- **Tests:** None found. The unit test at `sse_notification_service.rs:616-619` only works around the immediate first tick.

### RT-005 Live delivery is filtered by scope

- **Behavior:** A live notification reaches a stream only if one of these holds:
  - `scope_type = "workspace"` and `scope_id` is in the stream's workspace set.
  - `scope_type = "user"` and `scope_id` equals the connected user.

  Any other scope type is dropped. A dropped event triggers no payload fetch.
- **Kind:** `guarantee`
- **Trigger:** Each `pg_notify` received by the instance (RT-023).
- **Effects:** A trace-level log per filtered event, and a debug log for an unknown scope.
- **Guarantees:**
  - This in-process filter is the only authorization for live events, because `sse_events` has no RLS.
  - Workspace-scoped events go to every member regardless of their workspace permissions. See the Observations on events and permissions.
- **Source:** `src/services/sse_notification_service.rs:183,247-281`, `src/repositories/traits.rs:1856-1863`
- **Tests:** `src/services/sse_notification_service.rs::tests::live_stream_filters_out_cross_tenant_and_foreign_user_events`

### RT-006 The workspace set is a connect-time snapshot

- **Behavior:** A stream's workspace set is loaded once at connect time and never updated on the server. When membership changes, the change is not reflected until the client reconnects. The server asks the client to reconnect by sending a user-scoped `connection.refresh_required` event.
  - A removed member, a member who left, or a member of a deleted workspace keeps receiving that workspace's events on an open stream until the client reconnects.
  - A newly added workspace (just created, or joined by accepting an invitation) delivers nothing until the client reconnects.
- **Kind:** `guarantee` (a weak isolation property; listed so the spec can decide on it)
- **Trigger:** The membership mutations listed under `connection.refresh_required` in the Event catalog.
- **Effects:** On removal, the removed user's still-open stream receives `member.removed`, then `connection.refresh_required`. On workspace deletion, members receive `workspace.deleted`, then a `connection.refresh_required` for each member.
- **Guarantees:**
  - Server-side isolation after a membership change depends on the client honouring `connection.refresh_required`. Nothing on the server closes or re-filters the stream.
  - On account deletion, `connection.refresh_required` is deliberately not sent to the deleting user (`user_service.rs:953-960`), and not after member-only leaves either (`user_service.rs:974-977`).
- **Source:** `src/handlers/sse_notifications.rs:114-119`, `src/services/sse_notification_service.rs:183`. Emitters: `src/services/workspace_service.rs:96-118,321-343`, `src/services/workspace_member_service.rs:621-645,781-814`, `src/services/user_invitation_service.rs:272-296`
- **Tests:** None found for the snapshot itself. The emitter side is covered by `tests/workspace_fanout_live.rs::create_workspace_fans_out_connection_refresh_and_audits_workspace_create`, `tests/invitation_accept_fanout_live.rs::accept_invitation_fans_out_member_joined_and_refresh_and_audits`, `workspace_member_service.rs::tests::{remove_member_success_audits_and_fans_out, leave_workspace_success_clears_default_audits_and_fans_out}` and `workspace_service.rs::tests::delete_workspace_emits_audit_and_deletion_fanout`.

### RT-007 Authentication is checked only at connect

- **Behavior:** Once open, a stream is never re-authenticated. Access-token expiry, session revocation (logout, revoke, revoke-others, password change or reset, identity unlink, refresh-token reuse termination, expired-session cleanup) and account deletion all leave an open stream running. It keeps receiving user-scoped and workspace-scoped events until the client disconnects or the instance shuts down.
- **Kind:** `guarantee` (an absent guarantee that the spec must decide on)
- **Trigger:** Any session-ending action while a stream is open.
- **Effects:**
  - Most session-ending paths publish `session.revoked` (see the Event catalog), and the client is expected to log out and close the stream.
  - Refresh-token reuse termination (`public_auth_service.rs:850-880`) and expired-session cleanup publish nothing.
- **Source:** `src/handlers/sse_notifications.rs:88-140` (no later check), `src/services/sse_notification_service.rs:310-338` (the stream ends only on shutdown), `src/services/public_auth_service.rs:850`
- **Tests:** None found.

### RT-008 Replay missed events after a reconnect

- **Behavior:** If `Last-Event-ID` holds a UUID that exists in `sse_events`, the stream first sends the events strictly after that event, then switches to live delivery. The comparison is on `(created_at, id) > (cursor.created_at, cursor.id)`. Replayed events must match the current workspace set or the user, are ordered by `created_at ASC, id ASC`, and are capped by RT-009.
- **Kind:** `capability`
- **Trigger:** `GET /v1/users/me/events` with a known `Last-Event-ID`.
- **Input & validation:** The header is parsed with `Uuid::parse_str` after an empty-string check. Whitespace is not trimmed. Invalid values are treated as absent (RT-010).
- **Effects:** Two queries: the cursor row's `created_at` by id, then the keyset query.
- **Errors:** A DB error in either query fails the whole request through `?`, so no stream opens. It does not fall back to live-only.
- **Guarantees:**
  - The cursor lookup has no scope check: any user's event id is accepted as a position, but results are always scope-filtered.
  - The cursor row itself is excluded.
  - Replay is fully sent before any live event or keepalive.
- **Source:** `src/handlers/sse_notifications.rs:101-102`, `src/utils/request.rs:149-153`, `src/services/sse_notification_service.rs:128-170,327-335`, `src/repositories/postgresql/sse_notification_repository.rs:426-516`
- **Tests:** `tests/sse_notification_repository_live.rs::get_events_since_replays_only_events_the_reconnecting_user_may_see`, and `src/utils/request.rs::tests::parse_last_event_id_valid_uuid`

### RT-009 Replay is capped

- **Behavior:** Replay returns at most `SSE_EVENT_REPLAY_LIMIT` events (default 100), always the earliest after the cursor. Events between the last replayed event and the moment of subscription are never delivered, and the client gets no signal that replay was cut short.
- **Kind:** `rule`
- **Trigger:** Replay (RT-008).
- **Input & validation:** The limit must be greater than 0. A value above 500 logs a warning.
- **Source:** `src/repositories/postgresql/sse_notification_repository.rs:479-487`, `src/server.rs:1340`, `src/config/environment.rs:495,933-938,1575-1581`
- **Tests:** `tests/sse_notification_repository_live.rs::get_events_since_detects_gap_and_honors_limit`

### RT-010 Unknown, expired or malformed `Last-Event-ID` falls back to live only

- **Behavior:** The stream starts with live events only, with no replay and no indication to the client, in three cases:
  - The header is missing or empty.
  - The value is not a UUID.
  - The value is a UUID with no `sse_events` row (deleted by retention, or never existed).
- **Kind:** `rule`
- **Trigger:** `GET /v1/users/me/events`.
- **Effects:** A debug log, "Last-Event-ID not found - proceeding with live events only".
- **Source:** `src/utils/request.rs:149-153`, `src/repositories/postgresql/sse_notification_repository.rs:435-455`, `src/services/sse_notification_service.rs:151-166`
- **Tests:** `tests/sse_notification_repository_live.rs::get_events_since_detects_gap_and_honors_limit` (gap half), and `src/utils/request.rs::tests::{parse_last_event_id_none, parse_last_event_id_empty_string, parse_last_event_id_invalid_format, parse_last_event_id_partial_uuid}`

### RT-011 Subscribe before replaying, and de-duplicate

- **Behavior:** The stream subscribes to live notifications before it runs the replay query. Any live notification whose id was already replayed is dropped, so events committed while replay runs are delivered exactly once.
- **Kind:** `guarantee`
- **Trigger:** Stream creation with a known `Last-Event-ID`.
- **Guarantees:**
  - There is no gap between replay and live for events committed after subscription.
  - The workspace set is loaded before subscription (RT-001). Events committed between that query and the subscribe are covered only by replay, so a fresh connection without `Last-Event-ID` can miss them.
  - The de-duplication set holds at most `replay_limit` ids.
- **Source:** `src/services/sse_notification_service.rs:109-123,128-150,197-227`
- **Tests:** None found.

### RT-012 Replay uses current membership, not membership at the time of the event

- **Behavior:** Replay filters with the workspace set loaded at reconnect, not membership at the time each event was written.
  - A user who just joined a workspace and reconnects with an older cursor receives that workspace's events from between the cursor and the join, within the replay window. Such events can carry other members' emails, names and permissions.
  - Events from a workspace the user has left are not replayed.
- **Kind:** `guarantee` (an isolation property)
- **Trigger:** Replay after a membership change, for example the reconnect requested by `connection.refresh_required` with `reason: invitation_accepted`.
- **Source:** `src/handlers/sse_notifications.rs:114-119`, `src/repositories/postgresql/sse_notification_repository.rs:470-482`
- **Tests:** None found. The live replay test passes a fixed workspace set.

### RT-013 Live payload is fetched per event, and skipped on a miss

- **Behavior:** For each live notification that passes the filter, the stream reads `payload, trace_id FROM sse_events WHERE id = $1`. If the row is gone, the event is skipped at debug level. If the query fails, the event is skipped with a warning (`error_type = event_fetch_failed`, `action = failing_open`) and the stream continues.
- **Kind:** `hardening`
- **Trigger:** Live delivery.
- **Effects:** One DB query per event per subscribed stream that passes the filter.
- **Source:** `src/services/sse_notification_service.rs:277-281,382-449`, `src/repositories/postgresql/sse_notification_repository.rs:382-410`
- **Tests:** `tests/sse_notification_repository_live.rs::get_event_data_returns_payload_and_trace_id_or_none` (repository only).

### RT-014 A slow subscriber silently loses events

- **Behavior:** Each instance has one in-process broadcast channel with capacity 1024, shared by all its streams. A stream that falls more than 1024 notifications behind gets a `Lagged(n)` error. The error is mapped to `RepositoryError::ConnectionFailed("broadcast_lagged")`, logged as a warning (`error_type = broadcast_recv_failed`, `action = failing_open`), and the stream continues. The skipped events are never delivered and the client gets no signal.
- **Kind:** `hardening`
- **Trigger:** Back-pressure on a stream. Each stream processes events one at a time, with one DB query each.
- **Source:** `src/repositories/postgresql/sse_notification_repository.rs:55,154-157,337-366`, `src/services/sse_notification_service.rs:286-303`
- **Tests:** None found.

### RT-015 Live delivery order

- **Behavior:** Within one stream, live events arrive in the order the instance received their notifications. Notifications arrive in commit order, and each publish is its own autocommit `INSERT`. Publishes from one request run one after another, so a multi-event fan-out such as `session.revoked` before `identity.unlinked` arrives in code order.
- **Kind:** `guarantee`
- **Trigger:** Live delivery.
- **Guarantees:**
  - `filter_map` awaits each event's payload fetch before taking the next.
  - Replay order is `(created_at, id)`, where `created_at` is the `NOW()` of each autocommit insert.
  - The code relies on this ordering for the identity-unlink fan-out.
- **Source:** `src/services/sse_notification_service.rs:207-306`, `src/services/identity_service.rs:978-1001,1206-1213`
- **Tests:** None found.

### RT-016 Streams end on graceful shutdown

- **Behavior:** Every stream subscribes to the process shutdown broadcast and ends cleanly when it fires, including during replay. The client sees the connection close and is expected to reconnect, which lands on another replica and resumes through `Last-Event-ID`.
- **Kind:** `guarantee`
- **Trigger:** SIGTERM or SIGINT (RT-034).
- **Effects:** A debug log, "SSE stream received shutdown signal". The receiver count drops (RT-018).
- **Guarantees:** The stream holds its own receiver from `AppState::subscribe_shutdown()`, and the sender lives in `AppState`, so the future resolves only on a real signal.
- **Source:** `src/handlers/sse_notifications.rs:124`, `src/app_state.rs:201-203`, `src/services/sse_notification_service.rs:310-338`
- **Tests:** `src/shutdown.rs::tests::initiate_shutdown_sets_flag_and_notifies_subscribers` (broadcast only). None for stream termination.

### RT-017 No rate limit and no connection cap on the stream

- **Behavior:**
  - The rate-limit middleware skips any path ending in `/users/me/events`, so stream requests consume no bucket and carry no `X-RateLimit-*` headers.
  - There is no limit on concurrent streams per user, per session or per instance.
  - Idempotency handling is skipped because the request is a `GET`.
- **Kind:** `rule`
- **Trigger:** `GET /v1/users/me/events`.
- **Source:** `src/middleware/rate_limit.rs:131-150`, `src/middleware/idempotency.rs:291-296`. No limiter exists in `src/services/sse_notification_service.rs`.
- **Tests:** `tests/rate_limit_middleware_http.rs::rate_limit_exempt_sse_path_bypasses_exhausted_bucket`

### RT-018 Subscriber gauge

- **Behavior:** When OTel metrics are enabled, an observable up-down counter `sse_active_subscribers` reports the instance's current broadcast receiver count on each collection cycle. That count includes every open stream. The instance is identified by the `service.instance.id` resource attribute.
- **Kind:** `capability` (operator-facing signal)
- **Trigger:** The OTel metric collection cycle. Registered at startup only when a `MeterProvider` exists.
- **Effects:** The metric is shown on the Grafana `app-api` dashboard (`grafana/provisioning/dashboards/app-api.json:1405`).
- **Source:** `src/repositories/postgresql/sse_notification_repository.rs:197-209`, `src/server.rs:1263-1273`
- **Tests:** None found.

### RT-019 Publishing is post-commit, best-effort and outside the business transaction

- **Behavior:** A service publishes by calling `SseEventPublisher::publish_{workspace,user}_event`. The call runs one autocommit `INSERT INTO sse_events (event_type, scope_type, scope_id, payload, trace_id) ... RETURNING id` on the pool, after the business change has committed. Every call site catches the error, logs a warning (`error_type = sse_publish_failed`, `action = failing_open`) and still returns success. There is no transactional outbox and no retry, so delivery is at most once.
- **Kind:** `guarantee`
- **Trigger:** Every emitter in the Event catalog.
- **Effects:** One `sse_events` row per event, which fires the trigger in RT-022. A debug log, "… SSE event published successfully".
- **Guarantees:**
  - An event is lost if the insert fails or the process dies between commit and publish.
  - An event is skipped if the request errors after commit but before the publish. For example, `delete_workspace` and `leave_workspace` return early if `clear_default_workspace` fails.
  - Publishing adds request latency: one insert per event, which is N inserts for per-member or per-workspace fan-outs.
- **Source:** `src/services/sse_event_publisher.rs:63-110`, `src/repositories/postgresql/sse_event_repository.rs:55-106`, `src/services/traits.rs:1018-1085`, `src/services/workspace_service.rs:808-830`, `src/services/workspace_member_service.rs:755-778`
- **Tests:** Emitter unit tests named `*_succeeds_when_sse_publish_fails` in `session_service.rs`, `identity_service.rs`, `user_service.rs`, `user_invitation_service.rs`, `workspace_service.rs` and `workspace_member_service.rs`, and the `*_succeeds_when_sse_publish_fails` tests in `tests/*_fanout_live.rs`.

### RT-020 Only workspace and user scopes are accepted

- **Behavior:** `create` rejects any `scope_type` other than `"workspace"` or `"user"` with `RepositoryError::ValidationError` before touching the DB. The column also has `CHECK (scope_type IN ('workspace','user'))`.
- **Kind:** `hardening`
- **Trigger:** A publish call. Publishers only ever pass these two literals.
- **Source:** `src/repositories/postgresql/sse_event_repository.rs:62-69`, `migrations/0002_tables.sql:542`
- **Tests:** `tests/sse_event_repository_live.rs::create_rejects_an_invalid_scope_type_before_touching_the_database`

### RT-021 Each event carries the trace id of the request that caused it

- **Behavior:** On publish, the current span's W3C trace id (32 lowercase hex characters) is stored in `sse_events.trace_id` and returned in the envelope's `trace_id`. If no span is active, the invalid sentinel `000…0` is stored and an error is logged. The application never writes NULL.
- **Kind:** `capability` (clients use it to correlate their handling with the API trace)
- **Trigger:** A publish call.
- **Source:** `src/repositories/postgresql/sse_event_repository.rs:77,94`, `src/utils/trace.rs:104-126`
- **Tests:** `tests/sse_event_repository_live.rs::create_inserts_event_into_the_right_columns_and_returns_its_id` (sentinel), `tests/sse_notification_repository_live.rs::get_event_data_returns_payload_and_trace_id_or_none`

### RT-022 Notification trigger with a minimal payload

- **Behavior:** `AFTER INSERT ON sse_events FOR EACH ROW` calls `pg_notify('sse_event_created', json)`. The JSON holds only `id`, `event_type`, `scope_type`, `scope_id` and `created_at`; no payload. Postgres delivers it to listeners when the inserting transaction commits.
- **Kind:** `guarantee`
- **Trigger:** Any insert into `sse_events`.
- **Guarantees:**
  - Payload data never crosses the notification channel. It is read from the table only after the filter passes (RT-013).
  - Listeners never see rolled-back events.
- **Source:** `migrations/0007_triggers.sql:18-40`, `migrations/0008_grants.sql:167`
- **Tests:** `tests/sse_notification_repository_live.rs::trigger_pg_notify_payload_matches_the_consumer_contract`

### RT-023 Cross-instance fan-out through one shared listener per instance

- **Behavior:** When the backend is `postgres`, each instance spawns exactly one background task that holds one `PgListener` connection from the pool, `LISTEN`s on `sse_event_created`, and decodes each notification. It re-broadcasts the result on the instance's broadcast channel, which every local stream receives. Postgres sends every notification to every listening instance, so an event published on any replica reaches subscribers on all replicas.
- **Kind:** `guarantee`
- **Trigger:** Startup, in `server::run`.
- **Effects:**
  - One DB connection per instance.
  - The task runs in a root span `sse_shared_listener` with `component = background_task`.
  - A send with no receivers is ignored.
- **Guarantees:**
  - Delivery needs no sticky sessions: a client reconnecting to any replica gets live events there and replays from the shared table.
  - The listener is not part of the background-task lock (RT-029); every instance runs one.
- **Source:** `src/server.rs:1248-1330`, `src/repositories/postgresql/sse_notification_repository.rs:44,154-176,234-314,329-369`
- **Tests:** `tests/sse_notification_repository_live.rs::shared_listener_broadcasts_inserted_event_to_subscribers` (single instance).

### RT-024 Listener fault handling

- **Behavior:**
  - A notification whose payload does not decode is skipped with a warning (`pg_notify_payload_decode_failed`).
  - A stream error is logged as a warning (`pg_listener_stream_error`) and the loop continues, relying on sqlx to reconnect.
  - If the initial connect or `LISTEN` fails, the task returns `InitFailed{stage}`, which is logged at error level. If the stream ends, it returns `StreamEnded`, logged at warning level.

  In both terminal cases the task is never restarted. Open streams stay open with keepalives only, new streams still connect, and live events are unavailable on that instance until the process restarts. Readiness is unaffected.
- **Kind:** `hardening`
- **Trigger:** A malformed notification, a DB connection drop, or listener start-up failure.
- **Guarantees:**
  - Notifications sent while the listener is reconnecting are lost to open streams. Postgres does not queue them for a session that is not listening, and nothing replays them to a live stream; only a client reconnect with `Last-Event-ID` recovers them.
  - The broadcast sender is also held by the repository, so subscriber streams do not end when the listener exits.
- **Source:** `src/repositories/postgresql/sse_notification_repository.rs:75-86,106-115,239-314`, `src/server.rs:1302-1326`, `src/handlers/health.rs:74-75`
- **Tests:** `src/repositories/postgresql/sse_notification_repository.rs::tests::{parse_valid_payload_maps_all_fields, parse_malformed_payload_is_err_so_the_listener_skips_it}`

### RT-025 SSE event retention cleanup

- **Behavior:** Background task `sse_event_cleanup` runs `sp_cleanup_expired_sse_events(retention_hours)`, which deletes rows with `created_at < NOW() - retention_hours` and returns the count. It runs every `SSE_EVENT_CLEANUP_INTERVAL_HOURS` (default 1.0, converted to whole seconds), keeps rows for `SSE_EVENT_RETENTION_HOURS` (default 1), holds the distributed lock while it runs (RT-029), and makes up to 3 attempts through `retry_with_backoff`. Rows therefore live between the retention period and retention plus one interval, which bounds the replay window (RT-010).
- **Kind:** `rule`
- **Trigger:** Background task `sse_event_cleanup`. Spawned only when `ENABLE_BACKGROUND_CLEANUP=true` and the SSE backend is `postgres`.
- **Input & validation:** Retention must be greater than 0 and logs a warning above 24. The interval must be finite, above 0 and at least 1 s, and logs a warning above 24. The repository casts retention `u64 → i32`.
- **Effects:**
  - Deletes `sse_events` rows.
  - `log_cleanup_result("sse_events", …)` logs at info when rows were removed, debug when none were, and error after all retries fail, with `component = background_cleanup`.
  - The SP is `SECURITY DEFINER` with `search_path` pinned. It uses `idx_sse_events_created_at`.
- **Source:** `src/server.rs:1778-1839`, `src/services/sse_event_publisher.rs:122-131`, `src/repositories/postgresql/sse_event_repository.rs:119-139`, `migrations/0006_background_tasks.sql:252-269`, `migrations/0003_indexes.sql:149`, `src/background.rs:201-230`
- **Tests:** `tests/sse_event_repository_live.rs::cleanup_deletes_only_events_past_the_retention_window`, and `src/server.rs::tests::{conditional_tasks_require_their_backing, conditional_tasks_are_independent}`

### RT-026 Startup validation of SSE and lock settings

- **Behavior:** Startup aborts with a configuration error when any of these holds:
  - `SSE_NOTIFICATION_BACKEND` is anything other than `postgres` or `disabled` (case-insensitive).
  - `BACKGROUND_TASK_LOCK_DURATION_SECS` is 0 or less.
  - With the `postgres` backend only: the keepalive interval is 0, retention is 0, the replay limit is 0, or the cleanup interval is not finite, is 0 or less, or is under 1 s.

  Values above the recommended ceilings (keepalive over 60, retention over 24, interval over 24, replay limit over 500) only add warnings. A feature-status entry `sse_notifications` (mode, endpoint, settings) is logged at startup.
- **Kind:** `hardening`
- **Trigger:** Startup (`Config::from_env`, `Config::validate`).
- **Source:** `src/config/environment.rs:253-265,407-414,485-495,725-729,897-939,1549-1582,1679-1691`, `src/config/validation.rs:236-254`
- **Tests:** `src/config/environment.rs::tests::{validate_warns_on_sse_config_concerns, config_rejects_sse_cleanup_interval_nan, config_rejects_sse_cleanup_interval_infinite, config_rejects_sse_cleanup_interval_subsecond, config_accepts_sse_cleanup_interval_fifteen_minutes, config_rejects_background_task_lock_duration_zero_or_negative, config_rejects_zero_for_every_floor_guarded_field}`

### RT-027 Per-process instance id

- **Behavior:** Each process generates a UUID v4 `instance_id` at startup, before OTel is initialized. The same value is the lock-holder identity in `background_task_locks.instance_id` and the OTel `service.instance.id` resource attribute.
- **Kind:** `guarantee`
- **Trigger:** Startup.
- **Guarantees:**
  - The id is never persisted or reused. A restarted process is a new holder, so it cannot reclaim a lock its previous incarnation left behind until that lock's TTL expires.
  - It is not added as a log field (LOGGING.md).
- **Source:** `src/server.rs:857-864,891-921,1496-1513`
- **Tests:** None found.

### RT-028 Every instance schedules every enabled task

- **Behavior:** There is no leader election. On startup each instance decides which tasks to spawn with `plan_background_tasks`:
  - With `ENABLE_BACKGROUND_CLEANUP=false`, nothing spawns.
  - Otherwise `rate_limit_cleanup`, `auth_cleanup`, `idempotency_cleanup` and `audit_log_cleanup` always spawn.
  - `sse_event_cleanup` spawns only if SSE is enabled.
  - `re_encryption` spawns only if the key ring holds more than one version.
  - `object_deletion_reaper` spawns only if storage is enabled.

  Each spawned task loops on its own `tokio::time::interval`. The first tick fires immediately at startup, and every tick tries the lock (RT-029) and runs the body only if it gets it.
- **Kind:** `rule`
- **Trigger:** Startup, then each interval tick.
- **Effects:** A `background_task` span per cycle (`otel.name = "background_task {task_id}"`, `component = background_task`). An info log at startup for each task, saying whether it is enabled or disabled and why.
- **Guarantees:** Instances' timers are not aligned. Because the lock is released after each run, a task can run up to N times per interval across N instances, one after another. Each instance restart also triggers an immediate run.
- **Source:** `src/server.rs:743-797,1479-1484,1518-2115`, `src/background.rs:55-127`
- **Tests:** `src/server.rs::tests::{all_tasks_spawn_when_enabled_and_all_backings_present, nothing_spawns_when_cleanup_disabled, conditional_tasks_require_their_backing, conditional_tasks_are_independent}`

### RT-029 Table-based mutual-exclusion lock

- **Behavior:** `sp_task_acquire_lock(task_id, instance_id, secs)` does an `INSERT … ON CONFLICT (task_id) DO UPDATE` that sets the holder and `lock_expires_at = NOW() + secs`. The update happens only if the existing lock has expired or already belongs to the caller. The function then returns whether the caller is now the holder. The call never blocks waiting for the lock.
- **Kind:** `guarantee`
- **Trigger:** Every background-task tick, through `run_with_distributed_lock`.
- **Effects:** Upserts a `background_task_locks` row. When the lock is held elsewhere, the cycle is skipped with a debug log, "Skipped - another instance holds the distributed lock".
- **Guarantees:**
  - At most one live holder per task id at a time; concurrent first-time inserts are serialized by the primary key.
  - The caller can re-acquire its own lock.
  - Expiry uses the database clock, so clock skew between instances does not matter.
  - The SP is `SECURITY INVOKER` and runs as `app_user`. No advisory locks are involved, so it is safe with connection pooling.
  - Exclusion holds only while a run fits inside the TTL (RT-032).
- **Source:** `migrations/0006_background_tasks.sql:378-410`, `src/repositories/postgresql/background_task_lock_repository.rs:61-82`, `src/utils/distributed_lock.rs:47-85`, `migrations/0008_grants.sql:152`
- **Tests:** `tests/background_task_lock.rs::{task_lock_enforces_single_holder_reclaim_and_scoped_release, concurrent_acquire_yields_exactly_one_holder}`, and `src/utils/distributed_lock.rs::tests::{lock_acquired_operation_succeeds, lock_not_acquired_skips}`

### RT-030 The holder releases the lock after every run

- **Behavior:** After the task body finishes, whether it succeeded or failed, `sp_task_release_lock(task_id, instance_id)` deletes the row, but only if the caller is the holder. Releasing a lock the caller does not hold is a no-op. If the release fails, a warning is logged (`lock_release_failed`), the lock expires after its TTL, and the body's result is still returned.
- **Kind:** `guarantee`
- **Trigger:** The end of each locked run.
- **Source:** `src/utils/distributed_lock.rs:87-107`, `migrations/0006_background_tasks.sql:414-430`, `src/repositories/postgresql/background_task_lock_repository.rs:96-115`
- **Tests:**
  - `tests/background_task_lock.rs::task_lock_enforces_single_holder_reclaim_and_scoped_release` (step 4).
  - `src/utils/distributed_lock.rs::tests::{lock_is_released_after_operation_completes, release_error_still_returns_operation_result, lock_acquired_operation_fails}`
  - A mock lock repository at `src/repositories/background_task_lock_repository.rs:70-169`.

### RT-031 A failed lock check skips the cycle

- **Behavior:** If `try_acquire` returns a DB error, the cycle is skipped. The body does not run, nothing is surfaced to the caller, and a warning is logged (`lock_acquisition_check_failed`, `action = failing_open`). The next tick tries again.
- **Kind:** `hardening`
- **Trigger:** A DB error during lock acquisition.
- **Source:** `src/utils/distributed_lock.rs:48-73`
- **Tests:** `src/utils/distributed_lock.rs::tests::acquisition_error_skips_cycle_without_running_operation`

### RT-032 Lock TTL reclaim, with no heartbeat

- **Behavior:**
  - A lock left by a crashed or stuck holder can be taken by any instance once `lock_expires_at` passes. The TTL is `BACKGROUND_TASK_LOCK_DURATION_SECS`, default 300.
  - The lock is not extended while a run is in progress. A body that runs longer than the TTL, including its retry backoff, can overlap with another instance's run of the same task.
  - The object-deletion reaper adds its own protection against such overlap: its claim query uses `FOR UPDATE SKIP LOCKED`.
- **Kind:** `hardening`
- **Trigger:** Crash, hang or long run of a holder.
- **Input & validation:** The TTL must be greater than 0 (RT-026).
- **Source:** `migrations/0006_background_tasks.sql:394-399,444-479`, `src/config/server.rs:83-88`, `src/background.rs:91-104`
- **Tests:** `tests/background_task_lock.rs::task_lock_enforces_single_holder_reclaim_and_scoped_release` (step 3, using a 0 s TTL).

### RT-033 Background task shutdown

- **Behavior:**
  - Each task loop stops at the next point where it is waiting for its interval tick.
  - A body already running is not cancelled; the shutdown broadcast is buffered with capacity 16, so the loop sees it after the body finishes.
  - After the HTTP server has drained, the process awaits each task handle one after another, waiting up to 10 s per task. A task that times out is left running, detached, and may still hold its lock until the TTL expires.
- **Kind:** `guarantee`
- **Trigger:** The process shutdown sequence (RT-034).
- **Effects:** Info logs: `shutdown_signal_received`, `shutdown_complete`, "Waiting for background task to complete", "stopped gracefully". A warning when a task does not stop in time.
- **Source:** `src/background.rs:68-127,143-185`, `src/server.rs:2183-2202`
- **Tests:** `src/background.rs::tests::{spawn_background_task_responds_to_shutdown, shutdown_background_tasks_with_no_handles, shutdown_background_tasks_with_completed_handle, shutdown_background_tasks_times_out_on_stuck_task}`

### RT-034 Process shutdown sequence

- **Behavior:** On SIGTERM or SIGINT (SIGINT only on Windows) the process proceeds in order:
  1. Logs the signal.
  2. Sets the atomic shutdown flag, so `/health/ready` returns 503.
  3. Sends one broadcast that reaches SSE streams, the shared listener and the background tasks.
  4. Axum stops accepting connections and drains in-flight requests, with no time limit of its own.
  5. Waits for background tasks (RT-033).
  6. Closes the DB pool.
  7. Logs "Graceful shutdown complete".
  8. Flushes, then shuts down, the OTel providers.

  The shared listener's task handle is not awaited.
- **Kind:** `guarantee`
- **Trigger:** An OS signal.
- **Guarantees:**
  - The whole sequence is bounded by Compose's `stop_grace_period: 45s`, after which SIGKILL follows (`docker-compose.yml:61-62`).
  - Caddy does not probe readiness. A draining replica leaves rotation through dial-error retry and the 5 s DNS refresh.
- **Source:** `src/shutdown.rs:25-168`, `src/app_state.rs:191-193`, `src/handlers/health.rs:74-75`, `src/server.rs:1196,2141-2360`, `Caddyfile` (`api_upstream`)
- **Tests:** `src/shutdown.rs::tests::initiate_shutdown_sets_flag_and_notifies_subscribers`, `tests/health_probes_http.rs::readiness_drains_to_503_on_shutdown_while_liveness_stays_200`

### RT-035 Shared state lives in Postgres, with some deliberate instance-local state

- **Behavior:** The state replicas must agree on lives in Postgres: sessions, rate-limit windows (`rate_limits`, HTTP), the idempotency cache (`idempotency_cache`, HTTP), background-task locks, `sse_events`, the object-deletion queue, and migration history. Migrations are serialized by sqlx's advisory lock in the one-shot `migrate` command. The following is instance-local:
  - SSE connections, with their workspace-set snapshots and the broadcast channel.
  - The shared listener.
  - The shutdown flag.
  - The telemetry proxy's per-instance byte budget (`TELEMETRY_PROXY_INSTANCE_BYTES_PER_SECOND`). The fleet total therefore scales with the replica count.
- **Kind:** `guarantee`
- **Trigger:** Running N replicas behind Caddy round-robin.
- **Source:** `src/services/telemetry_proxy_service.rs:113-126,215-232`, `src/config/server.rs:340-347`, `src/migrate.rs:104-110`, `documentation/MULTI_INSTANCE.md:3-5`
- **Tests:** None found for the coordination aspect.

## Data owned

**`sse_events`** (`migrations/0002_tables.sql:517-543`):
- Columns: `id UUID PK DEFAULT uuid_generate_v4()`, `event_type TEXT NOT NULL`, `scope_type TEXT NOT NULL CHECK IN ('workspace','user')`, `scope_id UUID NOT NULL` (no foreign key), `payload JSONB NOT NULL` (plaintext), `trace_id VARCHAR(32)` (nullable, but the app always writes a value), `created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`.
- No RLS (`0004_row_level_security.sql:47-51`).
- Index `idx_sse_events_created_at` (`0003_indexes.sql:149`).
- Trigger `sse_event_notify_trigger` → `notify_sse_event_insert()`, a plain function running as the inserting role (`0007_triggers.sql:18-40`).
- Cleanup function `sp_cleanup_expired_sse_events(INTEGER)`: `SECURITY DEFINER`, `search_path` pinned (`0006_background_tasks.sql:252-269`).
- Grants: `0008_grants.sql:147,167`, and table DML through the blanket grant.

**`background_task_locks`** (`0002_tables.sql:592-608`):
- Columns: `task_id TEXT PK`, `instance_id TEXT NOT NULL`, `locked_at TIMESTAMPTZ NOT NULL DEFAULT NOW()`, `lock_expires_at TIMESTAMPTZ NOT NULL`. No RLS.
- `sp_task_acquire_lock(TEXT,TEXT,INTEGER) → BOOLEAN` and `sp_task_release_lock(TEXT,TEXT) → BOOLEAN`, both `SECURITY INVOKER` (`0006:378-430`). Grants at `0008:152-153`.
- At most 7 rows: `rate_limit_cleanup`, `auth_cleanup`, `idempotency_cleanup`, `audit_log_cleanup`, `sse_event_cleanup`, `re_encryption`, `object_deletion_reaper`.

**Postgres channel:** `sse_event_created`.

**Background tasks:** `sse_event_cleanup`, whose body is described in RT-025. This file also covers how all seven tasks are scheduled, locked and shut down (RT-028 to RT-033).

**In-memory, per instance:**
- SSE broadcast channel, capacity 1024 (`sse_notification_repository.rs:55`).
- Shutdown broadcast, capacity 16 (`shutdown.rs:48`).
- Shutdown `AtomicBool`.
- One workspace-id set and one replayed-id set per stream.

## Configuration

| Variable | Default | Validation | Read at |
|---|---|---|---|
| `SSE_NOTIFICATION_BACKEND` | `postgres` | `postgres` or `disabled`, case-insensitive; anything else fails startup | `src/config/environment.rs:253-265` |
| `SSE_KEEPALIVE_INTERVAL_SECS` | 15 | > 0 (postgres only); warning above 60 | `environment.rs:486-489,900-904,1551-1557` |
| `SSE_EVENT_RETENTION_HOURS` | 1 | > 0; warning above 24; cast to `i32` when used | `environment.rs:490,907-911,1559-1565`; `sse_event_repository.rs:123` |
| `SSE_EVENT_CLEANUP_INTERVAL_HOURS` | 1.0 (f64) | finite, > 0, at least 1 s; warning above 24 | `environment.rs:491-494,913-931,1567-1573`; `server.rs:1782-1783` |
| `SSE_EVENT_REPLAY_LIMIT` | 100 | > 0; warning above 500 | `environment.rs:495,933-938,1575-1581` |
| `ENABLE_BACKGROUND_CLEANUP` | true | `true` or `false`, case-insensitive | `environment.rs:407-410` |
| `BACKGROUND_TASK_LOCK_DURATION_SECS` | 300 (`i32`) | > 0 | `environment.rs:411-414,725-729` |

- **Per-task intervals.** These are read by `server.rs` and owned by DATA and the task owners: `RATE_LIMIT_CLEANUP_INTERVAL_HOURS`, `CLEANUP_INTERVAL_HOURS`, `IDEMPOTENCY_CLEANUP_INTERVAL_HOURS`, `AUDIT_LOG_CLEANUP_INTERVAL_HOURS`, `RE_ENCRYPTION_INTERVAL_HOURS`, `OBJECT_DELETION_REAPER_INTERVAL_SECS`.
- **Fleet size.** `API_REPLICAS` (OPS) sets N.
- **Mentioned but not read.** The comment at `src/config/server.rs:229` names `SSE_IDLE_TIMEOUT_SECS`, but the API never reads it.

## Cross-domain dependencies

**RT calls out to:**
- AUTH: `user_scoped_auth_middleware`, `enforce_email_verification_middleware` and `PublicAuthService::get_user_accessible_workspaces`, which uses `sp_public_get_user_accessible_workspaces` (WS/DATA).
- HTTP: router mounting, the rate-limit exemption, idempotency's `GET` skip, and the error envelope for errors before the stream starts.
- OBS: spans, `extract_or_invalid_in` for trace ids, the meter provider for `sse_active_subscribers`, `instance_id` as `service.instance.id`, and `log_cleanup_result`.
- DATA: `retry_with_backoff` and every task body except SSE retention.

**Calls into RT:**
- **Publishers.** AUTH (`public_auth_service`, `session_service`, `identity_service`), USER (`user_service`) and WS (`workspace_service`, `workspace_member_service`, `user_invitation_service`) call `SseEventPublisher`. Each receives `Option<Arc<dyn SseEventPublisher>>` at construction (`src/server.rs:260,301,319,329,338,351,378`).
- **Background tasks.** These run through `spawn_background_task` and the lock:
  - HTTP: `rate_limit_cleanup`, `idempotency_cleanup`.
  - AUTH: `auth_cleanup`.
  - AUDIT: `audit_log_cleanup`.
  - DATA: `re_encryption`.
  - FILE: `object_deletion_reaper`, whose own `FOR UPDATE SKIP LOCKED` claim is at `0006:476`.
- **Shutdown flag.** The `/health/ready` handler (HTTP) reads it.

**OPS:** Compose `stop_grace_period: 45s`, the Caddy `dynamic a` upstream with dial retry, the `db-migrate` one-shot that runs under sqlx's advisory lock, and the pool-sizing guidance in `MULTI_INSTANCE.md`.

## Event catalog

- **Envelope.** Every event is sent as `{"data": <payload>, "trace_id": "<32 hex>"}` (RT-003).
- **Scope.** `user` means delivered only to streams of the user `scope_id`. `workspace` means delivered to every stream whose connect-time workspace set contains `scope_id` (RT-005, RT-006).
- **Publish semantics.** All publishes are post-commit and best-effort (RT-019), and none fire when SSE is disabled. Line numbers point at the `publish_*_event` call.
- **Session ids.** Fields named `*_by_session_id` carry the initiating session, so the client can ignore events it caused itself.

| Event type | Scope (target) | Payload fields | Triggered by | Emitter |
|---|---|---|---|---|
| `session.created` | user (the session's user) | `session_id`, `device_name`, `auth_method`, `created_at` | `POST /auth/register`, `POST /auth/login`, `POST /auth/oauth` (through `create_session`). Not sent on token refresh. | `src/services/public_auth_service.rs:1280` |
| `session.revoked` | user | `session_ids` (array), `revoked_by_session_id` (null on reset) | `DELETE /users/me/sessions/{id}` and `POST /auth/logout`, as a single-id list | `src/services/session_service.rs:160` |
| `session.revoked` | user | same | `DELETE /users/me/sessions/others`, only if at least one session was revoked | `src/services/session_service.rs:304` |
| `session.revoked` | user | same, with `revoked_by_session_id: null` | `POST /auth/reset-password`, only if sessions were deleted | `src/services/public_auth_service.rs:2214` |
| `session.revoked` | user | same | `PUT /users/me/password`, only if other sessions were deleted | `src/services/user_service.rs:1217` |
| `session.revoked` | user | same | `POST /users/me/delete`, only if sessions existed. Sent first in the deletion fan-out. | `src/services/user_service.rs:930` |
| `session.revoked` | user | same | `POST /auth/identities/credentials/unlink` and `DELETE /auth/identities/{method}`, only if sessions were reaped. Sent before `identity.unlinked`. | `src/services/identity_service.rs:244` (called at `:990`, `:1210`) |
| `identity.linked` | user | `method`, `linked_by_session_id` | `POST /auth/identities/credentials`, `POST /auth/identities/oauth/complete` | `src/services/identity_service.rs:190,206` (called at `:373`, `:545`) |
| `identity.unlinked` | user | `method`, `unlinked_by_session_id` | `POST /auth/identities/credentials/unlink`, `DELETE /auth/identities/{method}` | `src/services/identity_service.rs:197,206` (called at `:996`, `:1212`) |
| `identity.email_changed` | user | `changed_by_session_id` | `POST /users/me/email/confirm` | `src/services/user_service.rs:1644` |
| `profile.updated` | user | `updated_by_session_id` | `PUT /users/me`, only when something changed | `src/services/user_service.rs:486` |
| `member.profile_updated` | workspace (each workspace the user owns or belongs to) | `workspace_id`, `user_id`, `updated_by_session_id` | `PUT /users/me`, only when something changed. Skipped if listing the user's workspaces fails. | `src/services/user_service.rs:524` |
| `telemetry.consent_changed` | user | `updated_by_session_id` | `PUT /users/me/telemetry-consent`, only when the value changed | `src/services/user_service.rs:1745` |
| `connection.refresh_required` | user (the creator) | `reason: "workspace_created"`, `workspace_id` | `POST /users/me/workspaces` | `src/services/workspace_service.rs:330` |
| `connection.refresh_required` | user (each member, owner included; the deleting user is excluded on account deletion) | `reason: "workspace_deleted"`, `workspace_id` | `DELETE /workspace`; `POST /users/me/delete` (each owned workspace) | `src/services/workspace_service.rs:101` |
| `connection.refresh_required` | user (the removed user) | `reason: "member_removed"`, `workspace_id` | `DELETE /workspace/members/{member_id}` | `src/services/workspace_member_service.rs:631` |
| `connection.refresh_required` | user (the leaver) | `reason: "workspace_left"`, `workspace_id` | `POST /workspace/leave`. Not sent on account deletion. | `src/services/workspace_member_service.rs:796` |
| `connection.refresh_required` | user (the joiner) | `reason: "invitation_accepted"`, `workspace_id` | `POST /users/me/invitations/{id}/accept` | `src/services/user_invitation_service.rs:280` |
| `workspace.updated` | workspace | `workspace_id`, `updated_by`, `updated_by_session_id` | `PUT /workspace`, only when the name or picture changed | `src/services/workspace_service.rs:662` |
| `workspace.transferred` | workspace | `workspace_id`, `workspace_name`, `previous_owner_id`, `new_owner_id`, `new_owner_name`, `transferred_by_session_id` | `POST /workspace/transfer` | `src/services/workspace_service.rs:1200` |
| `workspace.deleted` | workspace | `workspace_id`, `workspace_name`, `deleted_by`, `deleted_by_session_id` | `DELETE /workspace` (called at `:821`); `POST /users/me/delete`, for each owned workspace (`user_service.rs:962`) | `src/services/workspace_service.rs:74` |
| `member.joined` | workspace | `workspace_id`, `workspace_name`, `member_id`, `user_id`, `user_name`, `user_email`, `joined_by_session_id` | `POST /users/me/invitations/{id}/accept` | `src/services/user_invitation_service.rs:250` |
| `member.left` | workspace | `workspace_id`, `workspace_name`, `member_id`, `user_id`, `user_name`, `left_by_session_id` | `POST /workspace/leave` (called at `:769`); `POST /users/me/delete`, for each member-only workspace (`user_service.rs:978`) | `src/services/workspace_member_service.rs:74` |
| `member.removed` | workspace | `workspace_id`, `workspace_name`, `member_id`, `removed_user_id`, `removed_user_name`, `removed_by`, `removed_by_session_id` | `DELETE /workspace/members/{member_id}` | `src/services/workspace_member_service.rs:600` |
| `member.permissions_updated` | workspace | `workspace_id`, `workspace_name`, `member_id`, `user_id`, `user_name`, `old_permissions`, `new_permissions`, `updated_by`, `updated_by_session_id` | `PUT /workspace/members/{member_id}/permissions` | `src/services/workspace_member_service.rs:432` |
| `invitation.received` | user (the invitee, only if they have an account) | `invitation_id`, `workspace_id`, `workspace_name`, `invited_by_name`, `permissions` | `POST /workspace/invitations` | `src/services/workspace_member_service.rs:1076` |
| `invitation.received` | user (the invitee, only if they have an account) | same | `POST /workspace/invitations/{invitation_id}/resend` | `src/services/workspace_member_service.rs:1554` |
| `invitation.updated` | user (the invitee, only if they have an account) | `invitation_id`, `workspace_id`, `workspace_name`, `old_permissions`, `new_permissions` | `PUT /workspace/invitations/{invitation_id}` | `src/services/workspace_member_service.rs:1246` |
| `invitation.cancelled` | user (the invitee, only if they have an account; a failed lookup is ignored) | `invitation_id`, `workspace_id`, `workspace_name` (nullable) | `DELETE /workspace/invitations/{invitation_id}` (called at `:1366`); `POST /users/me/delete`, for each pending invitation the user authored in workspaces they do not own (`user_service.rs:995`) | `src/services/workspace_member_service.rs:145` |
| `invitation.declined` | workspace | `workspace_id`, `workspace_name`, `invitation_id`, `declined_by_email`, `declined_by`, `declined_by_session_id` | `POST /users/me/invitations/{id}/decline` | `src/services/user_invitation_service.rs:409` |
| `keepalive` | that stream only (not stored, no `id:`) | `timestamp` (RFC 3339) | Timer, every `SSE_KEEPALIVE_INTERVAL_SECS` | `src/services/sse_notification_service.rs:190-195` |

No event is published by background tasks, triggers or SPs. Every row comes from a request-path service.

## Observations

### Security and isolation

- **Open streams are never re-authorized.**
  - Where: `src/handlers/sse_notifications.rs:88-140`, `src/services/sse_notification_service.rs:183,310-338`.
  - What: membership is snapshotted at connect, and session validity is never checked again (RT-006, RT-007). A client that ignores `connection.refresh_required` or `session.revoked` keeps receiving the workspace's events after removal, logout, password reset or account deletion.
  - Why it matters: the in-code comments call stale delivery a "security issue" (`workspace_member_service.rs:627`, `:784`), but the only fix is the client choosing to reconnect.
- **Replay can deliver events from before the user joined.**
  - Where: `sse_notification_repository.rs:470-482`.
  - What: replay filters with membership loaded at reconnect (RT-012). A user who just accepted an invitation and reconnects with an older cursor can receive that workspace's earlier events, which carry other members' emails and permissions.
- **Workspace events ignore workspace permissions.**
  - What: they go to every member's stream. `member.joined` (`user_email`), `invitation.declined` (`declined_by_email`) and `member.permissions_updated` (permissions) therefore reach members without `ReadMember`.
  - Where: the REST listings of members and invitations require `ReadMember` (`workspace_member_service.rs:243,855`).
- **Payload privacy is inconsistent.**
  - What: some comments say payloads are ids-only to keep PII out of stored rows (`user_service.rs:466-473`, `:1636-1641`, `:1737-1741`), yet other events store emails, user names and workspace names in plaintext in `sse_events.payload`.
  - Where: elsewhere workspace names are encrypted at rest (comment at `workspace_service.rs:350-351`). The `sse_events` rows are kept for up to retention plus one interval.

### Delivery reliability

- **No outbox.**
  - Where: `sse_event_repository.rs:83-95`. `SseEventRepository` has no `_in_tx` method.
  - What: events are inserted on the pool after commit, so delivery is at most once (RT-019). Several flows also return an error after their DB change committed but before they publish (`workspace_service.rs:808-815`, `workspace_member_service.rs:755-761`).
- **Silent losses with no resync signal.** Four separate mechanisms drop events without telling the client:
  - Replay truncation (RT-009). The comments say "Client should poll REST API … if gap exceeds this limit" (`sse_notification_service.rs:62-64`, `config/server.rs:248-249`), but nothing tells the client.
  - Broadcast lag (RT-014).
  - Listener reconnect gaps (RT-024).
  - Listener death (RT-024).
- **Listener death has no recovery or health signal.**
  - Where: `server.rs:1293-1329`, `health.rs:74-75`.
  - What: after `InitFailed` or `StreamEnded` the task is not restarted and readiness stays 200. The instance keeps serving streams that receive only keepalives.
- **Comment contradicts code on listener exit.**
  - Where: `sse_notification_repository.rs:101-103` and `:132-133` say subscribers "will see their broadcast streams drain".
  - Why it is wrong: the repository keeps its own `Arc<broadcast::Sender>` (`:142`), so the channel never closes and streams do not end.
- **Payload fetch fans out to the database.**
  - Where: `sse_notification_service.rs:278-281`.
  - What: every subscribed stream that passes the filter runs its own `SELECT payload…` for each event. A workspace event with M connected member streams on one instance costs M queries on that instance.

### Shutdown

- **Possible shutdown hang** (inferred from axum and hyper behaviour, not verified).
  - Where: `src/handlers/sse_notifications.rs:124`.
  - What: a stream subscribes to the shutdown broadcast inside the handler. A request that reaches the handler after `initiate_shutdown` has already sent would never see the signal, because broadcast receivers only get later messages, so its stream would run until SIGKILL.

### Documentation and comments that disagree with code

- **MULTI_INSTANCE.md overstates statelessness.** `MULTI_INSTANCE.md:3-5` says "nothing instance-local". In fact SSE streams, their workspace snapshots, the listener and the telemetry per-instance byte budget are instance-local (RT-035).
- **MULTI_INSTANCE.md is silent on coordination.** It never mentions background-task locks, cross-instance SSE fan-out or `ENABLE_BACKGROUND_CLEANUP`.
- **Pool-sizing formula versus code.**
  - Where: `MULTI_INSTANCE.md:145` and the `docker-compose.yml:131` comment ("10 pool + 1 LISTEN") count the listener as an extra connection on top of `DATABASE_MAX_CONNECTIONS`.
  - What the code says: `PgListener::connect_with(&pool)` and its comment "acquires ONE dedicated connection from pool" (`sse_notification_repository.rs:239-240`) place it inside the pool.
- **"Only one instance per cycle" is wrong.**
  - Where: `src/background.rs:47` and `.env.example:526`.
  - What: these say only one instance executes a task "per cycle". The wording "at any time" (`server.rs:1492`, `background_task_lock_repository.rs:14`) is accurate. The lock gives mutual exclusion only. With unaligned per-instance timers and release-after-run, a task runs up to N times per interval, plus once on every instance start (RT-028).
- **Lock TTL versus long-running tasks.**
  - Where: `src/config/server.rs:84-85`, `.env.example:549`.
  - What: the TTL "must be longer than the longest cleanup operation", but nothing enforces or measures this, and there is no heartbeat (RT-032). `re_encryption` and the reaper, together with `retry_with_backoff`, can run long.
- **Lock row count.** `postgresql/background_task_lock_repository.rs:26` says "Max 6 rows". There are 7 task ids.
- **`.env.example` is stale.**
  - Where: `.env.example:775`.
  - What: it describes SSE as "real-time workspace audit log notifications". Its task list (`:529-537`) omits the object-deletion reaper and pending-email-change cleanup.
- **trace_id NULL claims are false.**
  - Where: the DDL comment (`0002_tables.sql:532-536`), `SseEventData` docs (`repositories/traits.rs:1871,1876`) and `sse_notification_service.rs:364`.
  - What: they say `trace_id` is NULL for system-generated events. The app always writes a 32-character value, the sentinel when no span is active (RT-021), and no system-generated events exist.
- **Trigger comment misattributes commit-time delivery.**
  - Where: `0007_triggers.sql:34-36`.
  - What: it credits `AFTER INSERT (not BEFORE)` with ensuring notification only after commit. In Postgres a `NOTIFY` is always delivered at commit, whatever the trigger timing.
- **"No persistence" claim.** `config/server.rs:32` says the backend has "No message persistence". Events are persisted in `sse_events` and replayed.
- **Phantom header in handler doc.** The handler doc lists a `Connection: keep-alive` response header (`sse_notifications.rs:73`). The app sets no headers itself.

### Edge cases and stale test comments

- **Cast overflow.** `retention_hours as i32` (`sse_event_repository.rs:123`) wraps for values at or above 2^31. Validation only requires greater than 0. A wrapped negative or zero retention would delete every row.
- **Stale test comments.**
  - `tests/sse_notification_repository_live.rs:4` says the repository "has no `#[cfg(test)]` module". It has one (`sse_notification_repository.rs:519-562`).
  - `tests/sse_event_repository_live.rs:6-7` makes a similar "only repository without a `_live` test" claim.
  - No production behavior is asserted only by tests.
- **No end-to-end SSE test.** Every HTTP-level test builds the app with SSE disabled (`sse_notification_service: None`). No test opens a real stream, exercises replay through the handler, or covers delivery across two listeners.

## Open questions

1. **Re-checking authorization on open streams.**
   - Session: should a stream be closed or re-checked when its session is revoked or expires, or the user is deleted?
   - Membership: should a membership change re-filter or close the stream, or is client-side `connection.refresh_required` / `session.revoked` handling the intended contract (RT-006, RT-007)?
2. **Replay filtering by membership.** Should replay filter by membership at the time of each event, so a new member gets no events from before they joined (RT-012)?
3. **Delivery guarantees.** Is at-most-once publishing acceptable, or should events be written inside the business transaction (an outbox), given that several flows can commit and then skip the publish?
4. **Resync signal.** Should the server tell the client to resync after any of the silent-loss cases: replay truncation, broadcast lag, listener reconnect, or listener death?
5. **Listener failure.** After the listener dies, should it be restarted, or should the instance report not-ready?
6. **Connection limits.** Should there be a per-user or per-instance limit on concurrent streams, given the rate-limit exemption and the per-event payload query on every stream?
7. **Events and permissions.** Should workspace events respect member permissions such as `ReadMember`? Should PII (emails, names, encrypted-at-rest workspace names) be allowed in stored payloads at all?
8. **Mixed SSE configuration.** Is a fleet with mixed `SSE_NOTIFICATION_BACKEND` values supported? A disabled instance publishes nothing, so actions served by it never reach users connected to enabled instances.
9. **Cleanup disabled everywhere.** If every instance runs with `ENABLE_BACKGROUND_CLEANUP=false`, nothing trims `sse_events` and the table grows without bound. Is a dedicated runner instance the intended pattern?
10. **Background-task schedule.** Should a task run once per interval across the whole fleet, rather than once per interval on each instance as now? And should long runs extend their lock (a heartbeat) instead of relying on the TTL being longer than the run (RT-028, RT-032)?
11. **Shutdown timeout.** On timeout, should background tasks be aborted, and their locks released, instead of being detached (RT-033)?
