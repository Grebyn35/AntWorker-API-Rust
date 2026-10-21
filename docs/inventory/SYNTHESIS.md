# Inventory synthesis

The eleven inventories list 489 behaviors and 144 open questions. Most of the questions are the same few problems seen from different domains. This file groups them into cross-cutting decisions. D1 to D16 resolve the open questions. D17 to D31 cover foundations the open questions don't reach (see [coverage.md](../coverage.md)). D32 to D36 settle questions from the standards review. Each one becomes a rule in the spec and is applied to every module at once, which is what stops the old pattern of fixing one place and leaving another inconsistent.

Each decision has a recommendation. Decisions the owner accepts become decision records (ADRs) in the spec. The questions they don't cover are settled while each domain's spec is written; they are listed at the end.

Reference format: `AUTH Q5` is open question 5 in `01-auth.md`, and `AUTH O5` is observation 5. See [README.md](README.md).

## What the inventory shows

1. **No rule decides when side effects happen.** Audit entries, SSE events, emails, provider-token revocation and object storage writes each run before, inside or after the main transaction, chosen case by case. Every domain has bugs that come from this.
2. **Authorization covers only part of the API.** Six permissions guard members, invitations, workspace settings and the audit log. Files have no permission checks at all. Member and invitation queries don't check that the target row belongs to the workspace in the request (confirmed in code: `workspace_member_repository.rs:341-348`, `:473-490`).
3. **Workspace context is split between a header and stored server state.** Requests name a workspace in `X-Workspace-Id`, but the server also stores an "active" workspace per user. The two disagree, and users can end up in a state where they belong to workspaces but log in to none.
4. **Access changes don't reach open connections.** Revoking a session, removing a member or deleting an account leaves existing SSE streams receiving events.
5. **Personal data is handled differently in every place it's stored.** Events, audit values and logs each follow their own conventions.
6. **Failures are often silent or misreported.** Empty configuration values count as set. A failed trace exporter yields all-zero trace IDs. A disabled email backend returns 500 or claims success. Readiness checks nothing.
7. **Documentation and comments disagree with code** in every domain. Each inventory lists the mismatches under Observations.

## Already decided

- "Functionality" means capabilities and guarantees, not the exact HTTP shape. The client will be rebuilt against the new API, so the wire contract may change. This settles part of `HTTP Q1` and `MAIL Q7`.
- Web clients are out of scope for now but must not be precluded. D18 lists what that requires (`HTTP Q3`).
- The observability stack is a permanent part of the codebase. Only its use is optional at runtime.

## Defaults applied without a decision

These follow from the goal of no known issues and no tech debt:

- Dead code, unreachable branches and invariants that only tests enforce are not ported (`AUDIT Q15`).
- Comments never restate history or contradict code. The spec and doc comments on public items are the documentation.
- No lossy numeric casts and no panics outside tests, enforced by clippy.

## Decisions

### D1. Side effects commit atomically with the change

**Evidence:** `AUDIT O3`, RT "No outbox", `MAIL O13`, `MAIL O15`, `USER O5`, `USER O10`, `USER O20`, `AUTH O16`, `AUTH O17`, `FILE O4`, `FILE O5`, `WS O4`.

**Recommendation:** a transactional outbox.
- Every state-changing operation runs in one database transaction. In that transaction it writes its audit entries, its SSE events and its outbound jobs (emails, provider-token revocation, object deletion).
- A dispatcher delivers outbound jobs at least once, with retry and backoff. Every consumer is idempotent.
- Nothing external happens before the commit, and the commit alone decides the response. A step after the commit can't turn a committed change into an error.
- Uploads write the object first and the row second. A periodic sweep removes objects that have no row.

**Alternative:** audit entries in the transaction, with events and email left best effort. This is simpler, but it keeps two of the three bug classes.

**Resolves:** `AUDIT Q3`, `AUDIT Q8`, `RT Q3`, `MAIL Q1`, `MAIL Q9`, `FILE Q5`.

### D2. Authorization model

**Evidence:** `03-workspaces.md` "Current permission model", `WS O1`, `WS O3`, `WS O18`, `FILE O1`, `AUDIT O7`, RT "Workspace events ignore workspace permissions".

**Recommendation:**
- Each membership has exactly one role. Roles and their permission sets are defined in code (for example `owner`, `admin`, `member`). Permissions are no longer copied onto each member, so adding a permission updates every member with that role.
- The permission catalog covers every workspace resource, including files and the audit log, with a separate permission for seeing IP addresses in audit entries.
- One policy module decides every workspace action. A workspace route can't compile without declaring its permission, enforced through an extractor type.
- Hierarchy: you can only change or remove members whose role ranks below yours, and only assign roles below your own. Each workspace has exactly one owner. Ownership transfer swaps the owner and admin roles.
- Events and audit reads are filtered by the same permissions.
- Custom per-workspace roles are not built now, but roles live in one table so they can be added later. That gets its own ADR.

The exact role set and permission matrix are decided in the `access` module spec.

**Resolves:** `WS Q6`, `WS Q7`, `WS Q8`, `WS Q9`, `WS Q11`, `WS Q12`, `WS Q13`, `WS Q15`, `FILE Q1`, `AUDIT Q5`, `RT Q7`, `DATA Q3`.

### D3. Tenancy scoping, with no server-side active workspace

**Evidence:** `WS O1` (confirmed), `03-workspaces.md` "Current active-workspace model", `AUTH O2`, `WS O4`, `WS O5`, `HTTP O4`.

**Recommendation:**
- The workspace goes in the path: `/v1/workspaces/{workspace_id}/...`.
- Every repository function for workspace data takes the workspace ID as a required parameter, and every query filters by it. A row ID alone never addresses workspace data.
- There is no stored "active" or "default" workspace on the server. Login returns the session and the user. The client lists its memberships with `GET /v1/workspaces` and remembers its last workspace per device. A member can read any workspace they belong to, within their permissions, without switching.

**Resolves:** `WS Q1`, `WS Q3`, `WS Q4`, `WS Q5`, `WS Q16`, `AUTH Q2`.

### D4. What row-level security is for

**Evidence:** `DATA O1` to `DATA O8`, `DATA O14`, `AUDIT O15`, `AUDIT O16`. Today it takes about 2,100 lines of stored procedures, `SECURITY DEFINER` bypasses and per-request database context, and it still checks only membership.

**Options:**
- (a) RLS as the main authorization layer, including permissions. This duplicates the D2 policy module in SQL.
- (b) RLS as a backstop for tenant isolation only.
  - Workspace tables get a uniform policy on `workspace_id`, and user tables a uniform policy on `user_id`.
  - Requests always run as `app`, with RLS on, including before sign-in. A lookup before sign-in puts the hash of the secret the request presents (an access or refresh token, the sign-in email, a verification code or a provider subject) into the RLS context, and the policy exposes only the matching row.
  - Only work that spans tenants by nature bypasses RLS: background jobs such as retention and dispatch, and the real-time hub loading each new event once for fan-out. It uses a `worker` role whose grants cover only the tables that work touches, and no route can reach it.
  - The server refuses to start if the request role can bypass RLS.
  - No business rules in SQL and no `SECURITY DEFINER` procedures. `EXECUTE` is revoked from `PUBLIC`.
- (c) No RLS. Tenant isolation comes from the scoped repository API in D3, plus tests.

**Recommendation:** (b). A query bug still can't leak another tenant's data, and the policies are simple enough for someone forking the template to understand in minutes. (c) is the simpler choice if you value developer experience over a second line of defense.

**Status:** accepted, option (b), on 2026-10-20.

**Resolves:** `DATA Q1`, `DATA Q2`.

### D5. Access changes take effect immediately

**Evidence:** RT "Open streams are never re-authorized", `AUTH O1`, `AUTH O5`, `AUTH O8`, `WS O19`, RT "Replay can deliver events from before the user joined".

**Recommendation:**
- When a session is revoked, a user is deleted, or a membership is removed or its role changes, the server closes or re-filters the affected SSE streams. The signal is a Postgres `NOTIFY` sent when the change commits, and every open stream also re-checks its access at a configured interval (D35).
- Replay delivers a workspace event only to users who were members when the event was created.
- Sessions slide on refresh but have an absolute maximum lifetime (configurable, for example 90 days).
- Logging in again on the same device goes through the full revocation path for the replaced session: provider token, event and audit entry.

**Resolves:** `AUTH Q1`, `AUTH Q5`, `AUTH Q6`, `RT Q1`, `RT Q2`.

### D6. Audit policy

**Evidence:** `05-audit.md` (all observations), `USER O7`, `USER O8`, `WS O9`, `FILE O3`.

**Recommendation:**
- Audit every successful state change, including file upload and delete, session creation and email-change requests.
- Also audit security failures: failed login, wrong current password, permission denied and refresh-token reuse.
- Entries are written in the change's transaction (D1). Workspace actions go to the workspace log, and account actions to the user log. The target of an action is a stored, filterable column, so a user can see what was done to them.
- Audit values hold IDs. Emails appear only as hashes, never in plain text.
- IP addresses and user agents are visible only with the dedicated permission from D2.
- Retention is a bounded setting. On account deletion, the user's identifying data in audit entries is anonymized rather than kept until retention runs out.

**Resolves:** `AUDIT Q1`, `AUDIT Q2`, `AUDIT Q4`, `AUDIT Q6`, `AUDIT Q7`, `USER Q9`, `WS Q14`, `FILE Q4`.

### D7. Privacy and data minimization

**Evidence:** RT "Payload privacy is inconsistent", `AUDIT O6`, `OBS O26`, `AUTH O3`, `USER O11`, `FILE O6`, `OBS O22`.

**Recommendation:**
- SSE events carry IDs and non-sensitive fields only. Clients fetch details over REST, which enforces permissions.
- Logs never contain personal data. Redaction is by type: wrapper types such as `Email` and `Secret` redact themselves when formatted. This replaces today's lint on field names.
- Account enumeration is accepted on registration and on the email-change pre-flight. Both are rate limited, and the trade-off is recorded in an ADR. It is never possible on login or forgot-password.
- The telemetry proxy enforces telemetry consent on the server.
- Public files never expose their original file name, on any route.

**Resolves:** `AUTH Q3`, `USER Q6`, `OBS Q9`, `OBS Q11`, `FILE Q6`.

### D8. API contract conventions

**Evidence:** `HTTP O1` to `HTTP O4`, `HTTP O12`, `HTTP O25` to `HTTP O33`, `AUDIT O12`, `AUDIT O24`, `DATA O22`.

**Recommendation:**
- Resource-oriented paths, with verbs used by meaning: `PATCH` for partial updates and `DELETE` for removal. Actions stay as named `POST` sub-resources (`/transfer`, `/accept`), used consistently.
- `201 Created` for creates and `204 No Content` for deletes.
- Errors follow RFC 9457 (Problem Details), plus a stable machine-readable `code` and the `trace_id`. Validation, business-rule, conflict, not-found, rate-limit and feature-disabled errors each get their own code. No internal details ever reach the wire. Constraint violations map to 4xx, not 500.
- Success bodies are the resource itself. Lists share one shape: `items` plus an opaque `next_cursor`. Cursors keep full timestamp precision.
- Unknown query and body fields are rejected.
- Every oversized body gets the same 413 problem response.
- The OpenAPI document is generated from code and committed, and CI fails on drift. It is the contract the new client is built against.

**Resolves:** `HTTP Q1`, `HTTP Q7`, `HTTP Q10`, `HTTP Q11`, `HTTP Q12`, `AUTH Q7`, `AUDIT Q12`, `AUDIT Q14`, `DATA Q9`.

### D9. Strict configuration and honest failure

**Evidence:** `OPS O1` to `OPS O6`, `MAIL O5`, `MAIL O8`, `MAIL O24`, `HTTP O35`, `AUDIT O9`, `FILE O12`, RT "Listener death has no recovery or health signal".

**Recommendation:**
- Configuration is parsed once into typed values. Values are trimmed, an empty value means unset, and numbers are range-checked. Unknown enum values are errors.
- `ENVIRONMENT` is required and has no default.
- Every startup failure prints one format and exits non-zero, with no panics.
- A variable exists only if someone deploying a fork plausibly needs to change it. Everything else is a constant in code. Today there are 90 variables; the target is far fewer.
- A disabled feature answers 503 with a `feature_disabled` code, and a public capabilities endpoint advertises which features are on. No endpoint reports success for something that can't happen; forgot-password with email disabled answers 503.
- Readiness reflects the dependencies the instance needs to serve: the database and the event listener. Liveness only means the process is up.

**Resolves:** `OPS Q1`, `OPS Q2`, `OPS Q3`, `MAIL Q2`, `MAIL Q3`, `MAIL Q6`, `MAIL Q8`, `USER Q10`, `HTTP Q8`, `HTTP Q15`, `AUDIT Q9`, `DATA Q6`, `RT Q5`, `FILE Q10`.

### D10. The trace-ID guarantee

**Evidence:** `10-observability.md` "Trace-ID journey", `OBS O1` to `OBS O9`, `OBS O16`, `OBS O27`.

**The guarantee:** any trace ID the API returns to a client leads to that request's logs, and to its trace whenever the request ended in an error.

**Recommendation:**
- Every response, including streaming and raw downloads, carries the trace ID in a header, and every error body includes it.
- The SDK samples every trace. Tail sampling in the collector keeps every trace with a status of 400 or higher, plus a configurable share of successes. 4xx responses get an attribute the sampler can match.
- Logs are never sampled, and every log line carries the trace and span IDs.
- Grafana links both ways: logs to traces through a Loki derived field, and traces to logs. Latency metrics emit exemplars.
- Outbox rows (D1) store the originating trace context. Deliveries and background jobs start spans linked to it.
- Outbound calls get client spans. Trace context is sent only to our own infrastructure, never to third parties such as OAuth providers.
- In `opentelemetry` mode, an exporter that fails to build fails startup.
- Identity attributes on spans come from the authenticated context, never from client-supplied baggage.
- Phase 6 checks the guarantee end to end: trigger a 4xx and a 5xx, copy the trace ID, find the trace and the logs.

**Resolves:** `OBS Q1` to `OBS Q8`, `HTTP Q14`.

### D11. Abuse controls and rate limiting

**Evidence:** `HTTP O5` to `HTTP O9`, `FILE O15`, `AUTH O11`, `USER O1`, `MAIL O23`, `OPS O22`, `OBS O18`.

**Recommendation:**
- An IP-based limit applies to every route before authentication, so invalid tokens are throttled too. A per-user limit applies after authentication.
- Anonymous public-file downloads get their own bucket.
- Per-action limits apply to anything that sends email or checks a secret. All checks of the current password share one bucket per user.
- Each user can hold a capped number of concurrent SSE streams.
- The client IP comes from `X-Forwarded-For` only when the request arrives from an explicitly configured trusted proxy. Caddy sets security headers and its trusted proxies explicitly.
- Rate-limit headers follow the IETF `RateLimit` draft and describe the bucket that decided the response.

**Resolves:** `HTTP Q2`, `HTTP Q9`, `HTTP Q13`, `AUTH Q10`, `USER Q3`, `MAIL Q5`, `RT Q6`, `FILE Q9`, `OPS Q13`.

### D12. Idempotency keys

**Evidence:** `HTTP O15` to `HTTP O23`, `AUTH O23`, `FILE O16`.

**Recommendation:**
- Keep `Idempotency-Key`, following the IETF draft, for non-idempotent `POST`s only: creates, invitations and uploads. It is not used on auth routes that return tokens.
- The fingerprint covers the method, route, query and a body digest. Keys are scoped to the authenticated principal and compared exactly.
- TTL is enforced when an entry is read, not only by a cleanup sweep.
- An oversized body gets a 413 before anything is buffered.

**Resolves:** `HTTP Q4`, `HTTP Q5`, `HTTP Q6`, `AUTH Q13`, `FILE Q11`.

### D13. Background jobs and multiple instances

**Evidence:** RT "'Only one instance per cycle' is wrong", RT "Lock TTL versus long-running tasks", `FILE O13`, `FILE O14`, `OPS O7`, `HTTP O36`.

**Recommendation:**
- Each job belongs to the domain that owns its data, and all jobs are declared in one registry.
- Each job runs once per interval across the whole fleet. Its schedule row is claimed with `FOR UPDATE SKIP LOCKED`, and a long run extends its lease with a heartbeat.
- The outbox dispatcher from D1 is one of these jobs.
- Every instance runs the scheduler, and the leases make that safe, so there is no per-instance flag to disable jobs.
- Shutdown fits inside the container's grace period: tasks are cancelled and their leases released. Rows that keep failing go to a dead-letter state that is visible in metrics and has an alert.

**Resolves:** `RT Q8`, `RT Q9`, `RT Q10`, `RT Q11`, `OPS Q4`, `FILE Q7`, `AUTH Q12`.

### D14. Scope of field-level encryption

**Evidence:** `DATA O15` to `DATA O19`, `AUDIT O8`. The encryption module is about 2,500 lines plus the re-encryption procedures. Encrypted columns can't be searched or sorted in SQL, emails need separate lookup hashes, and those hashes break during key rotation.

**Options:**
- (a) Keep application-level AES-GCM on personal-data columns, and fix the gaps: associated data, lookup-hash rotation and reliable rotation reporting.
- (b) Encrypt only secrets, such as OAuth provider tokens, at the application level. Rely on storage-level encryption (disk, managed database, S3 server-side encryption) for everything else.
- (c) No application-level encryption.

**Recommendation:** (b). Application-level encryption of names and emails protects little, because the running application holds the key. It costs a lot for every developer who adds a column. Encrypting third-party credentials is the industry norm, and storage-level encryption covers backups and disks. This drops an existing guarantee, so it is your call.

**GDPR.** Article 32 asks for security appropriate to the risk and names encryption and pseudonymisation as examples. It does not require application-level column encryption. What the template provides instead:
- TLS in transit everywhere.
- Encryption at rest at the storage layer, stated as a deployment requirement: encrypted volumes, or a managed database and S3 with encryption enabled.
- Data minimization (D7).
- Erasure on account deletion, including anonymized audit entries (D6).
- Bounded retention (D6, D9).

The rights of access and portability are covered by the data export (D31). Compliance also depends on the operator's own processes: records of processing, processor agreements and breach handling. The template can document these but cannot carry them out.

This is an engineering reading, not legal advice. A real deployment still needs a privacy review.

**Status:** accepted, option (b), on 2026-10-20.

**Resolves:** `DATA Q4`, `DATA Q5`, `FILE Q12`.

### D15. Delivery pipeline and deploy scope

**Evidence:** `OPS O10` to `OPS O19`, `OPS O21`, `OBS O23`.

**Recommendation:**
- CI builds the image once, tests it, scans it and publishes that same image digest. Each architecture is built once and booted at least once.
- Live tests apply migrations through the production migrator.
- The required checks are documented and applied as branch protection.
- `vX.Y.Z` tags publish releases. `vX.Y.Z-rc.N` tags publish prerelease images only.
- There is no coverage threshold. The mapping from behavior IDs to tests is the coverage measure, because a coverage gate rewards exactly the "because a reviewer said so" tests the old repo accumulated.
- Migrations follow expand and contract, so the previous release keeps working during a rolling deploy.
- Alerts don't depend on the deploy tooling.
- The API container receives only the variables it reads.
- Windows is a supported development platform.
- Deploy tooling: keep a single-host rolling deploy as a template feature, rebuilt smaller and tested in CI against the real image. Whether that's wanted, versus shipping Compose plus documented commands and leaving orchestration to the fork's platform, is your call.

**Status:** accepted on 2026-10-20, including the single-host rolling deploy. D21 sets out the pipeline in detail.

**Resolves:** `OPS Q5` to `OPS Q9`, `OPS Q11`, `OPS Q12`, `DATA Q8`, `OBS Q13`.

### D16. Account and workspace lifecycle

**Evidence:** `USER O2`, `WS O3`, `WS O18`, `DATA O11`.

**Recommendation:**
- A user can't delete their account while they own a workspace that has other members. They must transfer or delete it first. Workspaces where they are the only member are deleted with the account.
- Deletion is immediate, with no grace period. That gets its own ADR.
- Registration creates no workspace (as today). The client offers to create one or accept an invitation.
- Accepting an invitation while already a member returns 409 and changes nothing.
- After a transfer, the former owner becomes an admin.

**Resolves:** `USER Q1`, `USER Q2`, `WS Q2`, `DATA Q7`, `DATA Q12`.

### D17. Internal error model

D8 fixes what errors look like on the wire. This decision fixes how errors move through the code, which is where consistency is won or lost.

**Evidence:** eight error types across about 7,300 lines in `src/errors/`. Repository-origin errors leak their messages (HTTP-020). Service errors survive only through special closures (HTTP-021). Business rules share one code with malformed input (`HTTP O27`). Every conflict says "Resource already exists" (`HTTP O26`). Constraint violations become 500 (`DATA O22`). Fail-open logging is inconsistent (`AUTH O24`). The key ring panics at startup (`OPS O4`).

**Recommendation:**
- Each module defines one error enum for its own failures, named in domain terms (for example `LastOwner` or `InvitationExpired`). A module never returns another module's error type; it converts at the call site.
- Infrastructure errors (`sqlx`, S3, SMTP) never leave the adapter that produced them. The adapter turns them into a domain outcome (a unique violation becomes `AlreadyExists`, a missing row becomes `None`) or into an opaque internal error that carries the cause for the logs only.
- Every module error declares a stable wire `code` and one category. The category alone sets the HTTP status, from a small fixed set: validation, unauthenticated, forbidden, not found, conflict, rate limited, payload too large, unavailable and internal. The final list is part of the error standard.
- One conversion turns any error into the RFC 9457 response (D8). The error code catalog in the OpenAPI document and the docs is generated from the enums, so it can't drift.
- An error is logged once, where it is handled, never where it is only passed on. Internal errors are logged at ERROR with the full cause chain and the trace ID. Expected client errors are not logged at ERROR.
- No `unwrap`, `expect` or panics outside tests, enforced by clippy. A catch-panic layer remains as the last line of defense.
- Deliberately ignoring an error is always explicit, through one helper that logs a standard shape and increments a metric. D1 removes most of today's fail-open sites.

**Why this stays consistent:** the types make the inconsistent version hard to write. A handler can't return a database error, there is only one route to an HTTP response, the category fixes the status, and the catalog is generated. Each rule also gets an ID that conformance reviews check.

**Alternative:** one global error enum, close to today's approach. It is simpler at first, but it grows into one large file that couples every module to every other.

### D18. Authentication that is ready for web clients

**Evidence:** sessions require `Authorization: Bearer` (AUTH-019). Sessions are unique per `(user_id, device_fingerprint)`, and every login requires a fingerprint (AUTH-001, AUTH-004, AUTH-007, AUTH-018). CORS blocks the custom headers (`HTTP O10`). OAuth already uses PKCE with a client-supplied redirect URI (AUTH-007), which suits browsers.

**What a browser needs that the desktop doesn't:** refresh tokens in `HttpOnly` cookies rather than script-readable storage, CSRF protection once cookies authenticate, CORS for its origin, and a way to authenticate `EventSource` streams and `<img>` downloads, neither of which can send an `Authorization` header.

**Recommendation, built now because it costs little:**
- Credentials are read through one extractor with a pluggable source. Today the only source is the Bearer header; a cookie source can be added later without touching any handler. SSE and file downloads use the same extractor, so cookies will cover them too.
- Login and refresh responses come from one function that issues the session. A later cookie mode is one new branch in that function.
- A device identifier is optional. When the client sends one (the desktop does), a re-login from that device replaces its session (D5). When none is sent, the login creates a new session.
- OAuth redirect URIs are checked against a configured allowlist.

**Built later, as additions rather than changes:** the cookie transport, CSRF protection, a CORS allowlist for web origins, and signed URLs for private downloads.

### D19. Observability stack design

D10 fixes the trace-ID guarantee. This decision fixes how the rest of the stack is designed, so it stops feeling messy.

**Evidence:** 11 monitoring containers, 6 dashboards, 32 alerts and 10 API metrics. One dashboard ("App Events") shows a single panel of signups. Dashboards can be edited in the UI. Some alerts depend on a rule file that only `deploy.sh` generates (`OBS O23`). Comments and docs disagree with the configuration (`OBS O10`, `OBS O12`, `OBS O29`).

**Why it feels messy:** it was built tool first, one dashboard or alert at a time, with nothing tying the pieces to a purpose.

**Recommendation: design from questions, not from tools.**
1. Write down the questions an operator must be able to answer:
   - Is the API up?
   - Which routes are slow or failing, and since when?
   - Why did this request fail? (D10)
   - Is a dependency degrading: Postgres, S3, SMTP, the collector?
   - Are background jobs and the outbox healthy (lag, dead letters)?
   - Are real-time events being delivered?
   - Is the desktop client healthy?
   - Is the telemetry pipeline itself dropping data?
2. Every metric is declared in code with one declaration macro, giving its name, type, unit, labels and description and following one pinned version of the OpenTelemetry semantic conventions. A catalog generated from the declarations lists every metric.
3. Dashboards and alerts come from the questions and the catalog:
   - One overview dashboard covering golden signals, dependencies, jobs and events, with a drill-down per area. Every panel answers a named question.
   - Alerts fire on symptoms against service-level objectives (burn rate for availability and latency), plus a few cause alerts for dependencies and dead letters.
   - Every alert has a runbook entry and a `promtool` test. The target is well under 32 alerts.
4. Everything is code. Dashboards are provisioned read-only. CI fails if a dashboard or alert references a metric missing from the catalog, or if a catalog metric is never used.
5. One topology for development and production: the app sends to the OpenTelemetry Collector, which feeds Prometheus, Loki and Tempo, with Grafana and Alertmanager on top. The Grafana stack is kept; it is the open-source standard and already in place. Which smaller parts stay (the blackbox prober, the Caddy log pipeline) is recorded in `docs/operations/observability.md`.
6. The observability docs are written for someone who has never used these tools, and include a walkthrough of finding a trace from an error.

### D20. Runtime topology and scaling

**Evidence:** Caddy balances round robin across replicas and runs no active health checks (OPS-034). There is no request timeout and no concurrency limit (`08-http-platform.md`, "What does not exist"). Readiness checks nothing (`HTTP O35`). Shutdown exceeds the container grace period (`OPS O7`). SSE fetches each event's payload once per open stream (RT "Payload fetch fans out to the database"). The pool-sizing formula in the docs is wrong (RT "Pool-sizing formula versus code").

**Recommendation:**
- **Horizontal:** the API stays stateless apart from open SSE connections, so any replica can serve any request and no sticky sessions are needed. Postgres is the only coordination point (rate limits, idempotency, leases, the outbox, `LISTEN/NOTIFY`), so scaling out means adding replicas behind the proxy. The docs give the connection budget: replicas × (pool size + 1 listener) must fit within the database's `max_connections`. They also note that `LISTEN` needs a direct connection; it does not work through PgBouncer in transaction mode. Startup logs the computed budget.
- **Event fan-out:** each instance fetches an event's payload once and hands it to every matching stream.
- **Vertical:** a request timeout plus a concurrency limit with load shedding (503 with `Retry-After`), so an overloaded instance degrades predictably. The database pool and runtime threads come from configuration with documented defaults. Compose sets memory and CPU limits.
- **Health:** liveness and readiness as in D9. Caddy checks readiness actively, so an instance that isn't ready gets no traffic. On shutdown, readiness flips first, the instance waits one health-check interval, then drains, all within the grace period.
- **Single host:** Caddy plus N replicas with the rolling deploy (D15).
- **Multiple hosts:** the image and its environment contract don't depend on a platform. The docs state the requirements (shared Postgres and S3, no sticky sessions) so a fork can run on Kubernetes, ECS or Fly. No manifests for those platforms ship with the template; that gets an ADR.
- **Measured capacity:** a load-test script records requests per second per replica at the latency target, so capacity is a documented number instead of a guess.

### D21. CI pipeline shape

This elaborates the CI part of D15.

**Evidence:** OPS-057 to OPS-071 and `OPS O10` to `OPS O17`. The old pipeline checks most of the right things: lint, advisories, shellcheck, alert-rule tests, telemetry contract tests, live tests, image boot, deploy scenarios, CVE scans, weekly rescans and dependency updates. Its problems are structural. It is a single 980-line workflow. It rebuilds the image three times, so the published image isn't the tested one. It relies on tools from the runner image, and it tests the deploy script against stub images.

**Recommendation:**
- One task runner (I'd use `just`) defines every check. CI only calls those tasks, so `just ci` on a laptop runs exactly what CI runs.
- Every pull request runs five stages, in order:
  1. Static checks: formatting, clippy with warnings denied, `cargo-deny` (advisories, licenses, bans, sources), shellcheck, OpenAPI drift, and the metric-catalog sync checks.
  2. Tests: unit tests, then integration tests against real Postgres and S3 service containers, with migrations applied by the production migrator.
  3. Build: one multi-architecture build with a layer cache, pushed by digest.
  4. Verify that digest: CVE scan and SBOM, boot each architecture, then run the end-to-end suite against the running stack. The suite covers the trace-ID guarantee, the alert-rule tests and the deploy-script scenarios on the real image.
  5. Promote that digest. On `main` it becomes `edge`. A `vX.Y.Z` tag retags the same digest, signs it (cosign, keyless) and attaches build provenance. Nothing is rebuilt at release.
- Workflows are split by purpose: pull requests and `main`, release tags, and scheduled jobs (weekly rescan and pruning), plus Renovate or Dependabot configuration.
- Everything is pinned: the toolchain from `rust-toolchain.toml` in every job, actions by SHA, images by digest, and the runner by version.
- The required checks are listed in the docs and applied as branch protection by a script, since branch protection lives outside the repo.
- The CI platform is GitHub Actions, which fits the repo's GitHub hosting, GHCR, keyless signing through OIDC, and Renovate or Dependabot. Since the workflows only call `just` tasks, moving to another CI service would mean rewriting only that thin layer.

## Foundations from the coverage check

[coverage.md](../coverage.md) checks the decisions against fixed references. These are the areas it found undecided.

### D22. Module architecture

**Evidence:** the old layout is horizontal (`handlers/`, `services/`, `repositories/`, `models/`), so adding a feature touches about six directories. `services/traits.rs` (1,445 lines) and `repositories/traits.rs` (2,026 lines) exist mostly so services can be mocked. The `public_*` repositories are grouped by how they bypass RLS rather than by domain.

**Recommendation:**
- A Cargo workspace with two crates:
  - `platform` is the shared core: configuration, database and transactions, HTTP plumbing, the auth extractor, error conversion, telemetry, the outbox, jobs, and the storage and email adapters.
  - `app` holds the feature modules, the routes and the composition root.
  - Cargo prevents `platform` from depending on `app`, so the core never learns about features.
- One directory per feature module inside `app`. The exact cut is decided in the architecture document. Each module has the same files, as needed: `routes.rs`, `service.rs`, `repo.rs`, `model.rs`, `error.rs` and `events.rs`.
- Only a module's `mod.rs` is visible to other modules. `repo` is private, so no module can query another module's tables, and Rust visibility enforces that at compile time.
- Layers inside a module:
  - The route handler extracts, authorizes, calls the service and shapes the response. It contains no SQL and no business rules.
  - The service holds the business rules and the transaction boundary.
  - The repository holds SQL only, with no rules.
- One composition root builds every service at startup. Traits exist only where there are two real implementations (object storage, email transport, clock), never just for mocking. Tests use the real database (D23).
- Tasks that run application logic against the database (migrate, seed) are subcommands of the same binary. Backup and restore use the standard PostgreSQL tools.

**Alternative:** one crate per module. It gives the strongest isolation, but adds boilerplate for every fork that adds a feature.

### D23. Testing strategy

**Evidence:** tests were written because reviewers asked for them. Mock-based tests exercise branches production can't reach (`AUTH O10`, `USER O18`). Most file tests bypass RLS (`FILE O24`). There is no end-to-end SSE test (RT "No end-to-end SSE test"). Live tests apply migrations differently from production (`OPS O10`).

**Recommendation:**
- A test exists to prove a behavior ID or a rule ID, and is named after it. A reviewer who thinks a test is missing must name the behavior or rule it would prove, or propose a new one.
- Three levels:
  - Unit tests for pure logic (domain types, the permission matrix, cursors), with no mocks of our own code.
  - Integration tests through HTTP against real Postgres and S3. This is the main level.
  - End-to-end tests against the built image for the cross-cutting guarantees: trace IDs, deploys and alerts (D21).
- Each test gets its own database, cloned from a migrated template database, so tests stay fast. Tests run as the real `app` role so RLS applies. The clock can be injected, and tests never sleep.
- A script checks the mapping both ways: every kept behavior has at least one test, and every test names an existing ID. That mapping is the coverage measure (D15).

### D24. Data access conventions

**Evidence:**
- 328 `Uuid::new_v4` calls. Random IDs fragment indexes.
- Timestamps come from the app clock (`AUDIT O13`), mixed with SQL `NOW()`, and `updated_at` is not maintained (`DATA O23`).
- 22 files use runtime-checked SQL. None use compile-time checked queries.
- Constraint violations become 500 (`DATA O22`).
- Business logic lives in stored procedures (D4).

**Recommendation:**
- IDs are UUIDv7: time-ordered, index-friendly and safe to expose. Postgres 18's `uuidv7()` generates them as column defaults.
- Persisted timestamps come from the database clock (`now()`), stored as `timestamptz` at full precision. Expiry checks run in SQL against `now()`. The app clock is used only for values that are never persisted.
- SQL is checked at compile time with `sqlx::query!`. The offline query cache is committed, so CI builds without a database.
- One transaction API opens each unit of work. It sets the RLS context (D4) and collects the outbox writes (D1), so neither can be forgotten.
- Optimistic concurrency on editable resources. Responses carry an `ETag`, and a client that sends `If-Match` gets 412 instead of overwriting a newer change. Clients that don't send it keep today's last-write-wins.
- Hard deletes, with cascades declared in the schema and no soft delete (recorded as an ADR).
- Migrations are forward-only and follow expand and contract (D15). The rewrite starts from one fresh baseline migration.
- Naming of tables, indexes and constraints is part of the standards.

### D25. Domain types and input validation

**Evidence:** validation is spread across the extractors, `validation/mod.rs` (1,422 lines) and the services. OAuth names and emails skip validation (`AUTH O20`). Device fingerprints are stored in mixed case (`AUTH O21`). Permission strings are stored non-canonically (`WS O7`). The same rules are checked in both SQL and Rust (`DATA O13`).

**Recommendation:**
- Parse, don't validate. Request bodies deserialize into request types that reject unknown fields, then convert at the boundary into domain types such as `Email`, `DisplayName`, `WorkspaceName`, `Password` and `FileName`.
- Each type's constructor trims, normalizes and checks limits. For example, emails are lowercased and names are put in Unicode NFC form.
- Code past the boundary accepts only domain types. A value can't skip validation, whatever its source: HTTP, an OAuth provider or configuration.
- Each limit is defined once, on its type, and appears in the OpenAPI schema.
- A validation failure lists every invalid field, each with its own error code (D8).
- The same mechanism provides the redacting `Email` and `Secret` types (D7).

### D26. Secrets and security baseline

**Evidence:**
- The api container receives all of `.env` (`OPS O21`).
- The key ring panics at startup (`OPS O4`).
- Verification-code hashes aren't compared in constant time (`AUTH O12`).
- The OAuth avatar fetch follows redirects with no host allowlist (`FILE Q14`).
- There is no threat model and no `SECURITY.md`.
- Caddy sets no security headers (`OPS O22`).

**Recommendation:**
- Secrets arrive as environment variables or as `*_FILE` variables pointing at mounted secret files, the Docker and Kubernetes convention. They are held in the `Secret` type, so they are never logged and never baked into images.
- Passwords are hashed with Argon2id at current OWASP parameters, in PHC format so parameters can be raised later. A login rehashes passwords stored with older parameters.
- Tokens are 256-bit random values stored as keyed hashes. Every comparison involving a secret is constant-time.
- Every outbound request to a URL supplied by a third party, such as the OAuth avatar, goes through one client. That client allows HTTPS only, blocks private and loopback addresses, re-checks every redirect, and limits size and time.
- API responses carry security headers. Caddy handles TLS and sets HSTS.
- A short threat model covers assets, actors and trust boundaries, with each mitigation mapped to a rule ID. A `SECURITY.md` states the disclosure policy.
- OWASP ASVS Level 2 is the security bar. Phase 6 includes a conformance pass against it, with each requirement mapped to a rule ID or marked not applicable.

**Resolves:** `FILE Q14`.

### D27. Documentation and Claude Code guidance

**Evidence:** the old API repo had no `CLAUDE.md`. Docs and comments disagree with code in every domain, and each Claude session invented its own standard.

**Recommendation:**
- A closed set of documents, each with one job. Nothing else counts as documentation. DOC-08 in `docs/standards/docs.md` holds the list:
  - `README.md`, `CLAUDE.md`, `SECURITY.md` and `CHANGELOG.md` at the root.
  - `docs/architecture.md`, including the glossary, and `docs/coverage.md`.
  - `docs/standards/`: the rules, each with an ID.
  - `docs/spec/`: one file per module, listing its behaviors with IDs.
  - `docs/adr/`: decision records.
  - `docs/operations/`: CI, deploy, backups, observability, the metric catalog, runbooks and the threat model.
  - `docs/guides/`: how to add a module and how to rename the fork.
  - `.claude/`: the Claude Code agent and command definitions.
- `CLAUDE.md` at the root stays short. It points to the architecture and standards and states the working rules:
  - Spec first.
  - Cite a rule ID or behavior ID for every finding.
  - No open-ended evaluations.
  - Tests are named by ID.
  - New rules are proposed, not invented.
  - Docs and comments describe the current state only.
- The conformance review is a checked-in subagent definition with a fixed prompt. Every review uses the same procedure, whether you run it, a fork does, or a future model does.
- **Current state only.** This rule applies to every document and every comment:
  - They describe the system as it is. They never record what changed, what something replaced, what an earlier version did, who asked for it, or when.
  - An edit rewrites the affected text so it reads as if it had always said that. There are no "update", "note", "now", "no longer" or "previously" remarks and no dated notes.
  - History has exactly three homes. Git holds commit messages and pull request descriptions. ADRs hold the context of a decision; a later ADR supersedes an earlier one rather than editing it. `CHANGELOG.md` holds the release notes for forks (D29).
  - Outside `docs/inventory/`, nothing refers to the old API. When the migration is complete, `docs/inventory/` is deleted.
- **Comments:**
  - A doc comment on a public item states what the item does and its contract: inputs, outputs, errors and invariants.
  - An inline comment appears only where the code can't show something the reader needs, such as an invariant, a non-obvious constraint, or why the obvious approach is wrong. It says that and nothing else.
  - A comment that restates the code is deleted.
  - There is no reviewer voice ("industry standard", "SOC 2 requires", "security fix") and no `TODO` or `FIXME`. Known work is tracked as an issue, not in the code.
- CI checks links, checks that every rule ID cited in the docs exists, and flags the most common history markers in docs and comments: `TODO`, `FIXME`, "previously", "no longer", "used to", "legacy", and dates. Review covers the rest.

Forks inherit all of this, so the template is ready for Claude Code from the first commit.

### D28. Local development experience

**Evidence:** there is no development email transport and no mail catcher (`MAIL O6`). Setup runs through `scripts/init.sh` and several Compose profiles. Windows needs special build settings (OPS-020).

**Recommendation:**
- From clone to running in two commands:
  - `just setup` checks the required tools and writes `.env` with generated development secrets.
  - `just dev` starts Postgres, SeaweedFS and Mailpit in Compose, then runs the API with auto-reload.
- Mailpit catches all development email and shows it in a web UI.
- `just seed` creates demo users and workspaces.
- The observability profile is one flag away and uses the same topology as production (D19).
- The OpenAPI UI is served in development only.
- Windows, macOS and Linux are all supported (D15).

### D29. Fork and extension model

**Evidence:** the old repo has no story for forking. The product name is scattered across code, images and configs (`app-api`, `app-desktop`, and `APP_DISPLAY_NAME`, which only 2 of 9 email templates use: `MAIL O20`). There is no guide for adding a feature.

**Recommendation:**
- The repo is published as a GitHub template repository, so a fork starts with clean history.
- `just rename <name>` rewrites the product name everywhere: crates, the binary, the image, service names, telemetry service names and email branding.
- Adding a feature:
  - A guide walks through copying the golden module (phase 4).
  - `just new-module <name>` scaffolds the module's files, migration, route registration, permissions and test file.
- Removing a feature: each OAuth provider is one file plus one registry line, and the client telemetry module is registered only in the composition root. A fork removes one by deleting it and its registration. The core modules, including files, are not removable, because avatars and workspace pictures depend on files. There are no Cargo feature flags for modules.
- Updates: forks diverge by design. Each template release ships a changelog with upgrade notes, and security fixes are flagged, so forks can port them by hand.

### D30. Backups and recovery

**Evidence:** nothing in the old repo covers backup or restore. `admin.sh reset-db` can destroy the bundled database, but nothing can restore it.

**Recommendation:**
- The bundled Postgres gets a scheduled backup to the object store, with retention. `pg_dump` is the simple baseline; WAL archiving adds point-in-time recovery. The tool is recorded in `docs/operations/backups.md`.
- `just restore` restores a backup into a fresh database.
- CI restores a backup and runs the smoke tests against it, because an untested backup is not a backup.
- With managed Postgres, the docs require point-in-time recovery from the provider.
- Bucket versioning for object storage is recommended and documented.
- The encryption key for secrets (D14) is backed up separately. Losing it means users must re-link their OAuth providers, and the docs say so.
- The reference deployment states its recovery targets: how much data can be lost, and how long a restore takes.

### D31. Personal data contract: export and erasure

A capability beyond the old API's functionality (owner question Q2). GDPR gives users the right to a copy of their data in a machine-readable format (Articles 15 and 20).

**Recommendation:**
- Every module that stores personal data implements one contract. It has three parts:
  - export the user's data, as JSON plus any files
  - erase it, by deleting or anonymizing
  - optionally, a precondition for erasure, such as "transfer or delete shared workspaces first" (D16)
- The composition root registers every implementation. That lets account deletion and export run across all modules without the accounts module depending on any of them.
- A CI check fails if any column referencing a user is not covered by a registered contract. A fork that adds a table with personal data can't forget export or erasure.
- Export is asynchronous:
  - `POST /v1/users/me/exports` starts a job, limited to one per 24 hours.
  - The job collects every module's part into one archive and stores it in object storage, where it expires after a short time.
  - The user is notified by email and SSE and downloads the archive through an authenticated endpoint.
  - Both the request and the download are audited.
- Scope: the user's own data. That means their profile, identities, sessions, memberships, own audit log and own files. Other members' data and whole-workspace content are excluded. An export of a whole workspace for its owner would be a separate future feature.
- Account deletion uses the same contract, so export and erasure can't drift apart.

## Decisions from the standards review

The standards review settled these five questions after its fourth pass. Each was tested against PostgreSQL 18.6 where it concerned database behavior.

### D32. Lookups during a key rotation

A key presented before sign-in, and a user's own email hash, are matched under every active key version: the context holds the HMAC under each version, and the policy matches any of them. Tested with 100,000 users stored under two key versions: the right row is found, through index scans. See DATA-15 and DATA-17.

**Status:** accepted on 2026-10-21.

### D33. Creating a workspace

`platform` generates a new workspace's ID before the insert, and the creating unit of work runs in that workspace's context, so the workspace row and the owner's membership pass RLS. This is the one exception to "the database generates IDs". Tested: a database-generated ID fails the policy, and a pre-generated ID passes. See DATA-08 and DATA-16.

**Status:** accepted on 2026-10-21.

### D34. Seeing and accepting invitations

A user's context carries their email hash, and invitations have an "addressed to the caller" policy, so an invitee sees exactly their own invitations in every workspace. Accepting one moves the unit of work into the invitation's workspace through an `Admission` that only `invitations` can build. Tested: the invitee sees only their invitations, through an index, and joining succeeds only after the move, inside one transaction. See DATA-15, DATA-16 and DATA-22.

**Status:** accepted on 2026-10-21.

### D35. Access-change signals

Access-change signals are sent with Postgres `NOTIFY` at commit and are never stored, so erasing a user can't remove the signal that closes their streams. Every open stream is also re-checked at a configured interval, 60 seconds by default, in case a signal is missed. Lost access ends at once, or within that interval in the worst case. See SEC-06.

**Alternative considered:** storing access changes briefly in a table without foreign keys.

**Status:** accepted on 2026-10-21.

### D36. The commit decides the outcome

A committed unit of work answers with what it recorded: a security failure recorded during sign-in answers with that failure, and any other commit answers with success. See ARCH-11.

**Status:** accepted on 2026-10-21.

## Questions left for the domain specs

These are product details local to one domain. Each is settled while that domain's spec is written, usually with a recommended default.

| Domain | Questions |
|---|---|
| AUTH | Q4 (verification gate by account), Q8 (reset marks email verified), Q9 (idempotent credential unlink), Q11 (unknown OAuth method on unlink) |
| USER | Q4 (email-change sessions, notice and cancel), Q5 (unlink cancels pending email change), Q7 (avatar MIME and size), Q8 (avatar deletion through files), Q11 (password reuse, pending reset codes), Q12 (consent upsert) |
| WS | Q10 (expired invitations) |
| FILE | Q2 (MIME policy for public files), Q3 (hide avatars and pictures from generic endpoints), Q8 (Range and streaming), Q13 (quotas), Q14 (OAuth avatar redirects) |
| AUDIT | Q10 (re-invite as update), Q11 (transfer action strings), Q13 (schema endpoint) |
| RT | Q4 (resync signal) |
| MAIL | Q4 (which notifications exist), Q7 (copy and labels), Q10 (localization, links, support contact) |
| DATA | Q10 (database-enforced image pointers), Q11 (single writer of `email_verified`) |
| OBS | Q10 (relayed collector 5xx), Q12 (Tempo cross-tenant merge, needs a live check) |
| OPS | Q10 (Compose guards on inactive profiles, needs a live check) |
