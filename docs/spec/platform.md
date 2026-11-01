# Spec: `platform`

## Purpose

`platform` holds the mechanisms every module uses: the unit of work, the proofs that authentication and authorization produce, background jobs, events and the real-time hub, the HTTP stack, errors, configuration and telemetry. It contains no product rules (ARCH-02). [architecture.md §4](../architecture.md#4-crates) lists its areas.

This spec covers what the first slice needs. Object storage, the telemetry proxy and the remaining areas are specified when the modules that use them are built.

## Owns

| Table | Holds |
|---|---|
| `events` | Published events, kept for replay |
| `jobs` | The job queue: one row per job that waits, runs or is dead-lettered, with its kind, payload, scope, trace context, attempts and lease. A job's row goes when it succeeds. |
| `job_schedules` | One row per periodic job, holding its lease and next run |
| `idempotency_records` | One row per user and idempotency key whose operation committed, with its fingerprint and its result |
| `rate_limit_buckets` | Rate-limit counters, keyed by a hash-based message authentication code (HMAC) |
| `_sqlx_migrations` | The applied migrations, recorded by `migrate` |

## Interface

- **Database:** `Database`, the pool of connections as the role `app`, which `serve` connects once at startup. Connecting runs the startup checks (PLATFORM-009, PLATFORM-030), and `begin` opens a unit of work.
- **Unit of work:** `UnitOfWork`, opened with the request's `Client`, or with a job's `JobContext` through `for_job` (Design: Unit of work), with `executor`, `commit`, `enqueue`, `publish_to_user` and `publish_to_workspace`, and `claim` and `commit_with` for an idempotency key.
- **Mail:** `MailConfig`, the email settings that `ServerConfig` holds (MAIL-006), `Transport`, the transport over the Simple Mail Transfer Protocol (SMTP), and `Email`, a rendered email it sends (Design: Mail).
- **Jobs:** `Job`, the trait of a queued job's payload, which names its kind, its most attempts and its time limit; `JobScope`, the user or workspace a job runs for, which only a proof builds; `JobFailure`, which says whether a failed run may be retried; and `Jobs`, the registry of every queued and periodic job's handler, whose `run` runs the dispatcher and the scheduler until shutdown (Design: Jobs).
- **Request context:** `Client`, the request's client IP (PLATFORM-047) and user agent.
- **Rate limits:** `RateLimits`, which a route takes from the request and hands its service, whose `check` counts the request in a bucket that the module names, and refuses it once the bucket is full, for a per-action limit such as AUTH-006 (PLATFORM-026). `RateLimiter`, whose `count` counts in a bucket, does the counting for it and for the pipeline's IP and user limits.
- **Request bodies:** `Json<T>`, the extractor of a JSON body (PLATFORM-054), `Idempotent<T>`, which a keyed route extracts instead, the body with its request's `IdempotencyKey` (PLATFORM-058), and `FieldErrors`, which collects every field that a request type's conversion into domain types refuses (PLATFORM-056), or every query parameter (PLATFORM-035).
- **Idempotency:** `UnitOfWork::claim` and `commit_with`, which keep an operation's result under its key (API-12).
- **Lists:** `Query<T>`, the extractor that parses a request's query parameters into `T`, the type that documents them, from which a route reads `Page`, a list's `limit` and `cursor` (API-06), and `Search`, its `search` (API-20), and `List<T>`, the body of a page of items, with its next cursor (API-05).
- **Tasks:** `bootstrap` and `migrate`, run by the binary's subcommands of the same names (ARCH-14).
- **Routes:** `Routes`, through which every module registers its routes and their documentation (API-14, API-17), and `http::app`, which wraps them in the request pipeline.
- **Errors:** `Category`, and `Problem<E>`, the one conversion of an error into a response (ERR-07).
- **Lifecycle:** `Readiness`, `ShutdownSignals` and `serve`, which runs the server until a shutdown signal and then shuts it down (OPS-05, OPS-06), and `probe`, which the `health` subcommand runs (PLATFORM-044).
- **Proof types:** `Principal`, `Authorized<P>`, `Memberships` and `JobContext`, and `AuditScope`, the log and the actor of an audit entry, which only a proof builds with an actor ([audit spec](audit.md#recording-an-entry)). `Authorize<P>`, the extractor of a workspace route, proves nothing until its `open` (Design: Unit of work).
- **Traits that modules implement** (ARCH-13):
  - `SessionVerifier`, implemented by `auth`.
  - `AccessPolicy`, implemented by `access`, with `authorize`, and from step 2.4 on `memberships`.
  - `SecurityEventSink`, implemented by `audit`.
  - The personal data contract.
- **Traits that modules use:** `Permission`, `ErrorCode`, and the job handler trait.
- **Calls:**
  - `authz::refuse(uow, &Authorized<P>)`: for a refusal inside an operation, rolls its unit of work back, records the refusal as a security event, and gives the 403 `request.permission_denied` (PLATFORM-025).
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
| PLATFORM-012 | A job runs with the scope it was enqueued under, the user or workspace of the proof its unit of work acted for, and its handler's queries filter by them. It doesn't ask `AccessPolicy` again. | DATA-22 | `platform_012_` |
| PLATFORM-013 | A failing job retries with backoff, up to its kind's most attempts, then moves to the dead-letter state; a failure its handler calls permanent moves it there at once. A succeeded job's row is deleted, so it keeps no payload. A dead letter keeps its payload for `JOB_DEAD_LETTER_RETENTION_DAYS` (1 to 365, default 14), then the sweep deletes it. | OPS-10 | `platform_013_` |
| PLATFORM-014 | A periodic job runs once per interval across all replicas, and a long run keeps its lease. | OPS-11 | `platform_014_` |
| PLATFORM-061 | A committed job runs within about a second while a replica has room, each replica running at most 4 at once. A replica claims only the kinds its registry holds, so during a deploy a kind that only the new release knows waits for its replicas. | D13, D20 | `platform_061_` |
| PLATFORM-062 | A run that passes its kind's time limit is cancelled, and counts as a failed attempt. A job whose replica stopped without ending or releasing it runs again once its lease runs out, and the stale run's outcome changes nothing; when the stopped run was its last attempt, the attempt counts as failed, and the job moves to the dead-letter state without running again. | D13, OPS-10 | `platform_062_` |
| PLATFORM-063 | On a shutdown signal, the dispatcher and the scheduler claim nothing more. Runs in flight go on until the drain deadline; each one still running then is cancelled and released, without counting the attempt, so another replica runs it at once. | OPS-06, D13 | `platform_063_` |
| PLATFORM-064 | Every hour, periodic jobs delete the rate-limit buckets whose window started more than a day ago, the idempotency records older than 24 hours, and the dead letters older than their retention. | DATA-28, OPS-10 | `platform_064_` |
| PLATFORM-065 | A job's run has a span of its own, linked to the trace context stored when it was enqueued, and is counted in `platform.job_runs` by kind and outcome. | OBS-07, OBS-11 | `platform_065_` |

### Events and streams

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-015 | A published event exists only if its unit of work commits. | DATA-27, D1 | `platform_015_` |
| PLATFORM-057 | Each replica's hub loads a published event once. | D20 | `platform_057_` |
| PLATFORM-016 | A workspace event reaches only streams whose principal holds the permission its type requires. Replay excludes events from before the membership. | SEC-17 | `platform_016_` |
| PLATFORM-017 | A committed access change closes or re-filters the affected streams. A missed signal is caught by the periodic re-check. | SEC-06, D35 | `platform_017_` |

### HTTP

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-018 | A request with the key and the fingerprint of an operation that committed answers that operation's result again, and changes nothing. The same key with another fingerprint answers 422 `request.idempotency_key_reused`. A request whose key's first request hasn't committed or rolled back answers 409 `request.idempotency_key_in_flight`. A committed operation always has its record, and one that rolled back has none, so its retry runs. A record older than 24 hours counts as absent. | API-12 | `platform_018_` |
| PLATFORM-058 | An `Idempotency-Key` that isn't one string of structured fields, of 1 to 255 characters, answers 400 `request.invalid_idempotency_key`. Keys are per user: another user's request with the same key runs as a new one. | API-12 | `platform_058_` |
| PLATFORM-019 | The health probes bypass rate limits, load shedding and the route groups' body limit, and return a plain status body. | OPS-05 | `platform_019_` |
| PLATFORM-020 | Requests beyond the concurrency limit get the 503 problem `request.overloaded`, with `Retry-After`. | OPS-07 | `platform_020_` |
| PLATFORM-021 | Every error is an RFC 9457 problem with `code` and `trace_id`, a path that doesn't exist included, and internal errors expose nothing. | ERR-07, ERR-08, API-07 | `platform_021_` |
| PLATFORM-022 | Every response except the health probes carries its trace context in `server-timing`. A request with a valid `traceparent` continues that trace. | API-16, OBS-07 | `platform_022_` |
| PLATFORM-031 | A request that runs past its timeout answers 503 `request.timed_out`, with `Retry-After`. | OPS-07, ERR-03 | `platform_031_` |
| PLATFORM-032 | A request to a path that doesn't serve its method answers 405 `request.method_not_allowed`, with `Allow` listing the methods the path serves. | ERR-03, API-07 | `platform_032_` |
| PLATFORM-034 | Every request except the health probes writes one `INFO` line when it completes, with its method, route, status and duration. A 5xx response also writes one `ERROR` line with its cause chain, except a panic's, whose one `ERROR` line the panic hook writes, and a 4xx response writes nothing above `INFO`. | OBS-03, ERR-10, ERR-12 | `platform_034_` |
| PLATFORM-035 | A query parameter that the route's documentation doesn't list answers 400 `request.invalid_query`, before the handler reads any value; with none, each parameter whose value the route refuses does. Either way `errors` names each such parameter: its JSON Pointer among the query's parameters, such as `/limit`, and the code `unknown`, or the code of its value's fault, such as `out_of_range`. | API-09, API-07 | `platform_035_` |
| PLATFORM-059 | A list's `limit` is 50 when absent, and 1 to 100. Its `cursor` is one that the same route answered, or it is refused. Paging through a list, rows that share a sort key included, returns each row once. | API-06, DATA-23 | `platform_059_` |
| PLATFORM-060 | A list's `search` is trimmed, has internal whitespace collapsed to single spaces, is normalized to Unicode Normalization Form C (NFC), and is 1 to 100 characters. An item matches when one of the fields its route names contains it, ignoring case in every script, so `å` matches `Å`, whatever locale the database was created with, and with `%`, `_` and `\` matching only themselves. | API-20 | `platform_060_` |
| PLATFORM-036 | A request whose `Content-Length` exceeds its route's body limit answers 413 `request.payload_too_large`, before the body is read. | API-08 | `platform_036_` |
| PLATFORM-054 | A JSON body whose `Content-Type` isn't `application/json`, that isn't JSON, or that doesn't match its request type, an unknown field included, answers 400 `request.invalid_body`. When the parser names the field it failed at, `errors` holds that field. | API-08, API-07 | `platform_054_` |
| PLATFORM-055 | A body that grows past its route's body limit while it is read, without a `Content-Length` that declared it, answers 413 `request.payload_too_large`. | API-08 | `platform_055_` |
| PLATFORM-056 | A request whose fields their domain types refuse answers 400 `request.invalid_fields`, with every refused field in `errors`: its JSON Pointer, a code that names the domain type's reason, such as `too_long`, and a message. | API-10, API-07 | `platform_056_` |

### Client IP, rate limits and browsers

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| PLATFORM-047 | A request's client IP is its connection's peer, unless the peer is a trusted proxy (`TRUSTED_PROXIES`): then it is the rightmost address in `X-Forwarded-For` that isn't a trusted proxy. When the walk from the right reaches the header's start, or a value that isn't an IP address, before such an address, the client IP is the last trusted proxy it passed. An IPv4 address mapped into IPv6, such as the peer of a listener on `[::]`, is taken as its IPv4 address. | SEC-12 | `platform_047_` |
| PLATFORM-048 | Every request except the health probes and a listed origin's preflight (PLATFORM-052) counts in the bucket of its client IP before authentication, an IPv6 client's by its /64 prefix, which one subscriber holds whole, and every authenticated request in its user's bucket after it. A request that finds its bucket full answers 429 `request.rate_limited`, and nothing after the check runs. The two limits and their window are settings. | SEC-12, D11 | `platform_048_` |
| PLATFORM-049 | A bucket counts the requests of a window that starts with the first request after the last window ended, by the database's clock. A refused request counts too. | SEC-12, DATA-03 | `platform_049_` |
| PLATFORM-050 | A response other than a 5xx to a request that buckets counted carries `RateLimit` and `RateLimit-Policy` for the bucket with the fewest requests left, which for a 429 is the one that refused it, named by the bucket's kind: `ip`, `user`, or the name of a per-action limit, such as `sign_in`. A 429 also carries `Retry-After`, the whole seconds until that bucket's window ends, rounded up. | API-15 | `platform_050_` |
| PLATFORM-051 | A bucket's key is an HMAC, under `HMAC_KEY`, of its kind and of what it counts, so no bucket holds an IP address, an email or a user ID. | D7, DATA-28 | `platform_051_` |
| PLATFORM-052 | A request whose `Origin` is one that `CORS_ALLOWED_ORIGINS` lists gets the headers of cross-origin resource sharing (CORS) that let a browser read the response and send its cookies, and a preflight from it is answered before the IP limit. A request from any other origin gets no CORS header. | SEC-19, D61 | `platform_052_` |
| PLATFORM-053 | A request whose method isn't safe answers 403 `request.origin_not_allowed` when its `Origin` is one that `CORS_ALLOWED_ORIGINS` doesn't list, or when a session cookie authenticates it and it carries no `Origin`. | SEC-19, D61 | `platform_053_` |

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
| PLATFORM-046 | On Linux, `serve` records its process's resident memory, in bytes, in `process.memory.usage`, when it starts and every 15 seconds after. | OBS-14, D19 | `platform_046_` |

## Design

### Database roles

`bootstrap` connects with `DATABASE_ADMIN_URL`, as an administrator such as the bundled Postgres superuser or a managed provider's admin role. The three database URLs must name the same database.
- It creates the roles `owner` and `app` with `LOGIN` and no other attribute. A role that already exists is given `LOGIN` and keeps its other attributes, because changing some of them takes a superuser, which a managed provider's admin role isn't. PLATFORM-009 refuses an `app` role whose attributes are unsafe.
- It sets each role's password from its URL. The password travels as a verifier, the stored form of a password in SCRAM-SHA-256 (Salted Challenge Response Authentication Mechanism with SHA-256). The API computes the verifier on a blocking thread (CODE-05), so the clear text never reaches the server, where statement logs could record it. The administrator connection logs none of its statements, because they carry the verifiers (OBS-04).
- It sets the `app` role's `statement_timeout` to 30 seconds and its `idle_in_transaction_session_timeout` to 15 seconds (DATA-19), and its `default_transaction_isolation` to `read committed` (DATA-25). Set on the role, these hold whatever the server or the database defaults to.
- On the database, it revokes `TEMPORARY` from `PUBLIC`, grants `CONNECT` to both roles, revokes `CREATE` on the schema `public` from `PUBLIC`, and grants it to `owner` only. A database created by a Postgres older than 15 grants `PUBLIC` that `CREATE`, which would let `app` create tables.

The first migration grants `app` `SELECT`, `INSERT`, `UPDATE` and `DELETE` on every table `owner` creates, revokes `EXECUTE` on new functions from `PUBLIC`, and lets `app` read the migrations table. Roles belong to the whole Postgres server, so every database on a server shares them, test databases included. Two deployments whose databases share a server would share the roles: one's `bootstrap` would set their passwords to its own, and its URLs would reach the other's tables. So a deployment's Postgres server holds no other deployment's database, as [deploy.md](../operations/deploy.md#choosing-the-services) requires of a managed one and of [another platform's](../operations/deploy.md#on-another-platform).

Two `bootstrap` runs at once can't both be relied on: they update the same roles, and one can fail with "tuple concurrently updated", since nothing serializes them: `bootstrap` takes no lock, and an advisory lock would serialize only the runs on one database, while roles belong to the whole server. The reference deployment runs `bootstrap` once per deploy, and one deploy at a time (OPS-16), so its runs never overlap. The tests' runs on one server take turns at a lock of the test support, and each managed deployment of the end-to-end tests has a server of its own ([ci.md](../operations/ci.md)).

### Migrations

`migrations/` holds one file per change, named `<timestamp>_<module>_<description>.sql` (DATA-09), and the `app` crate embeds them as `MIGRATIONS`. `migrate` takes an advisory lock on the database, compares every applied migration with its file before it applies anything, then applies each missing migration in its own transaction together with its record. It never applies a down migration, and it refuses a migration marked `-- no-transaction`, which would run outside a transaction, so a failure part way through it would leave a trace (PLATFORM-028).

### Connections

`Database::connect` opens a pool of up to `DATABASE_MAX_CONNECTIONS` connections as `app` ([`.env.example`](../../.env.example) gives its default). An operation waits up to 5 seconds for a free connection. The startup checks run on the first connection: the role (PLATFORM-009), then the schema (PLATFORM-030). The role check covers objects the role owns in every database of the server.

A database URL is the only source of its connection's settings (OPS-01). It sets the user, password, host and database itself, and the port too, or the port is 5432. It may set only the query parameters that `.env.example` lists for `DATABASE_URL`. The Postgres driver also reads libpq's environment variables, so the ones that `.env.example` lists, which could change a connection the URL describes, must be unset, even to an empty value. Every subcommand that reads a database URL refuses to start while one is set.

The certificates a verified connection trusts are the one input the URL doesn't decide alone. With `sslmode` set to `verify-ca` or `verify-full`, a connection trusts the certificates in the URL's `sslrootcert`, if any, together with the system's: the operating system's certificate store, or instead the files that `SSL_CERT_FILE` and `SSL_CERT_DIR` name when either is set. The driver adds `sslrootcert` to the system's certificates instead of replacing them. Those two variables are how an operator chooses the certificates a container trusts, such as in an image with no certificate store, so the API leaves them to the TLS library.

The driver reads the system's certificates each time it opens a verified connection: a short blocking read on the async runtime, which CODE-05 allows here. It happens when the pool, or the readiness check of PLATFORM-037, opens a connection, not on every query. Certificates compiled into the binary would avoid the read, but the list the driver offers for that, `webpki-roots`, is licensed CDLA-Permissive-2.0, which CODE-12 doesn't allow.

Pools, connections and transactions stay inside `platform::db`, and code elsewhere reaches the database through a unit of work (DATA-21). The test support is the one other place that names them (TEST-06): it creates and drops test databases, arranges tests with statements no unit of work may run, and reads stored state as `owner` or as the administrator. It gets its connections from `platform::db::testing`, which only the `test-support` feature compiles and only the tests enable, so the API never hands out a connection, or the connection options whose `Debug` output includes the password (SEC-01).

### Unit of work

A unit of work is one transaction on the `app` pool. It opens after authentication, the rate-limit checks and parsing, so no transaction waits on a slow client, and no request holds a pooled connection while it waits for a second one:
- On a workspace route, it opens together with `Authorized<P>`. The handler extracts `Authorize<P>`, which holds the caller's `Principal` and the workspace ID from the path, and calls its `open` once the body is parsed, since axum runs the body's extractor after every other: the transaction opens, `AccessPolicy::authorize` runs inside it, and `open` returns the open unit of work with the proof, which the handler hands the service, or the refusal (PLATFORM-024). `authorize` reads the caller's membership `FOR SHARE`, so a removal or a role change of that membership waits until the operation commits or rolls back, and the operation runs under the role it was authorized with. An operation that changes another member's row changes it only where the caller outranks that member, in the statement that changes it, so a row it may not change is skipped without being locked (access spec, "Removing a member"). No two operations then wait on each other's rows, since no two members each outrank the other (SEC-08, DATA-25).
- On a user or public route, the service opens it from the `Database`, with the request's `Client`, once any per-action limit it checks has counted.
- A job handler opens it with `for_job`, from the job's `JobContext`.

The service performs the writes and commits. A unit of work dropped without a commit rolls back, and nothing retries it. Every query in it filters by tenant itself (DATA-22). The unit of work sets no database context, because there is no row-level security (D4). A unit of work that opens for a request holds the request's `Client`, whose client IP and user agent the audit entries it records keep (AUDIT-003); a job's holds none.

### Request authentication

`Principal` reads the access token from the `Authorization` header when the request has one, which must hold `Bearer` and a token, and otherwise from the cookie `__Host-access_token` (AUTH-015). A request that the cookie authenticates and whose method isn't safe also passes the cross-site check (PLATFORM-053) before it is verified.

`SessionVerifier` hashes the presented access token with SHA-256 (SEC-03), and reads the session through the unique index on `sessions.access_token_hash`. It checks the expiry in SQL against `now()`. It returns a `Principal` holding the user's ID and the session's ID. A missing or unknown token answers 401 `request.unauthenticated`, and an expired one 401 `request.access_token_expired`. The request is then counted in its user's bucket (PLATFORM-048).

### Client IP

Each request's `Client` holds its client IP and its user agent. The client IP is the connection's peer, unless the peer is listed in `TRUSTED_PROXIES`, as addresses or ranges (PLATFORM-047). A proxy appends to `X-Forwarded-For` the address it received the request from, so the addresses on the right were written by trusted proxies, and the first untrusted one, read from the right, is the client that the outermost trusted proxy saw. An address further left could have been written by anyone. The reference deployment lists the private address ranges, since only its own containers reach a replica, on its private networks, and the addresses its proxy trusts in turn, such as a content delivery network's, which `TRUSTED_PROXIES` in [settings.env.example](../../deploy/settings.env.example) gives both.

### Refusals

`AccessPolicy::authorize` answers one of three things:
- not a member, which becomes 404 `request.not_found`
- a member without the permission, which becomes 403 `request.permission_denied`
- the member's role, from which `Authorized<P>` is built

A path whose workspace ID isn't a UUID names no workspace: it answers 404 `request.not_found`, as a path that no route serves does, and nothing records or counts it. A refusal is reported to the `SecurityEventSink`, which records it in its own short transaction, because the refused operation has written nothing. A member's refusal goes to the workspace's log, and a non-member's to the caller's own log. A rank refusal found inside an operation goes through `authz::refuse`: the operation's unit of work rolls back, and the refusal is recorded the same way (PLATFORM-025). A refusal names the caller, the workspace, the permission, whether the caller is a member, and the request's `Client`, and the sink records it with the trace ID of the span it is reported in. A sink that fails to record fails the request as an internal error, so no refusal goes unrecorded (DATA-27).

`Permission` is the trait of a permission type, which gives its name, such as `members.read`. The builder `workspace::<P>` registers a route whose handler extracts `Authorize<P>`, and documents the operation's access as `workspace` and its permission as `x-permission`, which TEST-09's generated test reads. A route of `superadmins` (D62) will be authorized the same way, by a proof of its own: a builder `superadmin` whose handler extracts that proof, which a trait of its own, implemented by `superadmins`, checks inside the unit of work, and whose refusals reach the same sink. Neither `Authorized<P>` nor any workspace route changes for it.

### Rate limits

Three kinds of bucket count requests:
- the IP bucket of every request but the health probes and a listed origin's preflight, which the pipeline counts before authentication
- the user bucket of every authenticated request, which `Principal` counts once it has verified the session
- the bucket of a per-action limit, which a module counts through `RateLimits::check`, such as the sign-in email's (AUTH-006)

A check counts the request before the operation's unit of work opens, in one statement on a pooled connection, outside any transaction, so the count commits at once and survives whatever happens next (PLATFORM-026). The statement inserts the bucket, adds one to its count, or starts a new window when the last one has ended, and returns the count and the window's end, all by the database's clock (PLATFORM-049). A bucket's key is an HMAC-SHA-256, under `HMAC_KEY`, of its kind and of what it counts, so the table holds no IP address, email or user ID (PLATFORM-051). A check is one call that counts and decides in one statement, so a fork could move the counts to a shared store, such as Valkey, by changing the rate limiter alone ([architecture §1](../architecture.md#1-system-context)). The IP and user limits count in Postgres too, as every limit does, so each holds across the fleet whatever replica a request reaches (D20), at the cost of a write on every request, which `just load-test` measures (OPS-18). Counting them in each replica's memory instead would make each limit one per replica (SEC-12).

A request that finds its bucket full stops there with 429 `request.rate_limited`. A check that fails, as when the database doesn't answer, fails the request as an internal error: the request couldn't have been served without the database anyway. The pipeline keeps each bucket that counted a request, and writes on the response, whatever answered it, other than a 5xx, the headers of the one that decided it, with `Retry-After` on a 429 (PLATFORM-050). The rows of ended windows stay until the hourly sweep deletes them (PLATFORM-064); a check that finds one starts a new window, so they change no decision.

### Jobs

**Enqueueing.** A module declares each queued job in its `jobs.rs` (OPS-09), as a payload type that implements `Job`, which names its kind, `<module>.<name>`, its most attempts, 10 unless it names another number, and its time limit, 5 minutes unless it names another. `uow.enqueue(scope, &job)` inserts a `jobs` row in the unit of work's transaction (PLATFORM-011), with the payload as JSON, the scope, and the trace context of the current span (OBS-07). The scope comes from the proof the unit of work acts under, the only place one is built: `Authorized<P>` gives its workspace and member, `Principal` its user, and a `JobContext` the scope of the job that enqueues; scheduled maintenance has none. A payload type changes only by adding fields that may be absent, as a migration expands a table, so a replica of the previous release still runs a job the new release enqueued; a change that can't be made that way is a new kind.

**The dispatcher.** Every replica of `serve` runs one, so jobs spread over the replicas, and no setting turns them off (D13). It claims runnable jobs, waiting ones whose `run_at` has passed, of the kinds its registry holds (PLATFORM-061), with `FOR UPDATE SKIP LOCKED`, in one statement outside any transaction that gives each a lease of 60 seconds and a new claim ID, adds one to its attempts, and commits at once. It runs at most 4 jobs at a time, claims again as soon as one ends, and polls once a second while it finds none, so a job runs within about a second of its commit. It polls rather than waiting on the listener: a notification would save at most that second, and a replica would still poll for what it missed while its listener reconnected.

**A run.** A run happens outside any transaction (DATA-26). The handler receives the payload and a `JobContext`, the job's ID, attempt and scope, and opens a unit of work with `for_job` when it needs one. While it runs, the dispatcher renews its lease every 20 seconds, by its claim ID, so a replica that lost its claim renews nothing. A run that passes its kind's time limit is cancelled, and fails (PLATFORM-062). When a run ends, the dispatcher writes its outcome by its claim ID, so a stale run changes nothing:
- A success deletes the row (PLATFORM-013): nothing of a job outlives it but its telemetry.
- A failure that the handler calls permanent, or the failure of the last attempt, moves the job to the dead-letter state, with the failure's message.
- Any other failure keeps the failure's message and sets `run_at` to the next attempt. The `n`th retry waits a random time between half of and all of the lesser of 10 seconds times 2 to the power `n − 1`, and an hour, so retries spread out rather than arrive together (OPS-10).

A replica that stops without releasing its claims leaves their leases to run out, and another replica then runs the jobs again: a job runs at least once, so every handler is idempotent, or its spec says why it can't be, as an email that may arrive twice ([mail spec](mail.md#design)).

**Scope.** A job runs with the scope it was enqueued under, and its handler's queries filter by it, as a request's do by its proof (PLATFORM-012, DATA-22). It doesn't ask `AccessPolicy` again: a job carries out what an authorized operation committed, such as sending the email of an invitation that exists, which a later change to its actor's role doesn't undo. A handler whose work depends on access as it stands when it runs reads that in its own unit of work.

**Periodic jobs.** The registry declares each with its name, its interval and its time limit, and `serve` inserts its `job_schedules` row, due at once, unless it exists. Every 10 seconds, each replica's scheduler claims the due rows of the names its registry holds with `FOR UPDATE SKIP LOCKED`, giving each a lease and a claim ID, runs the job, renews its lease as the dispatcher does, and then sets its next run one interval after this run began (OPS-11). A failed run waits for its next interval, rather than retrying sooner. A row whose name no registry holds, left by a job that a release removed, is never claimed.

**Sweeps.** `platform` registers three periodic jobs, each hourly, each deleting in batches of at most 1,000 rows, in a transaction each (PLATFORM-064):
- `platform.expire_rate_limit_buckets`: the buckets whose window started more than a day ago, the longest window any limit allows, so none it deletes still counts.
- `platform.expire_idempotency_records`: the records older than 24 hours, which `claim` already treats as absent.
- `platform.expire_dead_letters`: the dead letters older than `JOB_DEAD_LETTER_RETENTION_DAYS`.

Scheduled maintenance isn't audited (DATA-27).

**Shutdown.** On a shutdown signal, the dispatcher and the scheduler claim nothing more, while runs in flight go on as the HTTP server drains. Both share one deadline, the requests' own: the shutdown delay and the drain after the signal. At it, each run still going is cancelled, and its claim released, its attempt uncounted, so another replica runs it at once (PLATFORM-063).

**Alerts** (OBS-14). `JobsDelayed` pages when `platform.job_delay` stays above 10 minutes for 5: every email waits, and from step 5 every sign-up's code with them. `JobsDeadLettered`, a ticket, fires on any dead letter; its runbook lists the dead letters with their failures. A dead letter keeps its payload for investigating it (OPS-10), which the runbook's query leaves out unless the investigation needs it, since it may hold personal data. Nothing retries a dead letter, since each job's remedy is the operation that caused it, such as inviting again. `PeriodicJobFailing`, a ticket, fires when a periodic job fails twice within 3 hours.

### Mail

`ServerConfig` holds the email settings as a `MailConfig`, which `serve` reads with its own: their meanings and their checks are the [mail spec](mail.md#settings)'s (MAIL-006). With email on, the composition root builds one `Transport`: `lettre`'s SMTP transport on Tokio, with a pool of connections, whose TLS is `rustls` with its `ring` provider (SEC-14), verifying the server's certificate against the system's certificates, as a verified database connection does (D38). Building it with TLS reads those certificates' files through `rustls-native-certs`, a blocking read, once, while `serve` starts and before it accepts a request (CODE-05).

Each send runs in a client span of its own, `smtp send`, with the server's address and port (OBS-06). Its failure holds the server's reply code, or the kind of the failure, such as a refused connection or a timeout, and never the reply's text, which may quote the recipient's address, so neither the logs nor a dead letter hold more than the job's payload already does (OBS-04, ERR-04).

### Events and the real-time hub

- Each module declares its event types in its `events.rs`, each with its name (API-19), its scope, and for a workspace event the permission it requires, as it declares its audit actions.
- `uow.publish_to_user` and `uow.publish_to_workspace` insert an `events` row and call `pg_notify` with the event's ID, both in the unit of work's transaction. The hub, which delivers what is published, arrives in step 2.4. The row holds the event's scope (a user or a workspace), its type, the permission its type requires, its payload of IDs, and its trace context (OBS-07). Postgres delivers notifications only at commit, and no trigger is involved (DATA-07).
- Each replica's listener receives the notification. The hub loads the event once, and delivers it to each stream whose scope matches and whose principal holds the required permission. The hub caches each stream's `Memberships` from `AccessPolicy::memberships`, and refreshes them on access-change signals.
- On reconnect, a stream replays from its last event ID. The replay query filters on the caller's user ID and on the workspace IDs in their `Memberships`, and skips events created before the membership in their workspace (SEC-17).
- An access change sends a signal on the `access_changed` channel with `pg_notify` inside the change's transaction. The signal holds only IDs: the kind of change, the user, and where relevant the workspace or session. It isn't stored (D35). The hub closes the affected streams, or re-filters them against a fresh `AccessPolicy::memberships`.
- Every configured interval (60 seconds by default), the hub re-checks each open stream's session through `SessionVerifier` and its memberships through `AccessPolicy`, and closes or re-filters it on any change (SEC-06).

### Idempotency

The key's record is written by the operation's own unit of work, so it exists exactly when the operation committed, and no other state needs cleaning up:
1. `Idempotent<T>` reads the header, a string of structured fields as draft-ietf-httpapi-idempotency-key-header-08 defines it (RFC 9651), and refuses one that isn't (PLATFORM-058). The fingerprint is a SHA-256 digest of the method, the path and the query, each after its length, so no two requests' parts run together the same, then the digest of the body it read, whose length is fixed.
2. The service opens its unit of work, and its first statement is `claim`, with the caller's user ID, the key and the fingerprint. It inserts the record, which the unit of work completes at its commit. A committed record of the same user and key decides instead: its result when the fingerprint matches, which the service returns as its own, so the route maps it as it maps a fresh one, or 422 otherwise.
3. A record that another unit of work has inserted and not yet committed makes the insert wait on that transaction. `claim` waits at most one second, under a `lock_timeout` of its own, which bounds that wait as it bounds a lock's, and then answers 409, so a retry of a request still running is refused rather than held.
4. `commit_with(result)` writes the result into the record and commits, in the same transaction as the operation's writes. A unit of work that claimed a key and commits without its result fails as an internal error, so no keyed operation commits without one.
5. A unit of work that rolls back, whether it failed or the process ended, takes its record with it, so a retry runs again.
6. A record older than 24 hours counts as absent: `claim` replaces it. The hourly sweep deletes such records (PLATFORM-064).

A key is compared exactly as sent, once the structured field's own syntax is read, its quotes, its escapes and the spaces around it, with no trimming inside it and no folding of case (D12). It is the caller's own, so a retry from another session of the same user, after signing in again, still finds its record.

### Lists

`Page` reads `limit` and `cursor` (PLATFORM-059), both before it refuses either, so one response names both. A cursor is the base64url encoding of the first bytes of its route's SHA-256 digest, and the last item's sort key and ID, so a page is the next rows after that key in the route's order, whatever was added since (API-06). A cursor that doesn't decode into the route's key is refused, as `/cursor` with the code `invalid`. `Search` reads `search` (PLATFORM-060), and a repository matches it with `ILIKE`, its pattern escaped, under Postgres's built-in collation `pg_unicode_fast`, whose case mapping is Unicode's in every database, where the database's own locale may fold only the letters `A` to `Z`. Every list is first narrowed to one workspace or one user, so the match reads only that tenant's rows, and needs no index of its own until a load test shows one (OPS-18). The values that a list's route refuses are reported together, once its route group has found no name its documentation doesn't list (PLATFORM-035).

A list whose order and whose searched fields belong to different modules, such as members, whose order is their memberships' and whose names are their accounts', takes the same steps everywhere (ARCH-06). Without a search, the module that owns the order pages its rows through its index (DATA-23), and the other module gives the page's names in one batch. With one, the module that owns the fields first returns which of the tenant's IDs match, and the module that owns the order pages among those.

### Request pipeline

The order of the middleware stack is in [architecture.md §6](../architecture.md#6-request-lifecycle). The health probes are answered before the stack (OPS-05), and the security headers (SEC-11) wrap both.

**Routes and their documentation.** A module's `routes()` returns `Routes`, which only the builders of API-14 add to, and which takes the module's state, such as its service, only in the composition root, so the routes document themselves without a database. A builder takes a route's documentation with its method router, as utoipa-axum's `routes!` makes both from the handler's `#[utoipa::path]`, and the errors of the module that the route can answer. A route serves `HEAD` wherever it serves `GET`, as HTTP asks. To each operation, the builder adds:
- its access level, as the extension `x-access`
- the problems the pipeline can answer on it, and their codes and titles (ERR-13). Every operation lists `request.invalid_query`, `request.payload_too_large`, `request.rate_limited`, whose 429 carries `Retry-After`, `request.timed_out` and `request.overloaded`, whose 503 carries `Retry-After`, and `request.internal_error`; and `request.unavailable`, a 503 with `Retry-After` too, which only the proxy in front of the replicas answers, while none of them can (OPS-05). One whose method isn't safe also lists `request.origin_not_allowed` (PLATFORM-053), one that authenticates its caller `request.unauthenticated` and `request.access_token_expired` (AUTH-009), and one that takes a body `request.invalid_body` and `request.invalid_fields` (PLATFORM-054, PLATFORM-056).
- the module's errors that the route can answer, which the builder documents the same way
- `RateLimit` and `RateLimit-Policy` on each of its responses but the 5xx, which a request that load shedding refused, or that panicked, never carries (PLATFORM-050)

`check-openapi` fails an operation that lacks a `request.` code that another operation of its group lists: one of its access level, whose method is as safe as its own, and that takes a body as it does (ERR-13). clippy bans the routing methods of axum and utoipa-axum outside `platform::http`, so a route reaches the router only through a builder, which registers it at the paths its documentation names. A test sends every documented path each method OpenAPI can document that the path's documentation leaves out, and a method no route serves, and expects each to answer 405 with an `Allow` that lists exactly the documented methods. A method router whose fallback serves one of those methods fails it, so a route serves none of the methods OpenAPI can document that its documentation leaves out, however its method router was built (API-14, API-17). `http::app` mounts every module's routes, the fallbacks and the stack. `Routes::openapi` returns the routes' OpenAPI document, which covers the `/v1` routes and holds the router's own problems among its components: `NotFound`, and `MethodNotAllowed` with `Allow`. The binary's `openapi` subcommand writes it to `openapi.json`.

**Checking the document.** A response object in the document stays open to properties it doesn't list, because clients ignore the fields they don't recognize (API-05) and `/v1` may add response fields (API-18). The integration tests send plain HTTP requests to the API in process, and the test support checks every response against the document of the routes that answered it, with every object schema among the document's components closed, except the parts of an `allOf`, which each list only some of the properties of the whole. A field, status or content type the document doesn't list fails the test, and so does a header it lists that the response lacks (API-17). A `HEAD` response is checked against its `GET`, the only operation the document lists for both, since HTTP defines `HEAD` by `GET`. A request that no operation documents must get `NotFound` or `MethodNotAllowed`, except at the health probes, which the document leaves out and the check skips (OPS-05), and a CORS preflight, which the CORS layer answers before any route (PLATFORM-052). The tests of `serve`'s shutdown and of the binary's log lines read their responses over a connection of their own, which the check doesn't see. A client generated from the document couldn't send the malformed requests that the pipeline's behaviors need.

**Errors.** `ErrorCode` gives an error its code, its title and its category (ERR-02, ERR-03). A handler returns `Result<T, Problem<E>>`, and `?` turns any `E: ErrorCode` into a `Problem<E>`, whose response is the problem of API-07, with the trace ID of the request's span. A 5xx `Problem` also carries its cause chain in a response extension, which the request log writes and removes (ERR-10). The fallbacks answer a path that doesn't exist with 404 `request.not_found`, and a method the path doesn't serve with 405 `request.method_not_allowed`, to which axum adds `Allow` (PLATFORM-032).

**Trace context.** The tracer is OpenTelemetry's SDK, which exports what [Telemetry export](#telemetry-export) says. It samples every request, whatever the flags of an inbound `traceparent` say, so every trace ID a response carries can lead to a trace (OBS-08). The outermost layer of the stack opens the request's server span, as the child of a valid inbound `traceparent`, and writes `server-timing: trace;desc=<traceparent>` with the span's own trace context, because the tracing libraries don't write that header (PLATFORM-022). A 5xx response sets the span's status to error and its `error.type` to the problem's code, or to `_OTHER` when the request log knows no error for it, and a 4xx leaves both unset, as OpenTelemetry's conventions for HTTP servers ask. A span is named after its method and route, such as `GET /v1/capabilities`, and a span whose method is `_OTHER` after the protocol instead, such as `HTTP /v1/capabilities`. The span, the request's log line and its metric give the method as one of the methods HTTP's specifications define (RFC 9110's, `PATCH` and `QUERY`), or as `_OTHER`, so a client can't add values without bound (PLATFORM-040, OBS-11). The API serves no other method, so no method it serves becomes `_OTHER`.

**Limits.** Each route group's timeout and body limit are constants (OPS-07, API-08). The `public`, `user` and `workspace` groups share a timeout of 10 seconds, twice the 5 seconds an operation waits for a pooled connection, and a body limit of 64 KiB, far above the largest JSON body the first slice accepts. A module spec may set others for its routes. The body limit refuses a declared `Content-Length` over it before the body is read, and stops any other body at the limit while it is read; `Json<T>` turns that into the 413 problem (PLATFORM-055). The concurrency limit is a setting, because a deployment's capacity decides it (OPS-02).

**Query parameters.** A route accepts the query parameters its documentation lists, which come from the type its handler parses them into. That type declares `#[into_params(parameter_in = Query)]`, without which utoipa leaves its fields out of the query, and each of its fields is an `Option<String>`, which the route reads into domain types, so every value it refuses is named in one response (`Query<T>`). The builder answers any other name with 400 `request.invalid_query`, before the handler runs, so a route without query parameters refuses any query string (PLATFORM-035).

**Request bodies.** `Json<T>` reads a JSON body. It requires `Content-Type: application/json`, with or without a `charset` of `utf-8`, reads at most the route group's body limit (PLATFORM-055), and parses the body into `T`, whose `#[serde(deny_unknown_fields)]` refuses a field it doesn't declare (API-08). Each of those failures answers 400 `request.invalid_body`, with the JSON Pointer of the field the parser names in `errors`, when it names one (PLATFORM-054). A request type then converts into domain types through `TryFrom`, which tries every field and collects each refusal in `FieldErrors`, so one response names them all (API-10): 400 `request.invalid_fields`, whose `errors` give each field's JSON Pointer, the reason's code, and a message (PLATFORM-056).

**Browsers.** `CORS_ALLOWED_ORIGINS` lists the origins of web clients, such as `https://app.example.com`. Empty, the default, the API serves only clients that set their own headers, and sends no CORS header. With origins listed, the CORS layer, right inside the layer that reads the client and outside the concurrency limit, so that a browser can read even a 503 or a 429, answers a preflight from a listed origin itself, and adds to every response to one `Access-Control-Allow-Origin` with that origin, `Access-Control-Allow-Credentials: true` and `Vary: Origin` (PLATFORM-052). A preflight allows the methods the API serves, and the request headers `Authorization`, `Content-Type`, `Idempotency-Key`, `If-Match`, `Last-Event-ID` and `traceparent`; a response exposes `Location`, `ETag`, `RateLimit`, `RateLimit-Policy`, `Retry-After` and `server-timing`. Startup refuses `*`, a value that isn't an origin, and an origin that isn't `https`, except one whose host is a loopback address in development.

The cross-site check runs right inside the IP limit, so its refusals count and carry the rate-limit headers: a request whose method isn't safe answers 403 `request.origin_not_allowed` when its `Origin` isn't listed (PLATFORM-053). A browser sends `Origin` with every such request, and only a browser sends it, so the check refuses no client that sets its own headers. When a cookie authenticates such a request, `Principal` and the refresh route also refuse one that carries no `Origin`, so a request the cookie authenticates always comes from a listed origin. With `SameSite=Strict`, no other site's page can make a browser send the cookies at all; the check also covers a page of another origin on the same site.

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
- `platform.rate_limit_refusals`, by the rate limiter, for each request it refuses, by the kind of the bucket (PLATFORM-048)
- `platform.authorization_refusals`, by `authz`, for each refusal, by its reason: `not_member`, `permission` or `rank` (PLATFORM-025, ACCESS-003)
- `platform.idempotent_requests`, by `claim`, for each keyed request, by its outcome: `new`, `replayed`, `reused` or `in_flight` (PLATFORM-018)
- `platform.database_reachable`, 1 or 0, by the readiness check after every ping (PLATFORM-043)
- `platform.job_runs`, by the dispatcher and the scheduler, for each run, by the job's kind and the run's outcome: `succeeded`, `retried`, `dead_lettered`, `failed` for a periodic run, which waits for its next interval, or `released` at shutdown (PLATFORM-065)
- `platform.job_duration`, the seconds each run took, by the job's kind
- `platform.job_delay`, the seconds the longest-waiting runnable job has waited past its `run_at`, 0 when none waits, by each replica's dispatcher every 15 seconds, so a queue that falls behind shows before anyone misses an email
- `process.memory.usage`, the process's resident memory in bytes, every 15 seconds, from the kernel's report in `/proc/self/status`, so on Linux only (PLATFORM-046)
- the exporters' counts, above

### Readiness and shutdown

- The instance keeps one connection to the database outside the pool, and pings it every 5 seconds: a protocol message the server answers without running a query. A ping fails after 2 seconds without an answer, and a failed ping reconnects at once. The readiness probe reads the last result, so it never waits for the database or for a pooled connection (PLATFORM-037). The listener of the real-time hub uses the same connection when it is built (build order 2.4).
- `ShutdownSignals` installs the handlers for SIGTERM and SIGINT, or Ctrl+C on Windows, before `serve` connects to the database. A failure to install them fails startup (OPS-03, OPS-06).
- On a signal, `serve` turns readiness to `draining`, waits the shutdown delay so the load balancer stops sending requests, stops accepting connections, and waits for the requests in flight until the drain deadline. It then returns, and a request still running ends with the process. The jobs' runs drain beside the requests, until the same deadline (PLATFORM-063). The delay and the deadline are settings, because a deployment's health-check interval and grace period decide them (OPS-02). Then `main` writes out the logs.
- A failed or recovered check changes readiness before its log line is written, so whoever reads the line finds readiness changed.
- The `health` subcommand is the image's health check (OPS-15). It reads `LISTEN_ADDR` and no other setting, runs no async runtime, and writes nothing but its failure, so a check every few seconds adds nothing to the logs. `lifecycle::probe` connects over plain TCP and writes the request itself: the instance answers on its own address, which needs neither of the outbound clients (SEC-09). It waits 3 seconds for the connection and for each read of the answer, longer than the 2 seconds Windows takes to report a refused connection, and reads at most 4 KiB. A failure of the probe is written in the format of OPS-03, with `startup.area` `health`, because a one-shot subcommand's whole run is its startup; a missing or invalid `LISTEN_ADDR` fails as any setting does, with `config` (PLATFORM-023).

### Clock

`Clock` is a trait with two implementations (ARCH-13): the system clock, and the test clock, which only the `test-support` feature compiles. `now` reads the time, and `sleep` waits. A test clock's `sleep` completes when the test advances the clock past its deadline (TEST-07). Every wait in the workspace goes through it (CODE-06), the readiness checks and the shutdown delay included.

### Telemetry names

The list in `platform::telemetry` holds one constant per name (OBS-05). A name that a semantic convention defines is declared as `convention("http.route", otel::HTTP_ROUTE)`, a `const fn` that fails compilation when the two names differ. The conventions' constants come from `opentelemetry-semantic-conventions` 0.33.1, which publishes semantic conventions 1.42.0, with its `semconv_experimental` feature for names still in development. A deprecated constant fails clippy.

The names so far:
- From the conventions: `service.name`, `service.version`, `service.instance.id`, `deployment.environment.name`, `http.request.method`, `http.route`, `http.response.status_code`, `error.type`, `exception.message`, `code.file.path`, `code.line.number`, `server.address`, `server.port`, `db.namespace` and `user.id`, which the request's span carries once `Principal` has verified its caller (OBS-09).
- Trace context: `trace_id` and `span_id`, the top-level fields of JSON logs (OBS-05). The conventions' crate has no constant for them, so the list declares them without `convention`.
- The bridge's fields: `otel.name`, `otel.kind` and `otel.status_code`, which `tracing-opentelemetry` reads to name a span and set its kind and status, and which no JSON log line carries. `otel.status_code` comes from the conventions; the other two are the bridge's own (OBS-05).
- Own: `request.duration`, in seconds, `startup.area`, `migration.versions`, `database.connections`, `rate_limit.bucket`, `auth.outcome`, `authorization.reason`, `idempotency.outcome`, `job.kind`, `job.id`, `job.attempt`, `job.outcome`, and `workspace.id`, which the request's span carries once `Authorized<P>` has proven its caller a member (OBS-09).

A name joins the list with the first code that writes it.

`check-instrument` reads the list's names, then every `tracing` event and span macro, every `#[instrument(fields(…))]` and every `.record("…")`, including calls nested inside other macros. It fails on:
- a name the list doesn't hold, or a `{ CONST }` name that isn't one of the list's constants
- a `record` name that is neither a listed name nor one of the list's constants
- a message that isn't constant (OBS-01)
- an `#[instrument]` without `skip_all` (OBS-04)

## Open design questions

| Question | Settled by |
|---|---|
| Whether the hub refreshes a stream's cached memberships on every access-change signal for that user, or only on the signal's workspace | `platform_016_` and `platform_017_`, measured against a user in several workspaces |
| Whether one listener connection per replica is enough under load, or events and access changes need separate connections | a load test of the hub, recorded under OPS-18 |
| Whether `serve` needs a limit on the connections it accepts. Beyond the concurrency limit, a request gets 503 at once, but each open connection still holds memory: in the load test at 1.6 times a replica's capacity, with the client opening a connection for each request it couldn't send on one already open, a replica's memory reached 408 megabytes of its 512, and fell back afterwards | an overload test through Caddy, which opens the replicas' connections in a deployment, recorded under OPS-18 |
