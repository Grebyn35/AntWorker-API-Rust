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
| `_sqlx_migrations` | The applied migrations, recorded by `migrate` |

## Interface

- **Database:** `Database`, the pool of connections as the role `app`, which `serve` connects once at startup. Connecting runs the startup checks (PLATFORM-009, PLATFORM-030), and `begin` opens a unit of work.
- **Unit of work:** `UnitOfWork`, opened by the route (Design: Unit of work), with `executor`, `commit`, `enqueue` and `publish`.
- **Tasks:** `bootstrap` and `migrate`, run by the binary's subcommands of the same names (ARCH-14).
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
  - the list of telemetry names (OBS-05)

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `GET /health/live` | public, answered before the middleware stack (OPS-05) | PLATFORM-019 |
| `GET /health/ready` | public, answered before the middleware stack (OPS-05) | PLATFORM-019 |

## Behaviors

IDs 001 to 005, 007 and 008 are retired.

### Database

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-009 | `serve` refuses to start unless its database role meets DATA-13, and the refusal names each way the role breaks it. | DATA-13 | `platform_009_` |
| PLATFORM-010 | `bootstrap` creates the roles `owner` and `app` with their attributes and the `app` role's timeouts, and grants them their privileges on the database. Running it again leaves the roles, their attributes and settings, and the grants unchanged, and sets each role's password to the one in its URL. | DATA-12, DATA-19 | `platform_010_` |
| PLATFORM-027 | `migrate` connects as the role `owner`, and refuses any other role. It applies every embedded migration the database hasn't recorded, in version order. Running it again applies nothing, and concurrent runs apply each migration once. | DATA-09, DATA-12 | `platform_027_` |
| PLATFORM-028 | A migration that fails leaves no trace: its changes roll back, it isn't recorded, and `migrate` fails. | DATA-09 | `platform_028_` |
| PLATFORM-029 | `migrate` applies nothing while an applied migration differs from its file. It ignores a recorded migration that the binary doesn't contain, so an earlier release can run it after a later one. | DATA-09, DATA-10 | `platform_029_` |
| PLATFORM-030 | `serve` never applies migrations. It refuses to start until every embedded migration is applied, unchanged, and it starts against a schema that also holds a later release's migrations. | DATA-10, D9 | `platform_030_` |

### Units of work and authorization

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-006 | A unit of work that records a security failure commits that record, and the response is the failure. A failed sign-in answers 401. | ARCH-11, DATA-27, D36 | `platform_006_` |
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
| PLATFORM-022 | Every response except the health probes carries its trace context in `server-timing`. | API-16 | `platform_022_` |
| PLATFORM-031 | A request that runs past its timeout answers 503 `request.timed_out`. | OPS-07, ERR-03 | `platform_031_` |
| PLATFORM-032 | A request to a path that doesn't serve its method answers 405 `request.method_not_allowed`, with `Allow` listing the methods the path serves. | ERR-03, API-07 | `platform_032_` |

### Lifecycle

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-033 | `serve` fails startup when it can't install its handlers for the shutdown signals. | OPS-03, OPS-06 | `platform_033_` |

### Configuration

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-023 | Settings are trimmed and an empty value counts as unset. A missing required setting fails startup in the one startup-failure format. | OPS-01, OPS-03 | `platform_023_` |

## Design

### Database roles

`bootstrap` connects with `DATABASE_ADMIN_URL`, as an administrator such as the bundled Postgres superuser or a managed provider's admin role. The three database URLs must name the same database.
- It creates the roles `owner` and `app` with `LOGIN` and no other attribute. A role that already exists is given `LOGIN` and keeps its other attributes, because changing some of them takes a superuser, which a managed provider's admin role isn't. PLATFORM-009 refuses an `app` role whose attributes are unsafe.
- It sets each role's password from its URL. The password travels as a SCRAM-SHA-256 verifier that the API computes on a blocking thread (CODE-05), so the clear text never reaches the server, where statement logs could record it. The administrator connection logs none of its statements, because they carry the verifiers (OBS-04).
- It sets the `app` role's `statement_timeout` to 30 seconds and its `idle_in_transaction_session_timeout` to 15 seconds (DATA-19), and its `default_transaction_isolation` to `read committed` (DATA-25). Set on the role, these hold whatever the server or the database defaults to.
- On the database, it revokes `TEMPORARY` from `PUBLIC`, grants `CONNECT` to both roles, revokes `CREATE` on the schema `public` from `PUBLIC`, and grants it to `owner` only. A database created by a Postgres older than 15 grants `PUBLIC` that `CREATE`, which would let `app` create tables.

The first migration grants `app` `SELECT`, `INSERT`, `UPDATE` and `DELETE` on every table `owner` creates, revokes `EXECUTE` on new functions from `PUBLIC`, and lets `app` read the migrations table. Roles belong to the whole Postgres server, so every database on a server shares them, test databases included.

### Migrations

`migrations/` holds one file per change, named `<timestamp>_<module>_<description>.sql` (DATA-09), and the `app` crate embeds them as `MIGRATIONS`. `migrate` takes an advisory lock on the database, compares every applied migration with its file before it applies anything, then applies each missing migration in its own transaction together with its record. It never applies a down migration, and it refuses a migration marked `-- no-transaction`, which would run outside a transaction, so a failure part way through it would leave a trace (PLATFORM-028).

Queries are checked against the database while compiling (DATA-20), and `migrate` is part of the same binary. A migration is therefore applied with `just migrate` before the queries that use it are written.

### Connections

`Database::connect` opens a pool of up to `DATABASE_MAX_CONNECTIONS` connections as `app`, 10 by default. An operation waits up to 5 seconds for a free connection. The startup checks run on the first connection: the role (PLATFORM-009), then the schema (PLATFORM-030). The role check covers objects the role owns in every database of the server.

A database URL is the only source of its connection's settings (OPS-01). It sets the user, password, host and database itself, and the port too, or the port is 5432. It may set only the query parameters that `.env.example` lists for `DATABASE_URL`. The Postgres driver also reads libpq's environment variables, so the ones that `.env.example` lists, which could change a connection the URL describes, must be unset, even to an empty value. Every subcommand that reads a database URL refuses to start while one is set.

The certificates a verified connection trusts are the one input the URL doesn't decide alone. With `sslmode` set to `verify-ca` or `verify-full`, a connection trusts the certificates in the URL's `sslrootcert`, if any, together with the system's: the operating system's certificate store, or instead the files that `SSL_CERT_FILE` and `SSL_CERT_DIR` name when either is set. The driver adds `sslrootcert` to the system's certificates instead of replacing them. Those two variables are how an operator chooses the certificates a container trusts, such as in an image with no certificate store, so the API leaves them to the TLS library.

The driver reads the system's certificates each time it opens a verified connection: a short blocking read on the async runtime, which CODE-05 allows here. It happens when the pool opens a connection, not on every query. Certificates compiled into the binary would avoid the read, but the list the driver offers for that, `webpki-roots`, is licensed CDLA-Permissive-2.0, which CODE-12 doesn't allow.

Pools, connections and transactions stay inside `platform::db`. Code elsewhere reaches the database through a unit of work, and clippy refuses `sqlx::Pool`, `sqlx::PgConnection` and `sqlx::Transaction` outside it (DATA-21). The test support is the one other place that names them (TEST-06): it creates and drops test databases, arranges tests with statements no unit of work may run, and reads stored state as `owner` or as the administrator. It gets its connections from `platform::db::testing`, which only the `test-support` feature compiles and only the tests enable, so the API never hands out a connection, or the connection options whose `Debug` output includes the password (SEC-01).

### Unit of work

A unit of work is one transaction on the `app` pool. The route opens it after authentication, the rate-limit checks, the idempotency reservation and parsing, so no transaction waits on a slow client, and no request holds a pooled connection while it waits for a second one:
- A workspace route opens it together with `Authorized<P>`: the transaction opens, `AccessPolicy::authorize` runs inside it, and the handler receives the open unit of work with the proof (PLATFORM-024).
- A user route opens it from its `Principal`, and a public route from the `Database`.
- A job handler opens it with `for_job`, from the job's `JobContext`.

The service performs the writes and commits. A unit of work dropped without a commit rolls back, and nothing retries it. Every query in it filters by tenant itself (DATA-22). The unit of work sets no database context, because there is no row-level security (D4).

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
- Trace IDs come from the OpenTelemetry tracer (D10). Until telemetry export is built, the tracer exports nothing, and log lines still carry `trace_id` and `span_id` (OBS-02). The tracing libraries don't write the `server-timing` header, so the API writes it (PLATFORM-022).
- Each route group's timeout and body limit are constants (OPS-07, API-08). The concurrency limit, the readiness delay and the drain deadline are settings, because a deployment's capacity and grace period decide them (OPS-02, OPS-06).
- The delay before shutdown stops accepting connections is the first wait in the code, so `platform::clock` arrives with it (CODE-06).

### Telemetry names

The list in `platform::telemetry` holds one constant per name (OBS-05). A name that a semantic convention defines is declared as `convention("http.route", otel::HTTP_ROUTE)`, a `const fn` that fails compilation when the two names differ. The conventions' constants come from `opentelemetry-semantic-conventions` 0.33.1, which publishes semantic conventions 1.42.0, with its `semconv_experimental` feature for names still in development. A deprecated constant fails clippy.

The first names:
- From the conventions: `service.name`, `service.version`, `deployment.environment.name`, `trace_id`, `span_id`, `http.request.method`, `http.route`, `http.response.status_code`, `http.server.request.duration`, `error.type`, `exception.message`, `server.address`, `server.port`, `db.namespace`, and `user.id`, which is still in development.
- Own: `workspace.id`, `auth.session.id`, `job.id`, `job.name`, `realtime.event.id`, `startup.area` and `migration.versions`.

`check-instrument` reads the list's names, then every `tracing` event and span macro, every `#[instrument(fields(…))]` and every `.record("…")`, including calls nested inside other macros. It fails on:
- a name the list doesn't hold, or a `{ CONST }` name that isn't one of the list's constants
- a `record` name that isn't a literal
- a message that isn't constant (OBS-01)
- an `#[instrument]` without `skip_all` (OBS-04)
- an event above `DEBUG`, or a span, in module code (OBS-03, OBS-06)

## Open design questions

| Question | Settled by |
|---|---|
| How readiness tells an unreachable database from a busy pool, so that load alone never takes a replica out of rotation | the readiness tests of the request pipeline, run with the pool exhausted |
| How the health probes, which are answered before the middleware stack, fit API-14's three builders: through an exemption like the ones API-01, API-07 and API-16 give them, or through `public` outside the stack | the request pipeline's spec (build order 1.2) |
| How CI proves that every route is documented in `openapi.json` (API-17). `utoipa-axum` registers a route and its documentation in one `routes!` call, but its `.route()` adds a route with none | the request pipeline's spec (build order 1.2) |
| Which generator builds the typed client from `openapi.json`: openapi-to-rust (MIT, pre-1.0, with a rustls client), oas3-gen (MIT, one maintainer, with a native-tls client, which SEC-14 bans) or openapi-generator (a Java tool whose OpenAPI 3.1 support is in beta). progenitor is MPL-2.0 (CODE-12) and reads only OpenAPI 3.0, while utoipa 6 and aide write only 3.1. ARCH-01 allows three crates, so the client lives in the tests unless a proposal changes that | the typed-client spike, a throwaway test that compares them (build order 1.2) |
| Whether the hub refreshes a stream's cached memberships on every access-change signal for that user, or only on the signal's workspace | `platform_016_` and `platform_017_`, measured against a user in several workspaces |
| Whether a job enqueued in workspace context re-asks `AccessPolicy` when it runs, so a role change in between is respected | `platform_012_`, with a job enqueued before the enqueuing member's role changes |
| Whether one listener connection per replica is enough under load, or events and access changes need separate connections | a load test of the hub, recorded under OPS-18 |
| Whether two `bootstrap` runs at once must both succeed. They update the same roles, which belong to the whole server, so one can fail with "tuple concurrently updated". An advisory lock serializes runs only on the same database | the reference deployment (build order 1.5), which decides how `bootstrap` runs |
