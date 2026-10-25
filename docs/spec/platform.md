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
| `rate_limit_buckets` | Rate-limit counters, keyed by a hash-based message authentication code (HMAC) |
| `_sqlx_migrations` | The applied migrations, recorded by `migrate` |

## Interface

- **Database:** `Database`, the pool of connections as the role `app`, which `serve` connects once at startup. Connecting runs the startup checks (PLATFORM-009, PLATFORM-030), and `begin` opens a unit of work.
- **Unit of work:** `UnitOfWork`, opened by the route (Design: Unit of work), with `executor`, `commit`, `enqueue` and `publish`.
- **Tasks:** `bootstrap` and `migrate`, run by the binary's subcommands of the same names (ARCH-14).
- **Routes:** `Routes`, through which every module registers its routes and their documentation (API-14, API-17), and `http::app`, which wraps them in the request pipeline.
- **Errors:** `Category`, and `Problem<E>`, the one conversion of an error into a response (ERR-07).
- **Lifecycle:** `Readiness`, `ShutdownSignals` and `serve`, which runs the server until a shutdown signal and then shuts it down (OPS-05, OPS-06), and `probe`, which the `health` subcommand runs (PLATFORM-044).
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
  - the metric declarations: `Counter`, `Histogram` and `Gauge` (OBS-11)
  - the list of telemetry names (OBS-05)

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `GET /health/live` | public, answered before the middleware stack (OPS-05) | PLATFORM-019 |
| `GET /health/ready` | public, answered before the middleware stack (OPS-05) | PLATFORM-019, PLATFORM-037 |

## Behaviors

IDs 001 to 005, 007, 008 and 033 are retired.

### Database

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-009 | `serve` refuses to start unless its database role meets DATA-13, and the refusal names each way the role breaks it. | DATA-13 | `platform_009_` |
| PLATFORM-010 | `bootstrap` creates the roles `owner` and `app` with their attributes and the `app` role's timeouts, and grants them their privileges on the database. Running it again leaves the roles, their attributes and settings, and the grants unchanged, and sets each role's password to the one in its URL. | DATA-12, DATA-19 | `platform_010_` |
| PLATFORM-027 | `migrate` connects as the role `owner`, and refuses any other role. It applies every embedded migration the database hasn't recorded, in version order. Running it again applies nothing, and concurrent runs apply each migration once. | DATA-09, DATA-12 | `platform_027_` |
| PLATFORM-028 | A migration that fails leaves no trace: its changes roll back, it isn't recorded, and `migrate` fails. | DATA-09 | `platform_028_` |
| PLATFORM-029 | `migrate` applies nothing while an applied migration differs from its file. It ignores a recorded migration that the binary doesn't contain, so an earlier release can run it after a later one. | DATA-09, DATA-10 | `platform_029_` |
| PLATFORM-030 | `serve` never applies migrations. It refuses to start until every embedded migration is applied, unchanged, and it starts against a schema that also holds a later release's migrations. | DATA-10, D9 | `platform_030_` |
| PLATFORM-045 | Once `serve` has connected to the database, it writes one `INFO` line with the most connections the instance opens: `DATABASE_MAX_CONNECTIONS` for its pool, plus its own connection for readiness and the listener. | OPS-21, D20 | `platform_045_` |

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
| PLATFORM-019 | The health probes bypass rate limits, load shedding and the route groups' body limit, and return a plain status body. | OPS-05 | `platform_019_` |
| PLATFORM-020 | Requests beyond the concurrency limit get the 503 problem `request.overloaded`, with `Retry-After`. | OPS-07 | `platform_020_` |
| PLATFORM-021 | Every error is an RFC 9457 problem with `code` and `trace_id`, a path that doesn't exist included, and internal errors expose nothing. | ERR-07, ERR-08, API-07 | `platform_021_` |
| PLATFORM-022 | Every response except the health probes carries its trace context in `server-timing`. A request with a valid `traceparent` continues that trace. | API-16, OBS-07 | `platform_022_` |
| PLATFORM-031 | A request that runs past its timeout answers 503 `request.timed_out`, with `Retry-After`. | OPS-07, ERR-03 | `platform_031_` |
| PLATFORM-032 | A request to a path that doesn't serve its method answers 405 `request.method_not_allowed`, with `Allow` listing the methods the path serves. | ERR-03, API-07 | `platform_032_` |
| PLATFORM-034 | Every request except the health probes writes one `INFO` line when it completes, with its method, route, status and duration. A 5xx response also writes one `ERROR` line with its cause chain, except a panic's, whose one `ERROR` line the panic hook writes, and a 4xx response writes nothing above `INFO`. | OBS-03, ERR-10, ERR-12 | `platform_034_` |
| PLATFORM-035 | A query parameter that the route's documentation doesn't list answers 400 `request.invalid_query`. | API-09 | `platform_035_` |
| PLATFORM-036 | A request whose `Content-Length` exceeds its route's body limit answers 413 `request.payload_too_large`, before the body is read. | API-08 | `platform_036_` |

### Lifecycle

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-037 | `/health/ready` answers 200 `{"status": "ready"}` while the database answers the instance's own connection and the instance isn't draining. Otherwise it answers 503 with the status `draining` or `database_unavailable`. A pool with every connection in use doesn't change the answer. | OPS-05, D20 | `platform_037_` |
| PLATFORM-038 | On a shutdown signal, readiness answers `draining` at once. The server keeps serving for the shutdown delay, then stops accepting connections, waits for the requests in flight until the drain deadline, and returns. | OPS-06 | `platform_038_` |
| PLATFORM-044 | `health` sends `GET /health/ready` to the address in `LISTEN_ADDR`, the loopback address of its family when that address is unspecified, such as `0.0.0.0`. It exits 0 when the answer is 200. Otherwise it exits 1 and writes one startup-failure line on stderr that names the answer, or names the address as one nothing listens on, that didn't answer within 3 seconds, or that didn't answer with HTTP. | OPS-15, OPS-03, OPS-05 | `platform_044_` |

### Configuration

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-023 | Settings are trimmed and an empty value counts as unset. A missing required setting fails startup in the one startup-failure format. | OPS-01, OPS-03 | `platform_023_` |
| PLATFORM-041 | A subcommand that logs refuses to start while one of the OpenTelemetry variables that `.env.example` lists is set, even to an empty value, and the refusal names it. | OPS-01 | `platform_041_` |

### Telemetry

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-039 | With `OTEL_EXPORTER_OTLP_ENDPOINT` set, a subcommand that logs exports its spans, its log records and its metrics to the endpoint over the OpenTelemetry Protocol (OTLP) on HTTP, each with `service.name`, `service.version`, `deployment.environment.name` and a `service.instance.id` of its own process. A log record written inside a span carries the span's trace and span IDs. Without the setting, nothing is exported. | OBS-01, OBS-02, D10, D19 | `platform_039_` |
| PLATFORM-040 | Every request except the health probes records its duration in `http.server.request.duration`, with its method, its route if it matched one, its status, and the error code of a 5xx as `error.type`. A method that HTTP's specifications don't define is `_OTHER`, in the request's span and log line too. | OBS-11, OBS-15 | `platform_040_` |
| PLATFORM-042 | Before the process exits, `main` writes out the spans, log records and metrics not yet exported, the counts of the exports that failed included. | OPS-06, OBS-10 | `platform_042_` |
| PLATFORM-043 | `serve` records in `platform.database_reachable`, after every check, whether the instance's own database connection answered it, with the database's name as `db.namespace`. | OPS-05, D19 | `platform_043_` |

## Design

### Database roles

`bootstrap` connects with `DATABASE_ADMIN_URL`, as an administrator such as the bundled Postgres superuser or a managed provider's admin role. The three database URLs must name the same database.
- It creates the roles `owner` and `app` with `LOGIN` and no other attribute. A role that already exists is given `LOGIN` and keeps its other attributes, because changing some of them takes a superuser, which a managed provider's admin role isn't. PLATFORM-009 refuses an `app` role whose attributes are unsafe.
- It sets each role's password from its URL. The password travels as a verifier, the stored form of a password in SCRAM-SHA-256 (Salted Challenge Response Authentication Mechanism with SHA-256). The API computes the verifier on a blocking thread (CODE-05), so the clear text never reaches the server, where statement logs could record it. The administrator connection logs none of its statements, because they carry the verifiers (OBS-04).
- It sets the `app` role's `statement_timeout` to 30 seconds and its `idle_in_transaction_session_timeout` to 15 seconds (DATA-19), and its `default_transaction_isolation` to `read committed` (DATA-25). Set on the role, these hold whatever the server or the database defaults to.
- On the database, it revokes `TEMPORARY` from `PUBLIC`, grants `CONNECT` to both roles, revokes `CREATE` on the schema `public` from `PUBLIC`, and grants it to `owner` only. A database created by a Postgres older than 15 grants `PUBLIC` that `CREATE`, which would let `app` create tables.

The first migration grants `app` `SELECT`, `INSERT`, `UPDATE` and `DELETE` on every table `owner` creates, revokes `EXECUTE` on new functions from `PUBLIC`, and lets `app` read the migrations table. Roles belong to the whole Postgres server, so every database on a server shares them, test databases included.

Two `bootstrap` runs at once can't both be relied on: they update the same roles, and one can fail with "tuple concurrently updated", since nothing serializes them: `bootstrap` takes no lock, and an advisory lock would serialize only the runs on one database, while roles belong to the whole server. The reference deployment runs `bootstrap` once per deploy, and one deploy at a time (OPS-16), so its runs never overlap; the test support serializes the tests' runs.

### Migrations

`migrations/` holds one file per change, named `<timestamp>_<module>_<description>.sql` (DATA-09), and the `app` crate embeds them as `MIGRATIONS`. `migrate` takes an advisory lock on the database, compares every applied migration with its file before it applies anything, then applies each missing migration in its own transaction together with its record. It never applies a down migration, and it refuses a migration marked `-- no-transaction`, which would run outside a transaction, so a failure part way through it would leave a trace (PLATFORM-028).

### Connections

`Database::connect` opens a pool of up to `DATABASE_MAX_CONNECTIONS` connections as `app` ([`.env.example`](../../.env.example) gives its default). An operation waits up to 5 seconds for a free connection. The startup checks run on the first connection: the role (PLATFORM-009), then the schema (PLATFORM-030). The role check covers objects the role owns in every database of the server.

A database URL is the only source of its connection's settings (OPS-01). It sets the user, password, host and database itself, and the port too, or the port is 5432. It may set only the query parameters that `.env.example` lists for `DATABASE_URL`. The Postgres driver also reads libpq's environment variables, so the ones that `.env.example` lists, which could change a connection the URL describes, must be unset, even to an empty value. Every subcommand that reads a database URL refuses to start while one is set.

The certificates a verified connection trusts are the one input the URL doesn't decide alone. With `sslmode` set to `verify-ca` or `verify-full`, a connection trusts the certificates in the URL's `sslrootcert`, if any, together with the system's: the operating system's certificate store, or instead the files that `SSL_CERT_FILE` and `SSL_CERT_DIR` name when either is set. The driver adds `sslrootcert` to the system's certificates instead of replacing them. Those two variables are how an operator chooses the certificates a container trusts, such as in an image with no certificate store, so the API leaves them to the TLS library.

The driver reads the system's certificates each time it opens a verified connection: a short blocking read on the async runtime, which CODE-05 allows here. It happens when the pool, or the readiness check of PLATFORM-037, opens a connection, not on every query. Certificates compiled into the binary would avoid the read, but the list the driver offers for that, `webpki-roots`, is licensed CDLA-Permissive-2.0, which CODE-12 doesn't allow.

Pools, connections and transactions stay inside `platform::db`, and code elsewhere reaches the database through a unit of work (DATA-21). The test support is the one other place that names them (TEST-06): it creates and drops test databases, arranges tests with statements no unit of work may run, and reads stored state as `owner` or as the administrator. It gets its connections from `platform::db::testing`, which only the `test-support` feature compiles and only the tests enable, so the API never hands out a connection, or the connection options whose `Debug` output includes the password (SEC-01).

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

The order of the middleware stack is in [architecture.md §6](../architecture.md#6-request-lifecycle). The health probes are answered before the stack (OPS-05), and the security headers (SEC-11) wrap both.

**Routes and their documentation.** A module's `routes()` returns `Routes`, which only the builders of API-14 add to. A builder takes a route's documentation with its method router, as utoipa-axum's `routes!` makes both from the handler's `#[utoipa::path]`. A route serves `HEAD` wherever it serves `GET`, as HTTP asks. To each operation, the builder adds:
- its access level, as the extension `x-access`
- the problems the pipeline can answer on any route of its group: `request.invalid_query`, `request.payload_too_large`, `request.timed_out` and `request.overloaded`, whose 503 carries `Retry-After`, and `request.internal_error`

A route that documents its own response for one of those statuses keeps it, and lists the group's codes in it itself; `check-openapi` fails an operation that lacks a code another route of its group lists (ERR-13). clippy bans the routing methods of axum and utoipa-axum outside `platform::http`, so a route reaches the router only through a builder, which registers it at the paths its documentation names. A test sends every documented path each method OpenAPI can document that the path's documentation leaves out, and a method no route serves, and expects each to answer 405 with an `Allow` that lists exactly the documented methods. A method router whose fallback serves one of those methods fails it, so a route serves none of the methods OpenAPI can document that its documentation leaves out, however its method router was built (API-14, API-17). `http::app` mounts every module's routes, the fallbacks and the stack. `Routes::openapi` returns the routes' OpenAPI document, which covers the `/v1` routes and holds the router's own problems among its components: `NotFound`, and `MethodNotAllowed` with `Allow`. The binary's `openapi` subcommand writes it to `openapi.json`.

**Checking the document.** A response object in the document stays open to properties it doesn't list, because clients ignore the fields they don't recognize (API-05) and `/v1` may add response fields (API-18). The integration tests send plain HTTP requests to the API in process, and the test support checks every response against the document of the routes that answered it, with every object schema among the document's components closed, except the parts of an `allOf`, which each list only some of the properties of the whole. A field, status or content type the document doesn't list fails the test, and so does a header it lists that the response lacks (API-17). A `HEAD` response is checked against its `GET`, the only operation the document lists for both, since HTTP defines `HEAD` by `GET`. A request that no operation documents must get `NotFound` or `MethodNotAllowed`, except at the health probes, which the document leaves out and the check skips (OPS-05). The tests of `serve`'s shutdown and of the binary's log lines read their responses over a connection of their own, which the check doesn't see. A client generated from the document couldn't send the malformed requests that the pipeline's behaviors need.

**Errors.** `ErrorCode` gives an error its code, its title and its category (ERR-02, ERR-03). A handler returns `Result<T, Problem<E>>`, and `?` turns any `E: ErrorCode` into a `Problem<E>`, whose response is the problem of API-07, with the trace ID of the request's span. A 5xx `Problem` also carries its cause chain in a response extension, which the request log writes and removes (ERR-10). The fallbacks answer a path that doesn't exist with 404 `request.not_found`, and a method the path doesn't serve with 405 `request.method_not_allowed`, to which axum adds `Allow` (PLATFORM-032).

**Trace context.** The tracer is OpenTelemetry's SDK, which exports what [Telemetry export](#telemetry-export) says. It samples every request, whatever the flags of an inbound `traceparent` say, so every trace ID a response carries can lead to a trace (OBS-08). The outermost layer of the stack opens the request's server span, as the child of a valid inbound `traceparent`, and writes `server-timing: trace;desc=<traceparent>` with the span's own trace context, because the tracing libraries don't write that header (PLATFORM-022). A 5xx response sets the span's status to error and its `error.type` to the problem's code, or to `_OTHER` when the request log knows no error for it, and a 4xx leaves both unset, as OpenTelemetry's conventions for HTTP servers ask. A span is named after its method and route, such as `GET /v1/capabilities`, and a span whose method is `_OTHER` after the protocol instead, such as `HTTP /v1/capabilities`. The span, the request's log line and its metric give the method as one of the methods HTTP's specifications define (RFC 9110's, `PATCH` and `QUERY`), or as `_OTHER`, so a client can't add values without bound (PLATFORM-040, OBS-11). The API serves no other method, so no method it serves becomes `_OTHER`.

**Limits.** Each route group's timeout and body limit are constants (OPS-07, API-08). The `public`, `user` and `workspace` groups share a timeout of 10 seconds, twice the 5 seconds an operation waits for a pooled connection, and a body limit of 64 KiB, far above the largest JSON body the first slice accepts. A module spec may set others for its routes. The body limit refuses a declared `Content-Length` over it before the body is read, and stops any other body at the limit while it is read; the extractors that read bodies turn that into the 413 problem when they are built (build order 2.1). The concurrency limit is a setting, because a deployment's capacity decides it (OPS-02).

**Query parameters.** A route accepts the query parameters its documentation lists, which come from the type its handler parses them into. That type declares `#[into_params(parameter_in = Query)]`, without which utoipa leaves its fields out of the query. The builder answers any other name with 400 `request.invalid_query`, before the handler runs, so a route without query parameters refuses any query string (PLATFORM-035).

**Panics.** A panic hook replaces the default one, which would write plain text to stderr. It writes one `ERROR` line with the panic's message and location, inside the span the panic happened in, and counts the panic in `platform.panics`, on whatever thread it happened (ERR-12). Every subcommand that logs installs the hook. The catch layer answers the panic with the 500 problem, and marks the response so that the request log writes no `ERROR` line of its own, and the panic is logged once (ERR-10).

**Ignored errors.** `errors::ignore` writes one `WARN` line with a `snake_case` name for the failure as `error.type`, and its cause chain, and counts it in `platform.errors_ignored` with the same `error.type` (ERR-11). The pipeline ignores a failure to set a span's inbound parent, which leaves the request in a trace of its own, and a failure to write `server-timing`, which can't happen while the header's value holds only hexadecimal digits and dashes. `Telemetry::finish` ignores a failure to write out what one of the providers holds (PLATFORM-042).

### Logging

The logging setup writes one line per event to stdout: JSON in production, and text elsewhere (OBS-01). A JSON line holds `timestamp`, `level`, `message`, the event's fields, `service.name`, `service.version` and `deployment.environment.name`, and, inside a span, `trace_id` and `span_id` (OBS-02). `service.name` is the binary's name, so `just rename` renames it with the binary (ARCH-15). `LOG_LEVEL` is never coarser than `INFO`, because every request's completion line is written at `INFO` (PLATFORM-034).

A dedicated thread writes the lines, fed by a queue of 128,000 lines, so an event doesn't wait for stdout. When the queue is full, the event waits for room instead of being dropped (OBS-08): that wait is the one blocking call the logging setup makes on the async runtime (CODE-05). `main` writes out the queue before the process exits, waiting at most about a second for the logging thread, as `tracing-appender` does: a line it hasn't written by then is lost.

With telemetry export on, the same events also become OpenTelemetry log records: the bridge takes the events the lines take, with the same fields and severity, and the trace context of the span each was written in (PLATFORM-039). The records wait in a queue of their own, which drops a record that arrives while it is full (Telemetry export), so stdout drops no line while the process runs, and the logs store holds every record the API exported.

### Telemetry export

With `OTEL_EXPORTER_OTLP_ENDPOINT` set, every subcommand that logs exports its spans, its log records and its metrics over OTLP/HTTP with protobuf bodies, to `<endpoint>/v1/traces`, `/v1/logs` and `/v1/metrics`, the paths the protocol's specification defines (PLATFORM-039). Without it, nothing is exported, and the logs still go to stdout. [observability.md](../operations/observability.md) describes the collector that receives the telemetry.
- **Resource.** Every span, log record and metric carries `service.name`, `service.version`, `deployment.environment.name`, and `service.instance.id`: a version 4 UUID drawn from the operating system's random source when the process starts, so instances running side by side stay apart. The resource holds nothing else.
- **Spans.** A span's attributes are its own fields, all from the list of names (OBS-05): the bridge from `tracing` adds no source location, thread, target, level or timings. Events stay out of spans, since each event is a log record that its trace context links to its span. A number is recorded as a signed integer, because the bridge writes an unsigned one as text, and the collector's sampling compares `http.response.status_code` as a number (OBS-08).
- **Transport.** The three exporters share one blocking HTTP client, which runs on a thread of its own, so no export waits on the async runtime or needs it to run (CODE-05). An `https` endpoint is verified against the operating system's certificate store through rustls with its ring provider, as a verified database connection is (D38, SEC-14). SEC-09 lets the telemetry adapter use its library's client.
- **Batching.** A thread of the SDK exports the queued spans, and another the queued log records, each at least once a second and in batches of up to 512. Each queue holds 2,048 items, and an item that arrives while its queue is full is dropped. At a replica's capacity, the queues keep up: the load test counts as many spans the collector accepted as requests the replica served (OPS-18). A third thread exports the metrics every 15 seconds. An export that hasn't finished within 10 seconds fails.
- **Failures.** An exporter that can't be built fails startup (OBS-10), in the format of OPS-03, since logging doesn't exist yet. An export that fails at runtime is counted, and the process goes on (OBS-10). A wrapper around each exporter counts what it exported in `otel.sdk.exporter.span.exported`, `otel.sdk.exporter.log.exported` and `otel.sdk.exporter.metric_data_point.exported`, as the conventions define them: a failed export's items carry `error.type` `export_failed`, and exported items carry none. The exporter reports every failure the same way, a timeout included.
- **One source of settings.** The SDK and the exporter read `OTEL_*` variables of their own, such as headers, timeouts, queue sizes, the sampler and resource attributes, which would change what the API exports without `platform::config` knowing. `platform::config` reads the endpoint itself, and every subcommand that logs refuses to start while one of the others that `.env.example` lists is set (PLATFORM-041), as it does for libpq's.
- **Shutdown.** `Telemetry::finish`, which `main` calls last, shuts down the tracer, then the logger, then the meter, each after it writes out what it holds, so the last metrics count the exports that failed just before them (PLATFORM-042). The tracer and the logger each have 2 seconds, and the meter takes up to the 5 seconds its SDK allows itself, so writing out telemetry takes at most 9 seconds. Then the log lines are written out (OPS-06).

### Metrics

`platform::telemetry::metrics` holds the declaration types: `Counter`, `Histogram` and `Gauge` (OBS-11). A metric is a `static` that one of their `const fn new` builds from its name, unit, description and label names, and for a histogram its bucket boundaries. The first recording after telemetry is set up creates the instrument from the process's meter provider, and a recording before then is lost: an instrument created earlier would stay tied to no provider for the life of the process, so the metrics a test reads would depend on the order the tests run in (TEST-07). The binary sets telemetry up before any code records. A recording passes an array of label values as long as the declaration's label names, in their order, so a recording can't add a label. A value of `None` leaves its label out of the data point, as the conventions ask of `error.type` on a success. clippy bans getting an OpenTelemetry meter outside that module, and no metric can be created without one.

A name that a semantic convention defines is declared through `convention`, as the telemetry names are (OBS-05). The metric names come from `opentelemetry-semantic-conventions`' `metric` module, which holds the conventions' metric names.

`check-metrics` reads every declaration in the workspace. It fails on an own name that isn't `<module>.<name>` for the module that declares it, and on a label name that isn't one of the list's constants. `just metrics` writes the catalog, [metrics.md](../operations/metrics.md), from the declarations, and `check-metrics` fails while the committed catalog differs from it (OBS-12).

The metrics `platform` records:
- `http.server.request.duration`, by the request log, for every request it logs, with the boundaries the conventions advise (PLATFORM-040). It leaves out `url.scheme`, which the conventions require: the API receives every request over plain HTTP, behind the proxy that ends TLS, so the label would hold one value.
- `platform.panics`, by the panic hook (ERR-12)
- `platform.errors_ignored`, by `errors::ignore` (ERR-11)
- `platform.database_reachable`, 1 or 0, by the readiness check after every ping (PLATFORM-043)
- the exporters' counts, above

### Readiness and shutdown

- The instance keeps one connection to the database outside the pool, and pings it every 5 seconds: a protocol message the server answers without running a query. A ping fails after 2 seconds without an answer, and a failed ping reconnects at once. The readiness probe reads the last result, so it never waits for the database or for a pooled connection (PLATFORM-037). The listener of the real-time hub uses the same connection when it is built (build order 2.4).
- `ShutdownSignals` installs the handlers for SIGTERM and SIGINT, or Ctrl+C on Windows, before `serve` connects to the database. A failure to install them fails startup (OPS-03, OPS-06).
- On a signal, `serve` turns readiness to `draining`, waits the shutdown delay so the load balancer stops sending requests, stops accepting connections, and waits for the requests in flight until the drain deadline. It then returns, and a request still running ends with the process. The delay and the deadline are settings, because a deployment's health-check interval and grace period decide them (OPS-02). Then `main` writes out the logs.
- A failed or recovered check changes readiness before its log line is written, so whoever reads the line finds readiness changed.
- The `health` subcommand is the image's health check (OPS-15). It reads `LISTEN_ADDR` and no other setting, runs no async runtime, and writes nothing but its failure, so a check every few seconds adds nothing to the logs. `lifecycle::probe` connects over plain TCP and writes the request itself: the instance answers on its own address, which needs neither of the outbound clients (SEC-09). It waits 3 seconds for the connection and for each read of the answer, longer than the 2 seconds Windows takes to report a refused connection, and reads at most 4 KiB. A failure of the probe is written in the format of OPS-03, with `startup.area` `health`, because a one-shot subcommand's whole run is its startup; a missing or invalid `LISTEN_ADDR` fails as any setting does, with `config` (PLATFORM-023).

### Clock

`Clock` is a trait with two implementations (ARCH-13): the system clock, and the test clock, which only the `test-support` feature compiles. `now` reads the time, and `sleep` waits. A test clock's `sleep` completes when the test advances the clock past its deadline (TEST-07). Every wait in the workspace goes through it (CODE-06), the readiness checks and the shutdown delay included.

### Telemetry names

The list in `platform::telemetry` holds one constant per name (OBS-05). A name that a semantic convention defines is declared as `convention("http.route", otel::HTTP_ROUTE)`, a `const fn` that fails compilation when the two names differ. The conventions' constants come from `opentelemetry-semantic-conventions` 0.33.1, which publishes semantic conventions 1.42.0, with its `semconv_experimental` feature for names still in development. A deprecated constant fails clippy.

The names so far:
- From the conventions: `service.name`, `service.version`, `service.instance.id`, `deployment.environment.name`, `http.request.method`, `http.route`, `http.response.status_code`, `error.type`, `exception.message`, `code.file.path`, `code.line.number`, `server.address`, `server.port` and `db.namespace`.
- Trace context: `trace_id` and `span_id`, the top-level fields of JSON logs (OBS-05). The conventions' crate has no constant for them, so the list declares them without `convention`.
- The bridge's fields: `otel.name`, `otel.kind` and `otel.status_code`, which `tracing-opentelemetry` reads to name a span and set its kind and status, and which no JSON log line carries. `otel.status_code` comes from the conventions; the other two are the bridge's own (OBS-05).
- Own: `request.duration`, in seconds, `startup.area`, `migration.versions` and `database.connections`.

A name joins the list with the first code that writes it, such as `user.id` and `workspace.id` with authentication (build order 2.1).

`check-instrument` reads the list's names, then every `tracing` event and span macro, every `#[instrument(fields(…))]` and every `.record("…")`, including calls nested inside other macros. It fails on:
- a name the list doesn't hold, or a `{ CONST }` name that isn't one of the list's constants
- a `record` name that is neither a listed name nor one of the list's constants
- a message that isn't constant (OBS-01)
- an `#[instrument]` without `skip_all` (OBS-04)

## Open design questions

| Question | Settled by |
|---|---|
| Whether the hub refreshes a stream's cached memberships on every access-change signal for that user, or only on the signal's workspace | `platform_016_` and `platform_017_`, measured against a user in several workspaces |
| Whether a job enqueued in workspace context re-asks `AccessPolicy` when it runs, so a role change in between is respected | `platform_012_`, with a job enqueued before the enqueuing member's role changes |
| Whether one listener connection per replica is enough under load, or events and access changes need separate connections | a load test of the hub, recorded under OPS-18 |
| Whether `serve` needs a limit on the connections it accepts. Beyond the concurrency limit, a request gets 503 at once, but each open connection still holds memory: in the load test at 1.6 times a replica's capacity, with the client opening a connection for each request it couldn't send on one already open, a replica's memory reached 408 megabytes of its 512, and fell back afterwards | an overload test through Caddy, which opens the replicas' connections in a deployment, recorded under OPS-18 |
