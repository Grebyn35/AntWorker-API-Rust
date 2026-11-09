# Architecture

How the API is built: its parts, how they depend on each other, how a request flows through them, and where each rule is enforced. Read this before changing or adding a module.

While the API is being built, parts of this document describe the design to build rather than code that exists, as "Docs for unbuilt parts are the plan" in [CLAUDE.md](../CLAUDE.md) explains. The module table in [MIGRATION.md](MIGRATION.md#modules) shows what is built.

Decision IDs `D1` to `D36` refer to the decision records in [inventory/SYNTHESIS.md](inventory/SYNTHESIS.md), and later ones to the [tracker's decisions](MIGRATION.md#decisions), whose commits hold their reasons.

## 1. System context

```mermaid
flowchart LR
  client[Clients] --> caddy[Caddy<br/>TLS, load balancing,<br/>health checks]
  caddy --> api[API replicas<br/>1..N]
  api --> pg[(PostgreSQL 18)]
  %% part: files
  api --> s3[(S3-compatible<br/>object storage)]
  api --> clamd[ClamAV's daemon<br/>malware scanning]
  %% end part: files
  api --> smtp[SMTP]
  api --> oauth[OAuth providers<br/>Google, GitHub]
  api --> otel[OpenTelemetry<br/>Collector]
  otel --> prom[Prometheus] & loki[Loki] & tempo[Tempo]
  prom & loki & tempo --> grafana[Grafana]
  prom --> am[Alertmanager]
```

- PostgreSQL is the system of record and the only coordination point between replicas (OPS-08). It holds rate limits, idempotency records, job leases, the job queue and the event log. There is no Redis, Valkey or other shared store, and adding one to the template needs an architecture decision record (ADR) showing Postgres can't cope. What commits with a unit of work stays in Postgres in any fork, since a store outside the transaction can't commit with it (D1): events, jobs, audit entries and idempotency results. What doesn't, the rate-limit counts and any cache, sits behind one type of `platform` each, so a fork that moves one to a shared store changes that type and the composition root, and no module.
- Object storage holds file bytes. Uploads and downloads use it during the request, and deletions go through background jobs. <!-- part: files -->
- ClamAV's daemon scans each upload for malware during its request, before the upload is stored. <!-- part: files -->
- The Simple Mail Transfer Protocol (SMTP) delivers email, and only background jobs reach it. No request waits for an email to send (D1).
- The observability stack is always part of the codebase, and turning it on at runtime is optional.

## 2. Principles

1. **Mechanisms live in `platform`, policy lives in modules.** `platform` knows how to run a transaction, deliver a job or render an error. It does not know what a workspace member may do.
2. **One way to do each thing.** There is one transaction API, one error path, one authentication extractor, one authorization check and one outbound HTTP client. A second way needs an ADR.
3. **Enforce rules with the compiler first, then tooling, then review.** Visibility, types and crate boundaries come first. Clippy and CI scripts come second. A rule that only review can check is written so a reviewer can answer it yes or no.
4. **A side effect commits with the change that causes it** (D1).
5. **Every workspace request is authorized against the workspace named in its path** (D2, D3).
6. **Startup fails loudly; at runtime the service degrades predictably** (D9, D20).

## 3. Repository layout

```
Cargo.toml          workspace manifest
justfile            every task, run identically locally and in CI (D21), with its settings and variables
just/               the tasks the justfile imports, a file for each group that `just` lists (CODE-08)
Dockerfile          the image (OPS-13)
compose.yaml        the services a developer runs locally, and the images of the tools the tasks run
crates/
  platform/         shared mechanisms, no product knowledge
  app/              feature modules, routes, composition root, the binary
  xtask/            the custom CI checks, and the other tasks that `just` runs
migrations/         one ordered directory; each file name names its owning module
deploy/             the configuration of the observability stack and of the reference deployment
docs/
.github/            thin workflows that call `just`, and the configuration of Dependabot
.claude/            Claude Code definitions, including the conformance reviewer
CLAUDE.md
```

The configuration of the services the API runs beside lives under `deploy/`, so the root holds only what a developer edits day to day.

## 4. Crates

The API has two crates. Cargo forbids `platform` from depending on `app`, so the shared mechanisms can never come to depend on a feature. The workspace's third crate, `xtask`, holds the custom CI checks and the other tasks that `just` runs, such as `just release`, `just deploy`, `just load-test`, `just remove` and `just new-module`, whose templates are in `crates/xtask/templates/`, and neither API crate depends on it.

### `platform`

| Area | What it provides | Decisions |
|---|---|---|
| `config` | Typed configuration loaded once at startup. Values are trimmed, empty means unset, `*_FILE` secret files are supported, and every failure is reported in one format. | D9, D26 |
| `db` | One connection pool, the listener beside it, the unit of work, and the `bootstrap` and `migrate` tasks. A unit of work is one transaction, opened by the service, or with `Authorized<P>` on a workspace route, with a writer for jobs and events, and the claim of an idempotency key, whose record commits with it. Every query in it filters by tenant itself (DATA-16). | D1, D4, D12, D20, D24 |
| `jobs` | A durable job queue with retry, backoff and dead letters, which every instance's dispatcher polls, plus a scheduler that runs each periodic job once per interval across the fleet. A job records the user or workspace it was enqueued for, and its handler filters by them. | D1, D13 |
| `events` | The event log and the real-time hub. A unit of work publishes events, which take their positions in the log in the order their units of work commit. The hub listens on the listener, and loads each new event once and delivers it to the streams allowed to receive it (SEC-19). It also replays what a reconnecting stream missed, closes streams when an access-change signal arrives, and re-checks every open stream at a configured interval (SEC-06). | D1, D5, D20 |
| `rate_limit` | The buckets that count requests in windows, in the database every replica shares: the IP and user limits that the request pipeline applies, and the per-action limits a module checks, such as sign-in's. | D11 |
| `http` | The route builders and the OpenAPI document they produce, the middleware stack with trace context, timeouts, load shedding, cross-origin resource sharing (CORS) and the protection against cross-site request forgery (CSRF), extractors (JSON, a keyed route's JSON with its `Idempotency-Key`, a JSON Merge Patch, an update's `If-Match`, path, query), a path's row IDs, the response that answers a resource, with its `Location` and `ETag`, and rate limiting. | D8, D10, D11, D12, D20, D61 |
| `http::download` | The response that serves bytes that never change, such as an export's archive, or a range of them, and the extractor of what a download's request asks of them. | D126 |
| `http::upload` and `http::files` | The extractor of an upload's bytes, which a replica reads 4 at a time, and the name an upload gives. | D107 <!-- part: files --> |
| `authn` | Reads a request's access token from the Bearer header, for clients that set their own headers, such as the desktop client, or otherwise from an `HttpOnly` cookie, for browsers. It verifies the token through the `SessionVerifier` that `auth` implements, producing a `Principal`. | D18, D61 |
| `idempotency` | The `Idempotency-Key` of a keyed route, read as draft-08 of the IETF's draft defines it, with the fingerprint of its request, which a unit of work claims. | D12 |
| `lists` | A page's `limit` and cursor, a list's `search`, and the body of a page, which a route reads from its query and hands its service. | D8 |
| `authz` | The `Authorized<P>` proof and the `Memberships` type, generic over the `Permission` trait. Producing `Authorized<P>` opens the request's unit of work and asks the `AccessPolicy` that `access` implements inside it. Refusals are reported to the `SecurityEventSink` that `audit` implements. | D2, D3, D6 |
| `errors` | Error categories, the `ErrorCode` trait, and the single conversion to an HTTP response. | D17 |
| `telemetry` | The logging and tracing setup, the export of spans, logs and metrics over the OpenTelemetry Protocol (OTLP), the list of telemetry names, and the metric declarations. | D10, D19 |
| `storage` | The client of the S3-compatible bucket that holds files' bytes, which reads an object, or a range of its bytes, as a stream. | D1, D14, D106, D112 <!-- part: files --> |
| `signed_urls` | The signed URLs of downloads: a path, the session it acts for and when it stops working, signed with a hash-based message authentication code (HMAC) under `HMAC_KEY`, which a client that can't send a credential with a request, such as an `<img>`, sends in its place. | D61, D113 <!-- part: files --> |
| `scanner` | The client of ClamAV's daemon, which scans an upload's bytes for malware. | D26, D63, D107 <!-- part: files --> |
| `mail` | Sending email: `Mail` enqueues a module's rendered email in a unit of work, and the job `platform.send_email` sends it through the SMTP transport, which the email settings configure, with a client span for each send. Each module's emails, and their templates, live in the module (§5). | D1, D9 |
| `outbound` | The only HTTP clients: one for URLs from configuration, and one for URLs supplied by third parties, with protection against server-side request forgery (SSRF) (SEC-10). | D26 |
| `types` | Shared domain types: `Secret`, `EmailAddress`, `Timestamp`, a row's `Version`, the ID types, and `one_line`, the normalization of one line of text a user writes, which a module's domain types of such text, such as a name, build on. | D7, D25 |
| `clock`, `random` | The injectable clock and the operating system's secure random source (CODE-06). | D23, D26 |
| `hashing` | The hashes that stand in for secrets in the database: SHA-256 of a token, and a hash-based message authentication code (HMAC) with SHA-256, under `HMAC_KEY`, of a value short or common enough to guess, such as an email (SEC-03). | D26, D32 |
| `blocking` | Runs CPU-heavy work, such as hashing a password, on a blocking thread, inside a span of its own, at most one at once per processor (CODE-05, OBS-06, OPS-07). | D10, D81 |
| `personal_data` | The export and erasure contract that modules implement, and the archive of an export, which deflates each document a contract writes. | D31 |
| `archive` | The layout of a ZIP file from its entries' stored sizes and checksums, which serves any range of it without its entries' bytes. | D31, D126 |
| `lifecycle` | Liveness, readiness and bounded graceful shutdown. | D9, D20 |

`platform` knows two tenancy keys, a user ID and a workspace ID, because authorization, job scoping and event delivery need them. It knows nothing about memberships, roles or any other product rule.

### `app`

```
crates/app/
  src/
    main.rs            the subcommands (§9)
    lib.rs             the crate root: makes public only the composition root's entry points (CODE-15)
    composition.rs     the composition root: builds every service once at startup and mounts every module's routes
    modules/           one directory per module (§5)
  templates/           each module's email templates, in a directory named after the module
  tests/               integration tests over HTTP, against the real services, and end-to-end tests in tests/e2e/
```

## 5. Modules

### What each module owns

| Module | Owns (tables) | Responsible for |
|---|---|---|
| `access` | `workspace_memberships` | Roles, the permission matrix, the permission types used with `Authorized<P>`, and the `AccessPolicy` implementation (D2). It has no routes of its own. |
| `audit` | `audit_entries`, `audit_entry_clients` | Recording entries inside a unit of work, with each one's client beside it, which deleting the client's user deletes, the read endpoints, the action catalog, retention, and the `SecurityEventSink` implementation that records refused requests (D6, D124). |
| `realtime` | none | The endpoint of server-sent events (SSE), the frames it sends, and the event catalog. Delivery, replay and stream closing are handled by `platform::events` (D5). |
| `files` | `workspace_files`, `user_files`, `avatars`, `exported_files`, `object_deletions` | Upload, list, download and delete of user and workspace files, the types each upload accepts and its scan for malware, public files, signed download URLs, and the deletion of the bytes that no row names, with the images of the avatars that `accounts` sets, and of the workspace pictures that the full `workspaces` module adds (D117, D120). <!-- part: files --> |
| `accounts` | `users`, `data_exports`, `data_export_entries`, `data_export_chunks` | The profile, and account deletion and export, which it orchestrates across modules through the personal data contract (D16, D31). |
| `auth` | `sessions`, `device_tokens`, `verification_codes`, `identities`, `provider_tokens`, `pending_email_changes` | Registration, sign-in with a password or OAuth, refresh, sign-out, sessions, password reset and change, email verification and change, identity linking, and re-authentication for sensitive actions. Account deletion is confirmed here, then carried out by `accounts`. One file per OAuth provider in `auth/providers/`. Implements `SessionVerifier`. |
| `workspaces` | `workspaces`, `setup_runs` | Creating workspaces in each shape (below), updating, transferring and deleting them, and the members endpoints (list, change role, remove, leave). |
| `invitations` | `invitations` | Invite, list, resend, cancel, accept, decline, and expiry. <!-- part: invitations --> |
| `superadmins` | `superadmins` | The deployment's superadmins (D62): users who see every workspace's name, creation time, member count and owner, but not its content, delete any workspace, and create workspaces, which they then own ([workspace shapes](#workspace-shapes)). Its routes are authorized by the caller being a superadmin, never by a membership, and it records each action in the superadmin's own log, since deleting a workspace deletes the workspace's log. Its subcommand adds an account to the superadmins or removes it, which the API can't. |
| `client_telemetry` | `telemetry_consents` | The telemetry proxy for clients, and each user's consent to it, which it enforces on the server (D7, D122). |
| `system` | none | The capabilities endpoint (D9), the schema endpoints, which list the permissions and the audit actions, and `seed`, which fills a development database with demo accounts and workspaces through the API's own routes (D28). The health probes belong to `platform` (§4). |
| `platform` | `events`, `jobs`, `job_schedules`, `rate_limit_buckets`, `idempotency_records` | The mechanisms in §4. |

Table naming follows the data standard.

### Workspace shapes

Data a module adds for its product lives in a workspace, and is reached through `Authorized<P>` (§6). Account data, such as the profile, sessions and the user's own audit log, belongs to the user. A product uses workspaces in one of the shapes below, and the optional parts a fork keeps decide which.

| Shape | Who creates workspaces | Who joins one | The optional parts it keeps |
|---|---|---|---|
| Teams | each user, as many as they need | people a member invites | users creating workspaces, `invitations` <!-- part: invitations --> |
| Provisioned organizations | a superadmin, one for each organization the deployment serves | people a member invites | `superadmins`, `invitations` <!-- part: invitations --> |
| One organization | the setup subcommand, run by an operator for each workspace the organization needs, such as one per subsidiary, together with its owner's account (§9) | people a member invites | the setup subcommand, `invitations` <!-- part: invitations --> |
| Personal | the client, whenever a signed-in user belongs to none | nobody | users creating workspaces |

A fork chooses its shape once, when it starts, by removing the optional parts it doesn't keep with `just remove` (ARCH-07), never through a setting, so it carries no code for a shape it doesn't use.
A shape is enforced by the routes it removes: a route a fork keeps treats every caller the same, whatever the fork's client offers, and lets a caller do only what their role allows (§6).
In the personal shape, the API has no notion of a personal workspace, and nobody can join the workspace a user creates.
The client works in the oldest workspace the user belongs to, so a workspace made outside the client changes nothing the user sees, and it can leave workspaces out of its interface.
After creating one, it lists the user's workspaces again before working in any, so two devices that each found none and each created one both work in the older, and the other stays empty.
In the one organization shape too, a user usually works in one workspace, and a client offers a choice only to a user who belongs to several, such as the owner of two subsidiaries. <!-- part: invitations -->
The template keeps every part and serves the teams shape. <!-- part: invitations -->
Registration creates no workspace, and a user who belongs to none is offered to create one (D16).
An invited user joins only the workspace they were invited to. <!-- part: invitations -->
Only an operator runs the setup subcommand, so keeping it changes nothing for the template's users.
A fork may keep `superadmins` in any shape, for seeing and deleting workspaces.
In the provisioned shape, it is the one part that creates them. <!-- part: invitations -->
A superadmin who creates a workspace becomes its owner, as a user who creates one does, and hands it over through the ordinary ownership transfer when asked.
They set it up and invite its people first. <!-- part: invitations -->
They see its content as a member, never as a superadmin.
How each part creates a workspace is in the [workspaces spec](spec/workspaces.md#creating-a-workspace).
In every shape, the same authorization and the same tenancy checks guard the data (DATA-16, TEST-08).

### Dependencies

An arrow means "uses the public API of".

```mermaid
flowchart TD
  workspaces --> access & accounts & audit
  %% part: files
  workspaces --> files
  %% end part: files
  %% part: invitations
  invitations --> access & accounts & workspaces & audit
  %% end part: invitations
  auth --> accounts & audit
  accounts --> access & audit
  %% part: files
  accounts --> files
  files --> access & audit
  %% end part: files
  audit --> access
  superadmins --> workspaces & access & accounts & audit
```

`access`, `realtime`, `system` and `client_telemetry` depend on no other module.

The edges above are the only ones allowed, and the graph stays acyclic (ARCH-05). A module is used only through what its `mod.rs` exports (ARCH-04), and its SQL reads and writes only the tables it owns (ARCH-06). When a module needs another module's data, it calls a batch read in that module's API, such as `accounts::profiles(ids)`. An operation that spans modules passes one `&mut UnitOfWork` through every call, so all of it commits or none of it does (ARCH-11).

`platform` calls into `app` only through four traits, each implemented by a module and registered in the composition root (ARCH-12, ARCH-13):
- `SessionVerifier`, implemented by `auth`, verifies credentials for `platform::authn`.
- `AccessPolicy`, implemented by `access`, makes authorization decisions for `platform::authz`.
- `SecurityEventSink`, implemented by `audit`, records refused requests that `platform::authz` reports.
- The personal data contract is implemented by every module that stores personal data. It lets `accounts` export or delete an account without depending on every module.

### Inside a module

Every module uses the same file names, and only the files it needs (ARCH-03):

```
modules/workspaces/
  mod.rs             public API: what other modules may use, plus routes()
  routes.rs          handlers and route table: extract, authorize, call the service, shape the response
  service.rs         business rules, and the commit that ends the unit of work
  repo.rs            SQL only (private)
  model.rs           domain types
  error.rs           the module's error enum (D17)
  events.rs          events it publishes
  audit.rs           audit actions it records, and the kinds of resource they happen to
  jobs.rs            background jobs it declares (D13)
  emails.rs          emails it sends, rendered from its templates in templates/workspaces/ (D1)
  metrics.rs         metrics it declares (D19)
  personal_data.rs   its export and erasure contract (D31)
```

Three modules have one extra entry each: `access/permissions.rs` holds the permission matrix, `auth/providers/` holds one file per OAuth provider together with their registry, and `system/seed.rs` holds `seed`, a client of the API's routes, which names HTTP types as a service may not ([system spec](spec/system.md#seed)).

The layers have strict jobs (ARCH-08 to ARCH-10):
- **Handlers** contain no SQL and no business rules.
- **Services** contain no HTTP types.
- **Repositories** contain no rules.

A module's personal data contract, in its `personal_data.rs`, is a service of its own: it applies the module's rules to a deleted user's rows, through its repository or its `service.rs`, as a service does (D31).

A module splits a file into a directory with the same name, such as `service/`, when the file outgrows its limit, or when an optional part adds functions of its own to it, which the part then owns in a file of their own (ARCH-03, CODE-08, D119).

A trait that abstracts a dependency either inverts a dependency the crate or module graph forbids, as the four traits above do, or covers an external system with more than one implementation, such as email transport or the clock (ARCH-13). Nothing gets a trait just so it can be mocked, because tests run against the real database (D23).

## 6. Request lifecycle

```mermaid
sequenceDiagram
  participant C as Client
  participant P as platform middleware
  participant H as Module handler
  participant S as Module service
  participant DB as PostgreSQL
  C->>P: request
  P->>P: security headers, trace context, request log,<br/>panics, client, CORS, load shedding, IP rate limit, cross-site check
  P->>H: routed request
  H->>H: timeout, body limit, query check, Principal (authn),<br/>user rate limit, idempotency key, parse to domain types
  H->>DB: on a workspace route, BEGIN and authorize (authz)
  H->>S: call with domain types, and the unit of work on a workspace route
  S->>DB: on every other route, BEGIN
  S->>DB: domain writes + audit entries + events + jobs
  S->>DB: COMMIT
  DB-->>P: NOTIFY (events)
  S-->>H: result or module error
  H-->>C: response, or RFC 9457 problem with trace_id
```

1. **Caddy** terminates TLS, opens the request's trace, and forwards the request to a replica whose readiness check passes. While none passes, Caddy answers itself, with the 503 problem `request.unavailable` (OPS-05, D53).
2. **Platform middleware**, outermost first. The security headers wrap every response (SEC-12). The health probes are answered inside them, before the rest of the stack (OPS-05):
   - the security headers
   - trace context: the request's server span, and the trace-ID response header (D10)
   - the request log: one line when the request completes, and the error line of a 5xx (OBS-03, ERR-10)
   - panic catching (ERR-12)
   - the client: its IP, from the trusted proxies' header, and its user agent (PLATFORM-053)
   - CORS for the listed web origins (D61)
   - concurrency limit with load shedding (D20)
   - pre-authentication IP rate limit (D11)
   - the cross-site check (D61)
3. **The route** prepares the request, in this order:
   - Its route group's timeout and body size limit apply (OPS-07, API-08).
   - Query parameters that the route's documentation doesn't list are refused (API-09).
   - `Principal` authenticates the caller, by the Bearer header or a browser's cookie (D18, D61).
     On a download route, the signed URL its query holds authenticates it in place of either (D113). <!-- part: files -->
   - The per-user rate limit, in its own short transaction, then the `Idempotency-Key` of a keyed `POST`, which the unit of work claims as its first statement, after the authorization that opens a workspace route's, so the key's record commits or rolls back with the operation (D12).
   - The body is parsed into request types, then domain types (D25).
   - On an upload route, whose group has a timeout and no body limit of its own, the order differs: the handler refuses a declared length over its module's limit, then, on a workspace's, checks that the caller may upload, through `Authorize::check`, in a short transaction that writes nothing, and only then reads the body, through `Upload`, in one of the replica's turns, before the unit of work opens and authorizes the caller again (PLATFORM-073, PLATFORM-074). <!-- part: files -->
   - The unit of work opens last, so no transaction waits on a slow client, and no request holds a pooled connection while it waits for a second one. On workspace routes, opening it produces `Authorized<P>`: inside the unit of work, the caller's membership in the workspace from the path is loaded and permission `P` checked (D2, D3). A missing membership is a 404, so the response never reveals whether the workspace exists. On other routes, the service opens it, once any per-action limit it checks has counted, such as the sign-in's (AUTH-006), after a read, in a transaction of its own that writes nothing, of what decides which limit counts, such as the sign-in's device token (ARCH-11).
4. **The handler** calls the service with domain types only.
5. **The service** performs its writes in its unit of work, and in the same transaction records audit entries, publishes events and enqueues jobs (D1, D6). For a request with an `Idempotency-Key`, the same transaction stores the operation's result under the key (API-12). Then it commits.
6. **The response.** The handler maps the result to the response. A module error goes through the single conversion to an RFC 9457 problem that carries the trace ID (D17).
7. **After the commit**, the background work picks up what committed:
   - Postgres notifies each instance's hub, which loads a new event once and delivers it to the streams allowed to see it, and closes the streams that an access change affects (D5, D20, D35).
   - A job dispatcher, which polls the queue every second, claims each new job.

Nothing runs after the response except background work driven by committed rows.

## 7. Data and consistency

- **Database roles:**
  - `owner` runs migrations and owns the tables. It is never used at runtime.
  - `app` serves every request and runs every job. It can read and change data, but not the schema, and it is never a superuser (DATA-12, DATA-13).
- **Tenancy is enforced by the application (D4).** There is no row-level security. Every query on tenant data filters by tenant itself, but the reads that DATA-16 exempts (DATA-16): workspace data by the workspace ID from `Authorized<P>` or a job's recorded scope, and user data by the caller's ID. Three things keep that true:
  - Repository functions for workspace data take the workspace ID as a parameter.
  - A CI check fails any query on a workspace table that doesn't filter by `workspace_id`, or by `user_id` on a table that also has one (DATA-16).
  - A generated test sends every workspace route an ID from another workspace the caller belongs to (TEST-08).
- **No business logic lives in SQL**, and there are no stored procedures or `SECURITY DEFINER` functions (DATA-07).
- **What the role can't protect against.** A compromised API process can read and change anything `app` can. The defenses for that case are:
  - The database is not reachable from outside the private network.
  - Roles have least privilege: no data definition language (DDL) statements, no superuser, no other schemas.
  - Secrets are handled under D26.
  - The audit log and backups support detection and recovery (D6, D30).
- **Conventions (D24):**
  - UUIDv7 IDs from `uuidv7()`.
  - Timestamps from `now()` in the database.
  - SQL checked at compile time with `sqlx::query!` and a committed offline cache.
  - `ETag` and `If-Match` on editable resources.
  - Hard deletes, with cascades declared in the schema.
- **Side-effect tables.** A unit of work writes to the module's own tables, `audit_entries` (owned by `audit`), and `events` and `jobs` (owned by `platform`). After the commit:
  - Events reach every authorized stream, in the order their units of work committed, and a stream that reconnects replays what it missed.
  - Jobs run at least once, with retry and backoff, and land in a dead-letter state after the last attempt. Every job handler is idempotent, but for an effect outside the API that a run can't take back, such as an email, whose spec states what a repeat causes (OPS-09).
- **Migrations** are forward-only and follow expand and contract, so the previous release keeps working during a rolling deploy (D15). They live in one ordered directory, and each file's description names its owning module.

## 8. Cross-cutting mechanisms

Each of these has a standard in `docs/standards/` with numbered rules. This is the one-paragraph version.

- **Errors (D17).** Each module has its own error enum. Every variant of an error that a route can answer declares a stable code and a category, and the category alone sets the HTTP status. Infrastructure errors never leave their adapter. An error is logged once, where it is handled.
- **Validation (D25).** Request types reject unknown fields and convert at the boundary into domain types that can only hold valid values. Code past the boundary accepts only those types.
- **Authentication (D5, D18, D61, D63).** Sessions slide on refresh but have an absolute maximum lifetime. Desktop and web clients are both supported: credentials arrive in the Bearer header from a client that sets its own headers, or in an `HttpOnly` cookie from a browser, whose requests CSRF protection guards, from the origins that the CORS allowlist names.
  A download also takes a URL signed for a session, whose user it authorizes as any caller's (D113). <!-- part: files -->
  Every user may add a second factor, and every superadmin must. Revoking a session closes its streams.
- **Authorization (D2, D62).** One permission matrix, in `access/permissions.rs`, maps roles to permissions. Every workspace route declares its permission in its type. Role rank limits who may change whom. A route of `superadmins` is authorized by the caller being a superadmin instead, and reaches no workspace's content.
- **Observability (D10, D19).** Every response except the health probes carries a trace ID that leads to its logs, and to its trace whenever the request ended in an error. Metrics are declared in code. The metric catalog is generated from the declarations, and CI checks the dashboards and alerts against it.
- **Configuration (D9).** Strict and typed, with no defaults that are unsafe in production. A disabled feature answers 503 and is advertised by the capabilities endpoint.
- **Background jobs (D13).** Declared in one registry. A queued job runs at least once, on whichever instance claims it, with retries and a dead-letter state; a periodic job runs once per interval across the fleet. Each run holds a lease that a heartbeat renews, and shutdown is bounded.
- **Personal data (D31).** Every module that stores personal data implements export and erasure. CI fails if any column referencing a user isn't covered by a contract.
- **Security (D26).** The bar is the one that [the security rules](standards/security.md) state, from the Application Security Verification Standard (ASVS) of the Open Worldwide Application Security Project (OWASP) (D63). The [threat model](operations/threat-model.md) maps each mitigation to a rule ID.

## 9. Runtime topology

- **The binary** has these subcommands:
  - `bootstrap` creates the database roles and their grants, with an administrator connection. Running it again keeps the roles and grants, and sets each role's password to the one in its URL (PLATFORM-002).
  - `serve` binds one HTTP port and runs the HTTP server, the real-time hub, the job dispatcher and the scheduler.
  - `migrate` is the one-shot migration step.
  - `seed` fills a development database with demo accounts and workspaces, through the API's own routes, as a client would, and refuses in any other environment ([system spec](spec/system.md#seed)).
  - The setup subcommand creates a workspace and its owner's account, each time an operator runs it, in a fork that keeps it (§5, "Workspace shapes").
  - The superadmin subcommand adds an account to the superadmins or removes it, in a fork that keeps `superadmins` (D62). `just` runs it, on a deployment's server over Secure Shell (SSH), so only someone who can reach the server can change who is a superadmin.
  - `openapi` writes the contract, `openapi.json`, which a test compares with the committed one (API-17).
  - `health` asks the instance whether it is ready, for the image's health check (PLATFORM-063).
- **The single-host reference deployment** runs on one Linux server, in Docker Compose (D15, D20, D45):
  - Caddy terminates TLS for the API's address and Grafana's, with certificates it obtains through the Automatic Certificate Management Environment (ACME), and sets HTTP Strict Transport Security (HSTS) (SEC-12). It sets `X-Forwarded-For` from the connection, or keeps the one a configured trusted proxy sends, such as a content delivery network in front of the server (SEC-13). It balances requests across the replicas its own health checks find ready (OPS-05).
  - The API runs in two colors, blue and green, of N replicas each, every replica with the deployment's limits on CPU and memory. Caddy names every replica of both colors and checks each one's readiness, so a replica that isn't running fails its check and gets no request. Between deploys, one color runs.
  - The scanner, ClamAV's daemon beside the updater of its signatures, runs once, from ClamAV's own image, and only the replicas reach it ([deploy.md](operations/deploy.md#the-scanner)). <!-- part: files -->
  - The bundled Postgres runs from an image that the pipeline builds beside the API's: the official image with pgBackRest (D44). Only the deployment's private network reaches it. It archives its write-ahead log continuously, and takes scheduled base backups, encrypted, to an S3-compatible bucket off the server (OPS-15).
  - Telemetry goes where D42 says, below.
  - The database and telemetry are each bundled or a provider's, and one setting of the deployment chooses which, with no file to edit: the deployment then runs only the services that choice needs, and the API's settings change only in the addresses and credentials they hold. The database is the bundled Postgres, or a managed one, whose provider then gives the point-in-time recovery (OPS-15). Telemetry goes to the self-hosted stack or to a managed backend (D42). The backups go to a bucket that a provider holds off the server, at any provider that speaks S3, with no setting that bundles one (D44): a bucket on the server would lose what it holds with the server, so it would need a backup off the server of its own.
    Files' bytes go to such a bucket too, in every deployment. <!-- part: files -->
- **A deploy** takes a release by its version, the latest release's unless the developer names one. `just deploy`, on a developer's machine, refuses a version that no release has, with a pointer to [release.md](operations/release.md), so a developer who deploys learns there that a release comes first. It verifies the release's images as `verify-release` does, and gives the server only what the release published: the images, which the server pulls by digest, and the deployment's files from the release's tag, never a checkout of the repository. On the server, the deploy runs `bootstrap` and `migrate`, starts the idle color, waits until Caddy finds every new replica ready, then stops the live color, whose replicas drain (OPS-06), and removes what earlier deploys left and nothing uses: the other releases' files, and their images but the previous release's API image, which stays for a rollback, so the server's disk doesn't fill with them, while every container's log is capped (D57). A new replica that doesn't become ready rolls the deploy back: the deploy removes the idle color, and the live one never stopped (OPS-14). Migrations follow expand and contract, so both colors work during the switch (DATA-10). Postgres and Caddy each run once, so restarting one interrupts requests, and a deploy restarts them only when the operator allows it.
  The scanner runs once too, and its restart interrupts uploads alone. <!-- part: files -->
  One deploy runs at a time, so two `bootstrap` runs never overlap.
- **Telemetry in a deployment** goes to one of two places (D42):
  - The self-hosted stack, the default. Grafana answers on an HTTPS address of its own behind Caddy, and every viewer signs in; nobody views it anonymously.
  - A managed backend that accepts OTLP, such as Grafana Cloud, instead of Prometheus, Loki, Tempo, Grafana and Alertmanager. The collector still runs beside the API, keeps its receivers and processors, tail sampling among them (OBS-08), and holds the backend's credentials. It exports to the backend, and does there what the stack's Prometheus does: it scrapes its own metrics, and turns resource attributes into labels. The API's configuration is the same. Grafana Cloud is the worked example, into which the dashboards and the rules are imported. With another vendor, the telemetry arrives the same way, and a fork rebuilds the dashboards and alerts in the vendor's tools.
- **Scaling out** means adding replicas. An instance opens at most its pool's size plus the listener, and logs that number when it starts. While a deploy runs, both colors do, and a deploy refuses replicas whose connections wouldn't fit within Postgres's, as [deploy.md](operations/deploy.md#scaling) works out (OPS-18). The listener needs a direct connection, which rules out PgBouncer in transaction mode. Under load, each replica sheds requests with 503 and `Retry-After` instead of queuing without bound (D20).
- **Multiple hosts.** The image and its environment contract don't depend on a platform. The operations docs state what any platform must provide: Postgres that the replicas share, with point-in-time recovery, on a server that holds no other deployment's database, readiness checks that stop traffic before an instance drains, a grace period longer than its shutdown, and TLS and encryption at rest (OPS-17).
  With `files`, it also needs a bucket for files' bytes, and a malware scanner that only a private network reaches. <!-- part: files -->

## 10. Technology

### In use

| Concern | Choice | Why |
|---|---|---|
| Language | Rust, edition 2024, toolchain pinned in `rust-toolchain.toml` | Every machine and CI job builds with the same compiler |
| Async runtime | `tokio` | The ecosystem standard |
| HTTP | `axum` | Extractors carry `Principal` and `Authorized<P>` as types |
| Database | `sqlx` with `query!` macros | Queries are checked against the schema at compile time (D24) |
| Serialization | `serde`, `serde_json` | The ecosystem standard |
| Errors | `thiserror` in modules | One enum per module (D17) |
| Logging | `tracing`, `tracing-subscriber`, with `json-subscriber` for JSON lines and `tracing-appender` for the logging thread | The ecosystem standard; `json-subscriber` puts the trace context and the service's fields at the top level of a line (OBS-02, OBS-05) |
| Trace context | `opentelemetry`, `opentelemetry_sdk`, `tracing-opentelemetry` | The standard bridge from `tracing` spans to OpenTelemetry (D10, D19) |
| Telemetry names | `opentelemetry-semantic-conventions` | The conventions' names, checked at compile time (OBS-05) |
| Email | `lettre` over SMTP, with `rustls` on `ring`, and `askama` templates; Mailpit catches development email (D28) | Async SMTP with pooled connections and the system's certificates (SEC-16, D38); templates checked against their values at compile time, with HTML escaped ([platform spec](spec/platform.md#sending-email)) |
| Telemetry export | `opentelemetry-otlp` over HTTP with protobuf bodies, through a blocking `reqwest` client on `rustls` with its ring provider, and `opentelemetry-appender-tracing` for log records | The protocol's default transport; the client runs on a thread of its own, so an export never needs the async runtime (D10, D19) |
| Observability stack | the OpenTelemetry Collector's contrib distribution, Prometheus, Loki, Tempo, Grafana and Alertmanager | D19's stack, configured once under `deploy/observability/` for development and production |
| Auto-reload | watchexec | `just dev` restarts the API when its code changes, on Windows, macOS and Linux (D28) |
| Password hashing | `argon2`, for the Argon2id that SEC-04 requires | The RustCrypto implementation, in pure Rust, which reads and writes the hashes in the string format of the Password Hashing Competition (PHC) (AUTH-014) |
| Middleware | `tower`, `tower-http` | The concurrency limit, load shedding, timeouts, panic catching and headers that axum is built to use |
| OpenAPI | `utoipa`, `utoipa-axum` | The document comes from the handlers' own annotations, and a route is registered with its documentation (API-14, API-17) |
| API reference | Swagger UI, from the jsDelivr content delivery network, which the API serves in development only (D28) | The most widely used reference for OpenAPI documents, under Apache-2.0; on the API's own origin, its requests carry the session's cookies, and it needs no copy in the repository or the image (PLATFORM-052) |
| Command line | `clap` | The binary's subcommands (ARCH-14) |
| Container image | BuildKit, through `docker buildx`; zig, through cargo-zigbuild, which compiles both platforms' static binaries against musl on one machine; cargo-chef; cargo-auditable; distroless's static base (D43) | One build gives both platforms' images, the compiled dependencies stay cached, the scans and the software bill of materials (SBOM) see the binary's crates, and the base has no shell, package manager or C library (OPS-12, OPS-13) |
| Allocator of the image's binary | `mimalloc`, for the musl targets only | Under musl's own allocator, a replica's memory grows with every request it serves, until the container's memory limit kills it; under mimalloc it stays flat (OPS-16) |
| Reverse proxy and TLS | Caddy | It obtains and renews certificates through the ACME protocol on its own, and checks the replicas' readiness before it sends them requests (OPS-14, OPS-17) |
| Deployment | Docker Compose on one server, set up by `deploy/server-setup.sh` and driven by `deploy/deploy.sh`, scripts for any shell of the Portable Operating System Interface (POSIX), which `just server-setup`, `just server-check`, `just deploy`, `just backup`, `just restore`, `just status` and `just logs` run over Secure Shell (SSH) from a developer's machine (OPS-19) | The server needs only Docker, which the setup installs; the release's files and its signed images are all a deploy takes, with no agent and no checkout on the server (D45, D51) |
| Load testing | k6 | Constant-arrival-rate scenarios measure the rate a replica serves within the latency objective (OPS-16) |
| Malware scanning | ClamAV's daemon, `clamd`, from ClamAV's own Debian image, which serves amd64 and arm64, beside `freshclam` for its signatures in the reference deployment; with one signature of the repository's own, for the test file of the European Institute for Computer Antivirus Research (EICAR), in development and the tests; reached through a client of `platform`'s own over TCP | The open-source scanner that the antivirus signatures ASVS 5.4.3 asks for come with, updated several times a day; its protocol is a few lines, which a client written here bounds by the clock and counts ([platform spec](spec/platform.md#scanner)) <!-- part: files --> |
| Object storage | `object_store`, signing with `ring` and sending through a `reqwest` client on `rustls` with its ring provider; SeaweedFS in development and the tests (D28) | The Apache Arrow project's client of S3-compatible buckets, which reads an object as a stream; SeaweedFS serves S3 without credentials in one container, as the end-to-end tests' backup bucket already does ([platform spec](spec/platform.md#storage)) <!-- part: files --> |
| Archives | `flate2`, on `miniz_oxide`, and `crc32fast`; a ZIP layout of `platform`'s own | The deflate of Rust's ecosystem, in Rust alone, and the checksum with the processor's instructions; no crate lays out an archive without its entries' bytes, or serves a range of one ([platform spec](spec/platform.md#archives)) |
| Backups | pgBackRest, in the reference deployment's image of the official Postgres | Continuous archiving of the write-ahead log, and point-in-time recovery from an S3-compatible bucket, encrypted, with retention (D44) |
| Image scan | Grype | It reads both the base's packages and the crates the binary records (SEC-15) |
| Shell linting | shellcheck | The standard linter for shell, run on the recipes of the justfile and the files it imports, and the deployment's scripts (CODE-07) |
| Registry and signing | crane, and cosign with keyless signing | crane copies and tags an image without changing its digest, so promoting it never rewrites it (OPS-12) |
| Dependency updates | Dependabot | It reads the crates, the actions, the images of both Dockerfiles and of every Compose file, and the toolchain, and needs no app installed (OPS-11) |

### Starting points

A part's tools and libraries are chosen when it is built, as "Docs for unbuilt parts are the plan" in [CLAUDE.md](../CLAUDE.md) says. These are the candidates the design started from, with the step of the [build order](MIGRATION.md#build-order) that chooses each. Where another section of this document names one, it describes the plan, not a choice.

| Concern | Starting point | Chosen in |
|---|---|---|
| Outbound HTTP | `reqwest` with `rustls` | the first outbound caller (5) |

The decided parts of the stack are decisions with recorded reasons: OpenTelemetry and the Grafana stack (D10, D19), GitHub Actions and `just` (D21), and Postgres as the only shared state (D20).

Validation uses the domain types (D25), not a validation crate. Async traits are native wherever no trait object is needed.

Exact versions are pinned in `Cargo.toml`.

## 11. Glossary

Each concept has one name, used the same way in code, SQL, the API and the docs (CODE-11).

| Term | Meaning |
|---|---|
| User | A person with an account. Owns their profile, sessions and identities, and the data they keep apart from any workspace. |
| Identity | One way a user signs in: a password, or a linked OAuth provider account. A user who registered has one or more. An account an operator creates for its owner has none until the owner sets a password ([the setup subcommand](spec/workspaces.md#the-setup-subcommand)). |
| Session | One signed-in device or client, holding an access token and a refresh token. |
| Provider token | A credential from an OAuth provider, stored encrypted (SEC-02). It is revoked when the session or identity that needs it ends. |
| Device token | A token that a registration or a sign-in gives the device, for the account's email, so that the device's next sign-in to that account counts only in a limit of its own, which no attempt from elsewhere fills (AUTH-019). |
| Principal | The authenticated caller of a request, produced by `platform::authn` (D18). |
| Workspace | A space whose members share its data: a team's, an organization's, or one user's own (§5). Owns the data its members share, and its audit log. |
| Workspace shape | How a product uses workspaces: who creates them and who joins them, which the optional parts a fork keeps decide (§5). |
| Membership | A user's place in a workspace, with exactly one role. Owned by `access`. |
| Owner | The one role per workspace that can transfer or delete it. A superadmin can also delete it. |
| Superadmin | A user whom the deployment's operator made one through `just`, who sees every workspace without being a member, but not its content, and deletes any workspace (§5, D62). Not a role: roles belong to memberships. |
| Role | A named set of permissions, defined in code in `access` (D2). |
| Permission | One thing a role allows on a workspace resource, such as `members.update`. |
| Invitation | An offer to join a workspace with a role, sent to an email address. <!-- part: invitations --> |
| Unit of work | One database transaction, opened by the service, or with `Authorized<P>` on a workspace route, with its side effects: audit entries, events and jobs (D1). |
| Job | Durable background work, delivered at least once (D13). |
| Migration | One forward-only change to the schema, in a file of `migrations/`, applied by `migrate` (DATA-09). |
| Startup | What a subcommand does before it serves: reading the configuration and running its checks. For a one-shot subcommand, such as `migrate`, its whole run. A startup failure ends the process (OPS-03). |
| Event | A record of a change, named `<resource>.<verb>`, such as `member.removed`, its name, which its frame's `event` carries (API-19), delivered to clients over SSE and replayable after a reconnect (D5). |
| Stream | One client's open SSE connection. |
| Frame type | What one frame of a stream is, which its `event` names: an event's name, or one of the frames the stream sends of its own, such as `stream.closed`; the event catalog documents each one (API-19, REALTIME-008). |
| Listener | The instance's own connection to the database, outside its pool, on which the hub listens for what commits and readiness checks the database (OPS-05, OPS-18). |
| Workers | The turns of an instance's CPU-heavy work, such as a password's hash: one per processor it may use, each held by one work at once, which `platform::blocking::Workers` hands out (PLATFORM-060). |
| Audit entry | A record of who did what, in the user's log or the workspace's log. It is never edited, except that erasing an account anonymizes its personal data (D6, D31). |
| File | Stored bytes plus metadata. A user file or a workspace file, each private or public. <!-- part: files --> |
| Avatar | The image that shows a user to others, at most one for each user, which `accounts` sets and `files` keeps. Not a file: it has no name, and is always public (D120). <!-- part: files --> |
| Personal data contract | A module's export and erasure of the personal data it stores (D31). |
| Release | A version that `just release` asked for, which a run of CI on `main` then signed and tagged `v<version>` (OPS-12). A final release is `X.Y.Z`. A prerelease, `X.Y.Z-rc.N`, comes before the final release of the same number, and is a version to try: a final release fixes the HTTP contract, the error codes and the applied migrations (API-18, ERR-02, DATA-09), and a prerelease doesn't. |
| Digest | The hash of an image's content, which names exactly that image. A tag can move to another image; a digest can't. |
| Image index | The list in a registry that names one image's version for each platform, with each one's SBOM and provenance. A multi-platform image's digest is its index's. |
| Capability | A feature that a deployment can turn off, which the capabilities endpoint lists with whether it is on (D9). |
| Trace ID | The identifier, returned with every response except the health probes, that leads to a request's logs, and to its trace whenever the request ended in an error (D10). |
| Personal data | In these documents: information that identifies a person directly, such as email addresses, names, avatars, IP addresses, user agents, file names and text a user writes. User and workspace IDs are pseudonymous. The General Data Protection Regulation (GDPR) still counts them as personal data, so export and erasure cover them (D31), but they may appear in telemetry and audit entries. |
| Point-in-time recovery | A restore of a database as it stood at any moment its backups reach, from a base backup and the write-ahead log archived after it: what the reference deployment's bundled Postgres provides, and what it requires of a managed one (OPS-15). |

The documents assume a developer knows these acronyms, and don't spell them out (DOC-06): API, ASCII, CI, CPU, CSV, DNS, GIF, HTML, HTTP, HTTPS, ID, IP, JPEG, JSON, PDF, PNG, REST, RFC, SDK, SQL, TCP, TLS, UI, URL, UTC, UTF and UUID. These names are written in capitals, and aren't acronyms to spell out: AES, BSD, CDLA, GCM, IETF, ISC, MIT, QEMU, README, S3, SHA, SIGINT, SIGKILL, SIGTERM, SOC, W3C, ZIP and ZIP64.
