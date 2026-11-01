# Triage: the first slice

Maps each old behavior the first slice touches to its verdict and to the spec behavior that carries it. Behaviors not listed here are triaged when their module is specified.

Verdicts:
- **kept:** the new behavior is the same.
- **changed:** the new behavior differs, for the reason given.
- **dropped:** no new behavior carries it.

| Old behavior | Verdict | New behavior | Reason for the change |
|---|---|---|---|
| AUTH-001 Register | changed | AUTH-001 | 201 Created (API-04); no workspace in the response (D3); audited in the same transaction |
| AUTH-002 Existing email on registration | changed | AUTH-002 | 409 `auth.email_in_use`; enumeration accepted on registration (D7) |
| AUTH-004 Log in | changed | AUTH-004 | No workspace resolution at sign-in (D3) |
| AUTH-005 Uniform login failures | changed | AUTH-005 | Failed attempts against an account are audited (D6) |
| AUTH-006 Per-email login rate limit | kept | AUTH-006 | |
| AUTH-014 Login lands in the default workspace | dropped | | No server-side active workspace (D3) |
| AUTH-015 Workspace resolution on refresh | dropped | | No server-side active workspace (D3) |
| AUTH-016 Opaque token pair, HMACs only | changed | AUTH-007 | Absolute session lifetime (D5); tokens stored as SHA-256 (D32) |
| AUTH-017 One session per device | changed | AUTH-008 | Device ID optional (D18); a replaced session is fully revoked (D5) |
| AUTH-019 Bearer validation | changed | AUTH-009 | Distinct code for an expired token; no session ID in the error (D8) |
| AUTH-022 Refresh rotation | changed | AUTH-010 | Sliding expiry bounded by the absolute lifetime (D5) |
| AUTH-023 Expired refresh token | kept | AUTH-011 | |
| AUTH-024 Refresh-token reuse | changed | AUTH-012 | Access-change signal, and audit in the same transaction (D1, D35) |
| AUTH-027 Log out | changed | AUTH-013 | 204 (API-04); the signal closes the session's streams (D35) |
| AUTH-055 Password hashing | changed | AUTH-014 | OWASP parameters; rehash on sign-in (D26) |
| USER-001 Read own profile | changed | ACCOUNTS-001 | Avatar and consent fields arrive with the full module |
| WS-001 New accounts start with no workspace | kept | AUTH-001 | |
| WS-002 Create a workspace | changed | WORKSPACES-001 | Path (D3), 201 (API-04), owner role (D2), workspace log (D6) |
| WS-003 Creating doesn't make the workspace active | dropped | | No server-side active workspace (D3) |
| WS-004 List my workspaces | changed | WORKSPACES-003 | Caller's role in each item; keyset paging; no total count (D8). The name filter is kept as `search` (API-20) |
| WS-018 List members | changed | WORKSPACES-004 | Roles instead of permission lists (D2). `search` by name or email (API-20) and `role` replace the email and permission filters |
| WS-023 Remove a member | changed | WORKSPACES-005 | Rank rule (D2), access-change signal (D35), audit in the same transaction (D1) |
| WS-024 Owner and caller can't be removed | changed | WORKSPACES-006 | The owner is protected by rank (SEC-08) |
| WS-028 Invite by email | changed | INVITATIONS-001, INVITATIONS-008 | A role instead of a permission list (D2); email sent by a job (D1). The invitee's live `invitation.received` is kept |
| WS-029 Existing members can't be invited | kept | INVITATIONS-002 | |
| WS-030 Re-inviting refreshes | changed | INVITATIONS-003 | Audited as an update; 200 |
| WS-034 List my pending invitations | changed | INVITATIONS-004 | Keyset paging instead of in-memory paging (API-06). The workspace-name filter is kept as `search` (API-20) |
| WS-035 Accept an invitation | changed | INVITATIONS-005 | Recorded in the workspace's log (D6); the membership and the invitation's deletion commit together (D34) |
| WS-036 Accept guards | changed | INVITATIONS-006 | Expiry is 422; an invitation for another email is 404 (DATA-22) |
| WS-038 Accepting while a member overwrites | changed | INVITATIONS-006 | 409, and nothing changes (D16) |
| WS-040 Invitation expiry | changed | INVITATIONS-007 | Hourly sweep. Managing expired invitations arrives with the full module |
| MAIL-028 Invitation email | changed | MAIL-002 | Sent by a job with retry (D1); not personalized with the invitee's account name |
| RT-001 Open the event stream | changed | REALTIME-001 | Filtered by permission (SEC-17); closed on loss of access (SEC-06) |
| RT-003 Event frame | kept | REALTIME-002 | |
| RT-004 Heartbeats | kept | REALTIME-003 | |
| RT-008 Replay after reconnect | changed | REALTIME-004 | Membership at the time of the event (SEC-17); truncation is signalled |
| AUDIT-010 Actor binding | kept | AUDIT-002 | |
| AUDIT-012 Client IP and user agent | changed | AUDIT-003 | Readable only with a dedicated permission (SEC-16); the IP comes only from trusted proxies (SEC-12) |
| AUDIT-013 Trace ID per entry | kept | AUDIT-003 | |

## Platform: the database

The old data layer's behaviors that `platform::db` replaces. The schema behaviors (DATA-019 to DATA-026) are triaged with the modules that own the tables, and the pool gauge (DATA-035) with the telemetry.

| Old behavior | Verdict | New behavior | Reason for the change |
|---|---|---|---|
| DATA-001 `migrate` applies the embedded migrations | kept | PLATFORM-027 | |
| DATA-002 Applied migrations are immutable | kept | PLATFORM-029 | Every applied migration is compared before anything is applied, so an edit can't hide behind an unapplied earlier version |
| DATA-003 Concurrent `migrate` runs apply each migration once | kept | PLATFORM-027 | |
| DATA-004 Versions the binary does not embed are ignored | kept | PLATFORM-029 | |
| DATA-005 A failing migration rolls back and fails the command | kept | PLATFORM-028 | |
| DATA-006 `migrate` reads only `DATABASE_URL` | changed | PLATFORM-027 | Reads `DATABASE_OWNER_URL` and the logging settings, and refuses any role but `owner` (DATA-12) |
| DATA-007 Schema changes run only in the one-shot | changed | PLATFORM-030 | `serve` also refuses to start against a schema that lacks one of its migrations |
| DATA-008 Runtime role bootstrap | changed | PLATFORM-010 | A subcommand instead of a script (ARCH-14); creates both roles; the password travels as a SCRAM verifier |
| DATA-009 Runtime role privileges | changed | PLATFORM-010, DATA-19 | Default privileges instead of grants by table name; no temporary tables; role-level timeouts |
| DATA-010 SECURITY DEFINER functions pin `search_path` | dropped | | No `SECURITY DEFINER` functions (DATA-07) |
| DATA-011 Per-transaction RLS context | dropped | | No row-level security (D4) |
| DATA-012 to DATA-018 RLS policies by table | changed | DATA-22, TEST-09 | Tenancy is enforced by each query's own filter, checked by `check-tenancy` and the foreign-workspace probes (D4) |
| DATA-027 Transaction helpers | changed | ARCH-11, PLATFORM-024 | One `UnitOfWork`, opened by the route and committed by the service |
| DATA-028 `_in_tx` methods re-activate context | dropped | | No row-level security (D4) |
| DATA-029 Database error classification and HTTP mapping | changed | ERR-04, ERR-05 | A repository maps each constraint it relies on, and every other database failure is `Internal`. The status for an unreachable database comes with the HTTP errors |
| DATA-030 Service-layer database retry | dropped | | Nothing inside the API retries a transaction: locks are taken in a consistent order, and a deadlock is a defect (DATA-25). Jobs keep their retry (OPS-10) |
| DATA-031 Background-task retry | changed | PLATFORM-013 | Jobs retry with backoff, then move to a dead-letter state (OPS-10) |
| DATA-034 Connection pool | changed | PLATFORM-009 | `DATABASE_MAX_CONNECTIONS`, 10 by default; a 5-second acquire timeout as a constant; no minimum connections |
| DATA-036 Repository access split | dropped | | No repositories that bypass row-level security (D4, D22) |

## Platform: the request pipeline

The old HTTP layer's behaviors that the request pipeline and the `system` module replace. The client IP and the rate limits (HTTP-011, HTTP-030 to HTTP-038) are triaged with rate limiting, request bodies, path parameters and pagination (HTTP-019 to HTTP-029, except HTTP-024) with the modules that first take them, idempotency (HTTP-039 to HTTP-049) with idempotency, and the request metrics and telemetry export (HTTP-013, OBS-010 to OBS-019, OBS-023, OBS-026 to OBS-056) with the telemetry.

| Old behavior | Verdict | New behavior | Reason for the change |
|---|---|---|---|
| HTTP-001 Command-line dispatch | changed | ARCH-14 | One subcommand per task, `serve` included, so no default task runs by mistake |
| HTTP-002 Startup order and fail-fast conditions | changed | OPS-03, OPS-06, PLATFORM-009, PLATFORM-030 | One startup-failure format; the signal handlers are installed first; nothing panics |
| HTTP-003 Composition functions | kept | ARCH-12 | |
| HTTP-004 Graceful HTTP shutdown | changed | PLATFORM-038 | A delay before the listener stops accepting, and a deadline on the drain (OPS-06) |
| HTTP-005 Liveness probe | changed | PLATFORM-019 | No version in the body, as the Application Security Verification Standard (ASVS) asks, and no envelope |
| HTTP-006 Readiness probe | changed | PLATFORM-037 | Also reports a database the instance can't reach (D9) |
| HTTP-007 Global middleware order | changed | architecture §6 | The security headers wrap every response; cross-origin resource sharing (CORS) answers only listed origins (HTTP-010) |
| HTTP-008 Route-group layer stacks | changed | API-14 | One builder per access level, through which every route is registered with its documentation |
| HTTP-009 Versioned prefix, framework defaults for misses | changed | PLATFORM-021, PLATFORM-032 | A path that doesn't exist answers a 404 problem, and a method it doesn't serve a 405 problem with `Allow` |
| HTTP-010 CORS policy | changed | PLATFORM-052, PLATFORM-053 | Only the listed origins of web clients, with credentials, and a request that changes something from any other origin is refused (D61, SEC-19) |
| HTTP-012 One log line per failed request | changed | PLATFORM-034 | One line per request, so every trace ID leads to a log line (D10); a 4xx is logged at `INFO` (ERR-10) |
| HTTP-014 Handler panics become a 500 | kept | ERR-12 | |
| HTTP-015 Request body size limits per route class | changed | PLATFORM-036 | A 413 problem for a declared length over the limit, before the body is read (API-08) |
| HTTP-016 Response envelope: success | dropped | | The body is the resource itself (API-05), and the trace context travels in `server-timing` (API-16) |
| HTTP-017 Response envelope: error | changed | API-07 | RFC 9457 problems |
| HTTP-018 Error mapping and hiding of internal detail | changed | ERR-03, ERR-08, ERR-09 | The category alone sets the status; every 500 has a fixed body |
| HTTP-024 Query-string parameters | changed | PLATFORM-035 | Unknown parameters are refused (API-09). Parsing values arrives with the first route that takes one |
| OBS-001 One server span per request | kept | OBS-06 | |
| OBS-002 An inbound `traceparent` continues the trace | kept | PLATFORM-022 | |
| OBS-003 Client baggage becomes span attributes | dropped | | Identity on spans comes from the server (OBS-09) |
| OBS-004 Route templating, with `/<unmatched>` | changed | PLATFORM-034 | A request that matches no route has no `http.route`, as OpenTelemetry's conventions ask |
| OBS-005 Every envelope carries `meta.trace_id` | changed | API-07, API-16 | Problems carry `trace_id`, and every response carries `server-timing` |
| OBS-006 Zero sentinel when no span is active | dropped | | Every response the stack answers has a span; the probes carry no trace context (OBS-15) |
| OBS-007 Span status follows the status code | kept | OBS-06 | |
| OBS-008 One structured log per 4xx and 5xx | changed | PLATFORM-034 | One completion line per request, and an `ERROR` line for each 5xx (ERR-10) |
| OBS-009 No log line for 1xx to 3xx | changed | PLATFORM-034 | Every request writes its completion line (D10) |
| OBS-020 Console JSON lines with the span hierarchy | changed | OBS-02, OBS-05 | Flat lines with `trace_id` and `span_id`, and the service's name, version and environment |
| OBS-021 Level filters by environment and `RUST_LOG` | changed | OBS-03 | One setting, `LOG_LEVEL`, with the same default everywhere, never coarser than `INFO` |
| OBS-022 The logging of `migrate`'s command-line interface (CLI) | changed | OBS-01, OPS-03 | `migrate` logs through the logging setup every subcommand shares |
| OBS-024, OBS-025 Instrumentation and field conventions | changed | OBS-04, OBS-05 | One list of telemetry names, checked by `check-instrument` |
| RT-034 Process shutdown sequence | changed | PLATFORM-038 | The order of OPS-06, bounded by settings rather than by the container's grace period alone |
| OPS-010 Startup sequence | changed | OPS-03 | One startup-failure format, written by `main` before logging exists |
| OPS-013 Command-line dispatch | changed | ARCH-14 | As HTTP-001 |

## Platform: client IP, rate limits and request bodies

The old HTTP layer's behaviors that step 2.1 replaces. Path parameters and pagination (HTTP-025, HTTP-028, HTTP-029) are triaged with step 2.2, whose routes first take them.

| Old behavior | Verdict | New behavior | Reason for the change |
|---|---|---|---|
| HTTP-011 Client IP and User-Agent | changed | PLATFORM-047 | `X-Forwarded-For` only from a trusted proxy, read from the right, since the addresses on its left could come from anyone; `X-Real-IP` isn't read (SEC-12) |
| HTTP-019 Database error classification | changed | ERR-04, ERR-05 | As DATA-029 |
| HTTP-020 Repository errors expose their message and details | dropped | | A response comes from the error's code and category alone, and `detail` never carries internals (ERR-08, ERR-09) |
| HTTP-021 Service errors keep their identity through repository closures | changed | ERR-06 | Errors cross modules by conversion |
| HTTP-022 JSON request bodies | changed | PLATFORM-054, PLATFORM-055 | Unknown fields are refused (API-08); one problem code; a body that grows past the limit is 413 |
| HTTP-023 Field-validation error format | changed | PLATFORM-056 | API-07's `errors`, each with a JSON Pointer, a code and a message, all fields at once (API-10) |
| HTTP-026 Input normalization at deserialization | changed | ACCOUNTS-002, ACCOUNTS-003 | Each domain type normalizes its own value (D25) |
| HTTP-027 Shared validator rules | changed | AUTH-003, the `auth` spec's device IDs | Each rule lives in its domain type (D25); the others arrive with the routes that take their fields |
| HTTP-030 Rate-limit tiers, keys and exemptions | changed | PLATFORM-048, AUTH-006 | The IP limit counts every request but the health probes, authenticated ones too, before authentication, and the user limit after it (SEC-12) |
| HTTP-031 Fixed-window counter | kept | PLATFORM-049 | |
| HTTP-032 Rate-limit rejection | changed | PLATFORM-048, PLATFORM-050 | The problem `request.rate_limited`, with the headers of the IETF draft (API-15) |
| HTTP-033 Rate-limit headers on passing responses | changed | PLATFORM-050 | The draft's headers, for the bucket with the fewest requests left (API-15) |
| HTTP-034 Observability-only rate limiting | dropped | | A limit is always enforced, and a deployment that needs more raises it with its setting (D9, OPS-02) |
| HTTP-035 Rate limiter failures fail open | changed | the platform spec's Rate limits | A count that fails fails the request as an internal error: the database it couldn't reach is the one the request needs |
| HTTP-036 Rate limits hold across instances | kept | PLATFORM-049 | |
| HTTP-037 Email-keyed buckets never store the email | changed | PLATFORM-051, AUTH-006 | Every bucket's key is an HMAC, those of IP addresses and users too |
| HTTP-038 Expired rate-limit windows are purged | changed | the platform spec's Rate limits | A periodic job, built with jobs in step 2.3 (OPS-11) |
