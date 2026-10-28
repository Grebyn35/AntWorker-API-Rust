# Migration tracker

Tracks the rewrite from `AntWorker-API` (commit `0210895`) to this repository. It holds status and pointers only (DOC-16). Every other fact lives in its home (DOC-14).

## Current work

**Next:** two parts end step 1.5. The trial server, which the owner allows wiping, was set up anew with `just server-setup` and `just server-check`, restored from its bucket, and runs 0.1.0-rc.8, with its telemetry in the owner's Grafana Cloud stack; the trial's findings are in the messages of the commits "Hand off step 1.5's trial deploy" and "Prove the server's setup on a new server, and act on what the trial showed".
1. Build D53: while no replica can answer, as during a restore, Caddy answers at once with the 503 problem `request.unavailable`, whose trace ID leads to Caddy's span and log line. The design, proven by experiment, is in the message of the commit "Record the owner's choice for what the proxy answers".
2. A conformance review of everything since a28df7b.

The trace test `traces::obs_08_…` fails now and then on amd64; its output now says why, so read it at its next failure.

**Waiting on the owner:** which provider production runs on, since the trial's encrypts no disk (OPS-20).

## Build order

The design is settled by building, not by further review of the documents. How the plan for unbuilt parts is kept, corrected and proven is in [CLAUDE.md](../CLAUDE.md), under "Docs for unbuilt parts are the plan".

From the end of step 1 on, the system is always deployable and observable: every change runs in the image CI publishes, the reference deployment rolls it out, and its telemetry reaches the observability stack. A step that adds a behavior also adds what operating it takes: its metrics and the dashboard questions they answer, its alerts and their runbooks, and its end-to-end scenarios.

Every part of the system is listed under [Foundation](#foundation), [Modules](#modules) or [CI checks](#ci-checks), with the step that builds it. Each part is specified just before it is built, and ends with a conformance review of what it built or changed ([Conformance reviews](#conformance-reviews)).

1. **Walking skeleton.** The running system, end to end, before any feature:
   1. **Review tools:** the checker tool in `xtask`, the conformance reviewer and the handoff skill in `.claude/` (D27). The checker starts with `check-test-map`, which reports every behavior that no test proves, and `check-docs`, which fails on any ID or link that doesn't resolve (DOC-13), on a check a rule names that neither [operations/ci.md](operations/ci.md) nor [CI checks](#ci-checks) lists, and on a document whose format breaks DOC-15 or DOC-16.
   2. **Request pipeline:** problem responses with trace IDs, trace context in and out, the middleware stack except rate limiting, readiness and bounded shutdown, and the capabilities endpoint as the first `/v1` route, with `openapi.json`, against which the test support checks every response the integration tests receive. The step's conformance review also covers the built code it rewrites, which no review has covered yet: `main.rs`, `composition.rs`, `platform::telemetry` and `platform::lifecycle`.
   3. **Telemetry:** traces, logs and metrics exported over the OpenTelemetry Protocol (OTLP); the metric declarations; the collector, Prometheus, Loki, Tempo, Grafana and Alertmanager, with logs and traces linked both ways; the first dashboard questions and alerts; and the generated metric catalog. `just dev` runs the API with the stack one flag away.
   4. **Image and pipeline:** the image, and CI that builds it once, scans it, boots it, runs the end-to-end suite against it, and promotes its digest, with signed releases that `just release` asks for.
   5. **Reference deployment:** the production setup on one server, its deploy tooling, scheduled backups and restore, and the load test, with Grafana on an HTTPS address of its own and the path to a managed observability backend (D42). The end-to-end suite proves a deploy with no downtime, a rollback, a restore, and the trace-ID guarantee for a 4xx response (D10).
2. **Thin slice.** Spec it, then build it test-first, each part extending the skeleton:
   1. `accounts`, `auth` and the `audit` writer, with authentication (`Principal` and `SessionVerifier`) and rate limiting: the IP and per-user limits and the per-email sign-in limit, built once with all three callers. It settles the `auth` items in [inventory/decision-trace.md](inventory/decision-trace.md), and extends the trace-ID guarantee to a 5xx response.
   2. `access` and `workspaces`, with `Authorized<P>`, the security event sink, and idempotency, which creating a workspace uses.
   3. Jobs, `mail` and `invitations`.
   4. The event hub and `realtime`, ending with "removing a member closes their stream". Readiness then includes the listener, and a deploy with an open stream loses no event.

   Together these exercise every mechanism the document reviews couldn't settle: D32 to D36, idempotency, jobs, email and the real-time hub.
3. **Rule fixes.** Apply one round of rule changes from what steps 1 and 2 taught, through [Changing a rule](standards/README.md#changing-a-rule). The round deletes rules as well as fixing them: a rule that no code needed, or that only restates another, goes. It settles each CI check that [CI checks](#ci-checks) leaves to it, and the commit message records the number of rules, behaviors and CI checks before and after. Right after it, rule and behavior IDs are renumbered once, closing the gaps the round leaves, before later tests carry the old numbers. From then on, IDs are never reused.
4. **Afternoon test.** A developer or agent new to the codebase, using only the docs, adds a small feature to the slice. Anything that slows them down is fixed by removing complexity, not by adding it.
5. **Remaining modules, one at a time.** `just new-module` and the guide to adding a module come first, shaped by the afternoon test, and every remaining module starts from them. Each module's spec is written just before the module is built, and carries the commitments that [inventory/decision-trace.md](inventory/decision-trace.md) lists for it.
6. **Close the migration.** The remaining parts under [Foundation](#foundation) are built, and one conformance review covers the whole codebase, under the same stopping rule. The architecture decision records (ADRs) are written from the rules as they then stand: ADR-00NN records decision DNN, and [inventory/decision-trace.md](inventory/decision-trace.md) lists where the rules refine the decisions' first wording in [inventory/SYNTHESIS.md](inventory/SYNTHESIS.md). Every `DNN` citation is replaced with a link to its ADR, and architecture.md is rewritten to describe the system as built (DOC-01). The migration closes when [Known gaps](#known-gaps) and [Rule proposals](#rule-proposals) are empty. Then `docs/inventory/` and this file are deleted.


## Foundation

The parts that aren't modules, from [architecture.md](architecture.md) and the documentation set (DOC-08). [inventory/decision-trace.md](inventory/decision-trace.md) lists the commitments each one carries. A part's step reads "built" once it is.

| Part | Decisions | Step |
|---|---|---|
| The checker tool, the conformance reviewer and the handoff skill | D23, D27 | built |
| The request pipeline: problem responses, trace context, the middleware stack, readiness and shutdown | D8, D9, D10, D17, D20, D26 | built |
| The capabilities endpoint and `openapi.json` | D8, D9 | built |
| Rate limiting: the IP, per-user and per-action limits, with the client IP taken from trusted proxies | D11 | 2.1 |
| Telemetry, the observability stack and `just dev`, with the observability docs, the metric catalog and the runbooks | D10, D19, D28 | built |
| The image and the delivery pipeline, with the release docs | D15, D21, D43 | built |
| The reference deployment, backups and the load test, with the deploy and backup docs | D15, D20, D30, D44, D45, D47, D49, D50, D51 | built |
| Grafana on an HTTPS address of its own with sign-in, and the path to a managed observability backend through the collector, with Grafana Cloud as the worked example | D19, D42 | built |
| The command that removes an optional part, which `check-removable` runs | D29, D39 | 2.3 |
| `just seed` and the OpenAPI UI in development | D28 | 2.4 |
| The threat model | D26 | 3 |
| `just new-module` and the guide to adding a module | D29 | 5 |
| `just rename`, the guide to renaming a fork, and the GitHub template repository | D29 | 6 |
| `SECURITY.md`, and the pass against Level 2 of the Application Security Verification Standard (ASVS) of the Open Worldwide Application Security Project (OWASP) | D26 | 6 |
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
| `platform` | HTTP, DATA, RT, OBS, OPS | database, request pipeline | first slice | database, configuration, request pipeline, lifecycle, telemetry | database, configuration, errors, types, request pipeline, lifecycle, telemetry | database, configuration, errors, types, request pipeline, lifecycle, telemetry |
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
| `system` | HTTP, OPS | capabilities | capabilities | capabilities | capabilities | capabilities |

## CI checks

Every check a rule names, and the step that builds it. [operations/ci.md](operations/ci.md) lists the checks that exist. Until a check exists, its rule is enforced by review.

| Check | Step |
|---|---|
| `fmt-check`, `test`, `deny` | built |
| `check-test-map`, `check-docs` | built |
| `check-docs`, for the names of other codebases and the paths into the inventory (DOC-04) | 3 |
| `check-docs`, for accepted ADRs (DOC-11) | 6, with the first ADR |
| `check-openapi`, `check-instrument` | built |
| `check-metrics`, `check-telemetry-config`, `alert-rules` | built |
| `check-openapi`, for list shapes, methods, statuses, scopes and formats (API-02 to API-05, API-11) | 2.1, with the first routes that have them |
| `check-openapi`, for the breaking-change diff (API-18, ERR-02) | the first final release |
| `check-pins`, `image-scan`, `image-rescan`, `verify-release`, `check-shell` | built |
| `e2e` | built, and extended by every later step |
| `check-layers`, `check-deps`, `check-schema` | 2.1, with the first service, the first call between modules and the first tables |
| `check-tenancy` | 2.2, with the first workspace tables |
| `check-layout`, `check-table-ownership`, `check-config`, `check-migrations`, `sqlx-check`, `setup-smoke` | 3 decides: built, or the rule's check changes |
| `check-personal-data` | 5, with export and erasure in `accounts` |
| `check-removable` | 2.3, with the first removal that leaves a workspace shape: `invitations`, for the personal shape |
| `test-migrations` | 6, with the first final release |
| `check-rename` | 6, with `just rename` |

## Known gaps

Built code that doesn't follow a rule or a behavior yet ([Changing a rule](standards/README.md#changing-a-rule)).

| Rule or behavior | Built code that doesn't follow it | Closed by |
|---|---|---|
| OPS-18 | A release's capacity, which `just load-test` measures, has no release notes to go in: `CHANGELOG.md` isn't written | 6, with `CHANGELOG.md` |
| DOC-12 | A release has no upgrade notes: `CHANGELOG.md` isn't written | 6, with `CHANGELOG.md` |

## Rule proposals

Proposals that wait, each with the step that decides it ([Changing a rule](standards/README.md#changing-a-rule)).

| From | Proposal | Decided in |
|---|---|---|
| Research | API-15 names the draft it follows: draft-ietf-httpapi-ratelimit-headers-11 (2026-05), with `RateLimit: "default";r=50;t=30` and `RateLimit-Policy: "burst";q=100;w=60`. | 2.1 |
| Research | API-12 names the draft it follows: draft-ietf-httpapi-idempotency-key-header-08 (2026-10), whose key is a structured-field string. | 2.2 |
| Conformance review of cf1253c | API-07, API-09: whether `request.invalid_query` names the unknown parameters in `errors`, since clients never act on `detail` (ERR-09) | 2.1, with the first route that parses query values |
| Conformance review of cf1253c | ARCH-12: whether the clock, readiness and the database that `main.rs` opens count among the services that only `composition.rs` builds | 2.1, with the first services |
| Conformance review of b60787f..9a0522d | DOC-15: `check-docs` reads the `tag` of each `#[utoipa::path]` under `crates/app/src/modules/<module>/`, and fails unless it is `<module>`. | 2.1, with the second module that serves routes |
| Conformance review of b60787f..9a0522d | API-17: the test support checks each request body an integration test sends against its operation's documented request, and `check-openapi` checks that each field of a domain type carries the type's limits. | 2.1, with the first route that takes a body |
| Conformance review of c252bab | DATA-20: a check reads the locked sqlx sources and fails on a public function that takes SQL text and that `clippy.toml` doesn't ban, so the bans stay complete when sqlx is updated. | 2.1, which writes the first queries of a module |
| Conformance review of 3ae7168 | A DATA rule for D39: data a module adds for its product has a `workspace_id`, and a check fails on a table keyed by a user ID that isn't listed as account data. | 2.2, with the first workspace tables and `check-tenancy` |
| Conformance review of 3ae7168 | ARCH-07: whether password sign-in is an optional part, for a fork that signs in only through OAuth providers | 5, with the full `auth` module |
| Conformance review of 3ae7168 | DATA-12 and the platform spec's unit of work: how an operator's subcommand, such as `seed` or the setup subcommand, opens its unit of work, and as which role | 2.4, with `seed` |

## Conformance reviews

Each pass of each review, run with the `conformance-review` skill in `.claude/skills/`. Reviewer A reads rule by rule, and reviewer B file by file. "Shared" counts the findings both made. A result is the pass's outcome under "When the review stops" in [standards/README.md](standards/README.md#when-the-review-stops), with the commits that acted on it.

| Scope | Pass | Findings (A, B, shared) | Must-fix | Must-fix regressions | Result |
|---|---:|---|---:|---:|---|
| `platform::db` | 1 | 14 (one reviewer) | 8 | n/a | `next pass`: afae006 |
| `platform::db` | 2 | 6 (3, 4, 1 shared) | 2 | 1 | `stopped: no progress`: 2adeab1 |
| Built parts that step 1.2 doesn't rewrite: `xtask`; `platform::db`, configuration, errors and types; the test support; the migration; CI and build files | 1 | 16 (12, 10, 6 shared) | 13 | 0 | `next pass`: 376a450 |
| Built parts that step 1.2 doesn't rewrite: `xtask`; `platform::db`, configuration, errors and types; the test support; the migration; CI and build files | 2 | 5 (5, 2, 2 shared) | 4 | 2 | `stopped: no progress`: b34412c, 6f66c22 |
| c252bab: the documentation rules and their format checks, the rules it rewrote against the code they describe, and the new clippy bans | 1 | 22 (18, 6, 2 shared) | 8 | 0 | `next pass`: 58d9917 |
| c252bab: the documentation rules and their format checks, the rules it rewrote against the code they describe, and the new clippy bans | 2 | 15 (12, 7, 4 shared) | 2 | 2 | `stopped: no progress`: c891b3a |
| cf1253c: step 1.2, the request pipeline, and the built code it rewrites: `main.rs`, `composition.rs`, `platform::telemetry` and `platform::lifecycle` | 1 | 17 (16, 10, 9 shared) | 9 | 0 | `next pass`: aa2ce06 |
| cf1253c: step 1.2, the request pipeline, and the built code it rewrites: `main.rs`, `composition.rs`, `platform::telemetry` and `platform::lifecycle` | 2 | 17 (12, 7, 2 shared) | 2 | 1 | `stopped: no progress`: 1d8c2d0 |
| b60787f..9a0522d: API-17's method test, and the checks that took over review for DOC-09, DOC-15, CODE-03, OBS-03 and OBS-06 | 1 | 7 (7, 3, 3 shared) | 3 | 0 | `next pass`: ffb3316 |
| b60787f..9a0522d: API-17's method test, and the checks that took over review for DOC-09, DOC-15, CODE-03, OBS-03 and OBS-06 | 2 | 6 (5, 1, 0 shared) | 3 | 2 | `stopped: no progress`: 9dca252 |
| 3ae7168: the workspace shapes (D39, D40), and the table check of DOC-15 | 1 | 9 (8, 2, 1 shared) | 3 | 0 | `next pass`: 16f4aae |
| 3ae7168: the workspace shapes (D39, D40), and the table check of DOC-15 | 2 | 7 (7, 3, 3 shared) | 1 | 1 | `stopped: no progress`: 1e56256 |
| e3939f2..f09687d: step 1.3, telemetry: export, the metric declarations, the observability stack and its checks, `just dev`, and the plan of D42 | 1 | 17 (14, 6, 3 shared) | 6 | 0 | `next pass`: ce2c87a |
| e3939f2..ce2c87a: step 1.3, telemetry: export, the metric declarations, the observability stack and its checks, `just dev`, and the plan of D42 | 2 | 7 (6, 2, 1 shared) | 1 | 0 | `next pass`: 3e95ac0 |
| e3939f2..090b820: step 1.3, telemetry: export, the metric declarations, the observability stack and its checks, `just dev`, and the plan of D42 | 3 | 8 (8, 2, 2 shared) | 1 | 0 | `stopped: no progress`: 953cbc1 |
| 63e2951..76e2b75: step 1.4, the image, the delivery pipeline and releases | 1 | 14 (11, 6, 3 shared) | 4 | 0 | `next pass`: 5375877 |
| 63e2951..5375877: step 1.4, the image, the delivery pipeline and releases | 2 | 6 (5, 2, 1 shared) | 0 | 0 | `clean` |
| 87c8718..1a85dbc: step 1.5, the reference deployment | 1 | 20 (18, 10, 8 shared) | 8 | 0 | `next pass`: 3960dba |
| 87c8718..3960dba: step 1.5, the reference deployment | 2 | 11 (9, 4, 2 shared) | 2 | 0 | `next pass`: 737c1be |
| 87c8718..353e0e9: step 1.5, the reference deployment | 3 | 10 (8, 4, 2 shared) | 1 | 0 | `next pass`: 97f5cae |
| 87c8718..97f5cae: step 1.5, the reference deployment | 4 | 9 (5, 5, 1 shared) | 1 | 0 | `stopped: no progress`: 9306556 |

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
| D39 | A product's data lives in workspaces, and the optional parts a fork keeps give it its shape: teams, one organization, or personal ([architecture](architecture.md#workspace-shapes)) | accepted |
| D40 | A data export covers the content of every workspace whose only member is the user, the same workspaces that erasure deletes ([accounts spec](spec/accounts.md#export-and-erasure)) | accepted |
| D41 | A latency spike leads to its requests through the kept traces slower than the latency objective, which the latency dashboard lists, rather than through exemplars, which OpenTelemetry's Rust SDK doesn't record (OBS-08) | accepted |
| D42 | The self-hosted observability stack is the default, with Grafana behind HTTPS and sign-in; a deployment may instead send its telemetry through the collector to a managed backend, with Grafana Cloud as the worked example ([architecture](architecture.md#9-runtime-topology)) | accepted |
| D43 | The image's binary is static against musl, compiled for both platforms on one machine through zig, on a base with no C library ([architecture](architecture.md#10-technology)) | accepted |
| D44 | The bundled Postgres has point-in-time recovery: pgBackRest archives its write-ahead log continuously and takes scheduled base backups, encrypted, to a bucket off the server, from a Postgres image the pipeline builds (OPS-17) | accepted |
| D45 | The reference deployment's deploy is a script of its own around Docker Compose, with two colors of replicas that Caddy names and health-checks one by one: a deploy starts the idle color, waits until Caddy finds it ready, then stops the live one ([architecture](architecture.md#9-runtime-topology)) | accepted |
| D46 | The image's static binary allocates through mimalloc, since under musl's own allocator a replica's memory grew with every request it served ([architecture](architecture.md#10-technology)) | accepted |
| D47 | The bundled database's backups are kept by time, `BACKUP_RETENTION_DAYS`, rather than by a count of full backups, so the full backups that `just backup` and a restore take never shorten how far a restore reaches ([backups.md](operations/backups.md)) | accepted |
| D48 | The end-to-end tests' servers pull Docker Hub's images through a pull-through cache that Compose runs and `just e2e` fills for their platform beforehand, and can't reach Docker Hub themselves (TEST-07, [ci.md](operations/ci.md)) | accepted |
| D49 | With the self-hosted stack, a deployment's alert integrations are its release's, in the fork's `alertmanager.yaml`, and their credentials the deployment's, in `secrets/alertmanager/`, which each integration reads through its option for a file ([observability.md](operations/observability.md#alerts)) | accepted |
| D50 | A deploy that refuses or fails once it installed its release's files, and before its replicas serve in place of the old ones, puts back the ones it replaced; those it couldn't put back, because a forced stop or a failed copy prevented it, the next deploy, backup or restore puts back first. Between tasks, once a deploy's replicas have served, the deployment's directory holds the files of the last deploy whose replicas served (OPS-16, [deploy.md](operations/deploy.md#the-files-on-the-server)) | accepted |
| D51 | The reference deployment's settings and secrets stay on its server, and every task on the server, its setup included, runs from a developer's machine through `just` over SSH, rather than keeping the settings in the repository, where secrets can't go, or on one developer's machine (OPS-22) | accepted |
| D52 | With a managed backend, each deploy imports the release's dashboards and alert rules into the backend's Grafana once its replicas serve: the dashboards into a folder that only the import and the Grafana's administrators may change, the rules as Grafana's own through its conversion of Prometheus rules rather than into the backend's ruler, whose alerts reach no Alertmanager on Grafana Cloud. Grafana's notification policies route the alerts, and a failed import only warns (OBS-13, OBS-14, [observability.md](operations/observability.md#a-managed-backend)) | accepted |
| D53 | While no replica can answer, the reference deployment's proxy answers at once with the 503 problem `request.unavailable` and `Retry-After`, and D10 holds for it: the proxy traces each request but the health probes, and logs each request it answers itself, through the collector, with no personal data (API-07, OBS-04, OBS-06, OBS-15) | accepted |
