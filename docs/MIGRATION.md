# Migration tracker

Tracks the rewrite from `AntWorker-API` (commit `0210895`) to this repository. It holds status and pointers only (DOC-16). Every other fact lives in its home (DOC-14).

## Current work

**Next:** step 1.2 of the build order, the request pipeline, in this order: the typed-client spike, the spec, the code test-first, and the conformance review. The spike's candidates, and the questions the spec settles, are in the [platform spec's open design questions](spec/platform.md#open-design-questions). The [`system` spec](spec/system.md) waits on its triage. [Known gaps](#known-gaps) lists the built code that step 1.2 brings in line.

**Waiting on the owner:** nothing.

## Build order

The design is settled by building, not by further review of the documents. How the plan for unbuilt parts is kept, corrected and proven is in [CLAUDE.md](../CLAUDE.md), under "Docs for unbuilt parts are the plan".

From the end of step 1 on, the system is always deployable and observable: every change runs in the image CI publishes, the reference deployment rolls it out, and its telemetry reaches the observability stack. A step that adds a behavior also adds what operating it takes: its metrics and the dashboard questions they answer, its alerts and their runbooks, and its end-to-end scenarios.

Every part of the system is listed under [Foundation](#foundation), [Modules](#modules) or [CI checks](#ci-checks), with the step that builds it. Each part is specified just before it is built, and ends with a conformance review of what it built or changed ([Conformance reviews](#conformance-reviews)).

1. **Walking skeleton.** The running system, end to end, before any feature:
   1. **Review tools:** the checker tool in `xtask`, the conformance reviewer and the handoff skill in `.claude/` (D27). The checker starts with `check-test-map`, which reports every behavior that no test proves, and `check-docs`, which fails on any ID or link that doesn't resolve (DOC-13), on a check a rule names that neither [operations/ci.md](operations/ci.md) nor [CI checks](#ci-checks) lists, and on a document whose format breaks DOC-15 or DOC-16.
   2. **Request pipeline:** problem responses with trace IDs, trace context in and out, the middleware stack except rate limiting, readiness and bounded shutdown, and the capabilities endpoint as the first `/v1` route, with `openapi.json` and the typed client generated from it, through which the integration tests call the API. It starts with the typed-client spike ([platform spec](spec/platform.md#open-design-questions)). Counting panics (ERR-12) and leaving the probes out of request metrics (OBS-15) wait for the metric macro in step 1.3. The step's conformance review also covers the built code it rewrites, which no review has covered yet: `main.rs`, `composition.rs`, `platform::telemetry` and `platform::lifecycle`.
   3. **Telemetry:** traces, logs and metrics exported over OTLP; the metric declaration macro; the collector, Prometheus, Loki, Tempo, Grafana and Alertmanager, with logs and traces linked both ways; the first dashboard questions and alerts; and the generated metric catalog. `just dev` runs the API with the stack one flag away.
   4. **Image and pipeline:** the image, and CI that builds it once, scans it, boots it, runs the end-to-end suite against it, and promotes its digest, with signed releases on tags.
   5. **Reference deployment:** the production setup on one server, its deploy tooling, scheduled backups and restore, and the load test. The end-to-end suite proves a deploy with no downtime, a rollback, a restore, and the trace-ID guarantee for a 4xx response (D10).
2. **Thin slice.** Spec it, then build it test-first, each part extending the skeleton:
   1. `accounts`, `auth` and the `audit` writer, with authentication (`Principal` and `SessionVerifier`) and rate limiting: the IP and per-user limits and the per-email sign-in limit, built once with all three callers. It settles the `auth` items in [inventory/decision-trace.md](inventory/decision-trace.md), and extends the trace-ID guarantee to a 5xx response.
   2. `access` and `workspaces`, with `Authorized<P>`, the security event sink, and idempotency, which creating a workspace uses.
   3. Jobs, `mail` and `invitations`.
   4. The event hub and `realtime`, ending with "removing a member closes their stream". Readiness then includes the listener, and a deploy with an open stream loses no event.

   Together these exercise every mechanism the document reviews couldn't settle: D32 to D36, idempotency, jobs, email and the real-time hub.
3. **Rule fixes.** Apply one round of rule changes from what steps 1 and 2 taught, through [Changing a rule](standards/README.md#changing-a-rule). The round deletes rules as well as fixing them: a rule that no code needed, or that only restates another, goes. It settles each CI check that [CI checks](#ci-checks) leaves to it, and the commit message records the number of rules, behaviors and CI checks before and after. Right after it, rule and behavior IDs are renumbered once, closing the gaps the round leaves, before later tests carry the old numbers. From then on, IDs are never reused.
4. **Afternoon test.** A developer or agent new to the codebase, using only the docs, adds a small feature to the slice. Anything that slows them down is fixed by removing complexity, not by adding it.
5. **Remaining modules, one at a time.** `just new-module` and the guide to adding a module come first, shaped by the afternoon test, and every remaining module starts from them. Each module's spec is written just before the module is built, and carries the commitments that [inventory/decision-trace.md](inventory/decision-trace.md) lists for it.
6. **Close the migration.** The remaining parts under [Foundation](#foundation) are built, and one conformance review covers the whole codebase, under the same stopping rule. The ADRs are written from the rules as they then stand: ADR-00NN records decision DNN, and [inventory/decision-trace.md](inventory/decision-trace.md) lists where the rules refine the decisions' first wording in [inventory/SYNTHESIS.md](inventory/SYNTHESIS.md). Every `DNN` citation is replaced with a link to its ADR, and architecture.md is rewritten to describe the system as built (DOC-01). The migration closes when [Known gaps](#known-gaps) and [Rule proposals](#rule-proposals) are empty. Then `docs/inventory/` and this file are deleted.

Each CI check is built when the code it checks exists. A check that never catches a real problem is removed.

## Foundation

The parts that aren't modules, from [architecture.md](architecture.md) and the documentation set (DOC-08). [inventory/decision-trace.md](inventory/decision-trace.md) lists the commitments each one carries. A part's step reads "built" once it is.

| Part | Decisions | Step |
|---|---|---|
| The checker tool, the conformance reviewer and the handoff skill | D23, D27 | built |
| The request pipeline: problem responses, trace context, the middleware stack, readiness and shutdown | D8, D9, D10, D17, D20, D26 | 1.2 |
| The capabilities endpoint and `openapi.json` | D8, D9 | 1.2 |
| Rate limiting: the IP, per-user and per-action limits, with the client IP taken from trusted proxies | D11 | 2.1 |
| Telemetry, the observability stack and `just dev`, with the observability docs, the metric catalog and the runbooks | D10, D19, D28 | 1.3 |
| The image and the delivery pipeline, with the release docs | D15, D21 | 1.4 |
| The reference deployment, backups and the load test, with the deploy and backup docs | D15, D20, D30 | 1.5 |
| `just seed` and the OpenAPI UI in development | D28 | 2.4 |
| The threat model | D26 | 3 |
| `just new-module` and the guide to adding a module | D29 | 5 |
| `just rename`, the guide to renaming a fork, and the GitHub template repository | D29 | 6 |
| `SECURITY.md` and the OWASP ASVS Level 2 pass | D26 | 6 |
| The ADRs and `CHANGELOG.md` | D27, D29 | 6 |

## Modules

Each module goes through these stages, in order:

1. **Triage:** every old behavior it draws on has a verdict: kept, changed or dropped.
2. **Spec:** its spec is written ([spec/](spec/README.md)).
3. **Tests:** every behavior in the spec has a test named by its ID.
4. **Code:** the module is implemented.
5. **Conformance:** a review against the spec, the standards and architecture.md finds no violation ([Reviews](standards/README.md#reviews)).

"First slice" means the stage is done for the behaviors the first slice builds. `platform` goes through the stages one area at a time, and "database" means its `db` area.

| Module | Draws on inventory | Triage | Spec | Tests | Code | Conformance |
|---|---|---|---|---|---|---|
| `platform` | HTTP, DATA, RT, OBS, OPS | database | first slice | database, configuration | database, configuration, errors, types, the liveness probe and the logging setup | database, configuration, errors, types |
| `accounts` | USER | first slice | first slice | | | |
| `auth` | AUTH | first slice | first slice | | | |
| `access` | WS | first slice | first slice | | | |
| `workspaces` | WS | first slice | first slice | | | |
| `invitations` | WS, MAIL | first slice | first slice | | | |
| `audit` | AUDIT | first slice | first slice | | | |
| `realtime` | RT | first slice | first slice | | | |
| `mail` | MAIL | first slice | first slice | | | |
| `files` | FILE | | | | | |
| `client_telemetry` | OBS | | | | | |
| `system` | HTTP, OPS | | | | | |

## CI checks

Every check a rule names, and the step that builds it. [operations/ci.md](operations/ci.md) lists the checks that exist. Until a check exists, its rule is enforced by review.

| Check | Step |
|---|---|
| `fmt-check`, `test`, `deny` | built |
| `check-test-map`, `check-docs` | built |
| `check-docs`, for the names of other codebases (DOC-04) | 3 |
| `check-docs`, for accepted ADRs (DOC-11) | 6, with the first ADR |
| `openapi` | 1.2, and its breaking-change diff (API-18) with the first release |
| `check-instrument` | 1.2, with the list of telemetry names in `platform::telemetry` |
| `check-telemetry-config`, `check-metrics`, `alert-rules` | 1.3 |
| `check-toolchain`, `check-pins`, `check-protection`, `image-scan`, `verify-release` | 1.4 |
| `e2e` | 1.5, extended by every later step |
| `check-layers`, `check-schema`, `check-deps`, `check-tenancy` | 1.2 to 2.2, each with the first code it checks |
| `check-layout`, `check-table-ownership`, `check-config`, `check-migrations`, `sqlx-check`, `setup-smoke` | 3 decides: built, or the rule's check changes |
| `check-personal-data` | 5, with export and erasure in `accounts` |
| `check-removable` | 5, with the first optional part |
| `test-migrations` | 6, with the first release |
| `check-rename` | 6, with `just rename` |

## Known gaps

Built code that doesn't follow a rule or a behavior yet ([Changing a rule](standards/README.md#changing-a-rule)).

| Rule or behavior | Built code that doesn't follow it | Closed by |
|---|---|---|
| OBS-02 | The logging setup writes no `service.name`, `service.version`, `deployment.environment.name`, `trace_id` or `span_id`. | 1.2 |
| OBS-05, OPS-03 | Log events use the field names `area`, `error` and `applied`, and no list of names exists yet. | 1.2 |
| PLATFORM-033 | `serve` shuts down at once, with exit code 0, when it can't install its signal handlers. | 1.2 |
| OBS-06, CODE-05 | `bootstrap` computes the password verifiers with `spawn_blocking` directly, without the span of the `platform` helper, which arrives with the tracer. | 1.2 |

## Rule proposals

Proposals that wait, each with the step that decides it ([Changing a rule](standards/README.md#changing-a-rule)).

| From | Proposal | Decided in |
|---|---|---|
| Field-name proposal | OBS-11: own metric names drop `app.`, which OpenTelemetry's conventions use for client applications since 1.32, and follow OBS-05's pattern for own names. ERR-11 and ERR-12 name two such metrics. | 1.3, with the metric macro |
| Catch-up review | OPS-13 and OPS-19: `just setup` checks the versions of the tools CI pins, such as cargo-deny 0.20.2, and a sqlx-cli that matches sqlx 0.9. | 1.4, with `check-pins` and `check-toolchain` |
| Standards review | An OPS rule for D14: TLS on every connection that leaves the host, and storage encrypted at rest as a stated deployment requirement. | 1.5 |
| Research | API-15 names the draft it follows: draft-ietf-httpapi-ratelimit-headers-11 (2026-05), with `RateLimit: "default";r=50;t=30` and `RateLimit-Policy: "burst";q=100;w=60`. | 2.1 |
| Research | API-12 names the draft it follows: draft-ietf-httpapi-idempotency-key-header-08 (2026-10), whose key is a structured-field string. | 2.2 |
| Conformance review of c252bab | CODE-03, DATA-20 and DATA-21: `check-docs` fails on an `#[expect]` of the print or `disallowed_*` lints outside the places those rules allow, so finding one takes no review. | 3, which settles the checks |

## Conformance reviews

Each pass of each review, run with the `conformance-review` skill in `.claude/skills/`. Reviewer A reads rule by rule, and reviewer B file by file. "Shared" counts the findings both made. A result is the pass's outcome under "When the review stops" in [standards/README.md](standards/README.md#when-the-review-stops), with the commits that acted on it.

| Scope | Pass | Findings (A, B, shared) | Must-fix | Must-fix regressions | Result |
|---|---:|---|---:|---:|---|
| `platform::db` | 1 | 14 (one reviewer) | 8 | n/a | `next pass`: afae006 |
| `platform::db` | 2 | 6 (3, 4, 1 shared) | 2 | 1 | `stopped: no progress`: 2adeab1 |
| Built parts that step 1.2 doesn't rewrite: `xtask`; `platform::db`, configuration, errors and types; the test support; the migration; CI and build files | 1 | 16 (12, 10, 6 shared) | 13 | 0 | `next pass`: 376a450 |
| Built parts that step 1.2 doesn't rewrite: `xtask`; `platform::db`, configuration, errors and types; the test support; the migration; CI and build files | 2 | 5 (5, 2, 2 shared) | 4 | 2 | `stopped: no progress`: b34412c, 6f66c22 |
| c252bab: the documentation rules and their format checks, the rules it rewrote against the code they describe, and the new clippy bans | 1 | 22 (18, 6, 2 shared) | 8 | 0 | `next pass` |

## Decisions

D1 to D36 are recorded in [inventory/SYNTHESIS.md](inventory/SYNTHESIS.md). Later decisions are applied to the rules and specs they settle, with their reasons in the commits that made them. [coverage.md](coverage.md) maps every foundational area to a decision, and each decision becomes an ADR.

| ID | Decision | Status |
|---|---|---|
| D1 | Side effects commit atomically with the change | accepted |
| D2 | Authorization model | accepted |
| D3 | Tenancy scoping, with no server-side active workspace | accepted |
| D4 | Tenancy without row-level security | accepted: (c), no row-level security |
| D5 | Access changes take effect immediately | accepted |
| D6 | Audit policy | accepted |
| D7 | Privacy and data minimization | accepted |
| D8 | API contract conventions | accepted |
| D9 | Strict configuration and honest failure | accepted |
| D10 | The trace-ID guarantee | accepted |
| D11 | Abuse controls and rate limiting | accepted |
| D12 | Idempotency keys | accepted |
| D13 | Background jobs and multiple instances | accepted |
| D14 | Scope of field-level encryption | accepted: (b), secrets only |
| D15 | Delivery pipeline and deploy scope | accepted, including the single-host rolling deploy |
| D16 | Account and workspace lifecycle | accepted |
| D17 | Internal error model | accepted |
| D18 | Authentication that is ready for web clients | accepted |
| D19 | Observability stack design | accepted |
| D20 | Runtime topology and scaling | accepted |
| D21 | CI pipeline shape, on GitHub Actions | accepted |
| D22 | Module architecture | accepted |
| D23 | Testing strategy | accepted |
| D24 | Data access conventions | accepted |
| D25 | Domain types and input validation | accepted |
| D26 | Secrets and security baseline | accepted |
| D27 | Documentation and Claude Code guidance | accepted |
| D28 | Local development experience | accepted |
| D29 | Fork and extension model | accepted |
| D30 | Backups and recovery | accepted |
| D31 | Personal data contract: export and erasure (new feature) | accepted |
| D32 | Hashing tokens, codes and emails | accepted |
| D33 | Creating a workspace | accepted |
| D34 | Seeing and accepting invitations | accepted |
| D35 | Access-change signals | accepted |
| D36 | The commit decides the outcome | accepted |
| D37 | Crates that implement the Postgres protocol may build its standard constructions from vetted primitives (SEC-14) | accepted |
| D38 | A verified database connection trusts the operating system's certificate store, not certificates compiled in ([platform spec](spec/platform.md#connections)) | accepted |
