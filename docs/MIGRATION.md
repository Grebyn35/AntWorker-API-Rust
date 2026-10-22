# Migration tracker

Tracks the rewrite from `AntWorker-API` (commit `0210895`) to this repository.

## Spec documents

| Document | Status |
|---|---|
| [architecture.md](architecture.md) | draft |
| [standards/](standards/README.md) | Review stopped after pass 4; its resolutions are D32 to D36. Tenancy is enforced by the application, with no row-level security (D4). 160 rules. |
| [spec/](spec/README.md) | The first slice's nine specs are written. The triage behind them is in [inventory/triage-slice.md](inventory/triage-slice.md). |
| `docs/adr/` | Not started. Written after the rule fixes (Build order, step 3), from the rules as they then stand. The recommendation text in SYNTHESIS.md predates the standards, and [inventory/decision-trace.md](inventory/decision-trace.md) lists where the rules refine it. ADR-00NN records decision DNN. Before `docs/inventory/` is deleted, every `DNN` citation is replaced with a link to its ADR. |
| [CLAUDE.md](../CLAUDE.md) | written |

## Current work

**Built:**
- The workspace scaffold (`platform` and `app` crates), strict configuration with the startup-failure format (PLATFORM-023, OPS-03), and the liveness probe (PLATFORM-019).
- `platform::db`: Postgres 18 in `compose.yaml`; the `bootstrap` and `migrate` subcommands (PLATFORM-010, PLATFORM-027 to PLATFORM-029); the startup checks of the role and the schema (PLATFORM-009, PLATFORM-030); the unit of work; the first migration, with the default privileges (DATA-19); the test support, with one database per test (TEST-06); and the committed query cache (DATA-20).

`just ci` passes locally and in GitHub Actions, where CI starts Postgres through `just setup` and Compose: formatting, clippy, 41 tests and `cargo-deny`. A scheduled workflow runs `cargo-deny` every day.

**Next:** step 1 of the build order, the walking skeleton, one part at a time. Each part ends with `just ci` passing.

## Build order

The design is settled by building, not by further review of the documents. Until a part is built, its spec and its sections of architecture.md are the plan, not a description of code. When building shows the plan is wrong, the plan is corrected in the same change as the code, and a rule changes through [Changing a rule](standards/README.md#changing-a-rule). A behavior counts as proven only when a test named by its ID passes.

From the end of step 1 on, the system is always deployable and observable: every change runs in the image CI publishes, the reference deployment rolls it out, and its telemetry reaches the observability stack. A step that adds a behavior also adds what operating it takes: its metrics and the dashboard questions they answer, its alerts and their runbooks, and its end-to-end scenarios.

Every part of the system is listed under [Foundation](#foundation), [Modules](#modules) or [CI checks](#ci-checks), with the step that builds it. Each part is specified just before it is built.

1. **Walking skeleton.** The running system, end to end, before any feature:
   1. **Review tools:** the checker tool in `xtask` and the conformance reviewer in `.claude/` (D27). The checker starts with `check-test-map`, which reports every behavior that no test proves, and `check-docs`, which fails on any ID or link that doesn't resolve (DOC-13) and on any check a rule names that neither [operations/ci.md](operations/ci.md) nor [CI checks](#ci-checks) lists.
   2. **Request pipeline:** problem responses with trace IDs, trace context in and out, the middleware stack, readiness and bounded shutdown, and the capabilities endpoint as the first `/v1` route, with `openapi.json`. It starts with the typed-client spike: before any test calls the API through a client generated from `openapi.json`, a throwaway test compares the candidate generators, including their licenses (CODE-12) and the OpenAPI version they read.
   3. **Telemetry:** traces, logs and metrics exported over OTLP; the metric declaration macro; the collector, Prometheus, Loki, Tempo, Grafana and Alertmanager, with logs and traces linked both ways; the first dashboard questions and alerts; and the generated metric catalog. `just dev` runs the API with the stack one flag away.
   4. **Image and pipeline:** the image, and CI that builds it once, scans it, boots it, runs the end-to-end suite against it, and promotes its digest, with signed releases on tags.
   5. **Reference deployment:** `deploy/compose.yaml`, the deploy script, scheduled backups and restore, and the load test. The end-to-end suite proves a deploy with no downtime, a rollback, a restore, and the trace-ID guarantee for a 4xx response (D10).
2. **Thin slice.** Spec it, then build it test-first, each part extending the skeleton:
   1. `accounts`, `auth` and the `audit` writer, with authentication (`Principal` and `SessionVerifier`) and the per-email rate limit. It settles the `auth` items in [inventory/decision-trace.md](inventory/decision-trace.md), and extends the trace-ID guarantee to a 5xx response.
   2. `access` and `workspaces`, with `Authorized<P>`, the security event sink, and idempotency, which creating a workspace uses.
   3. Jobs, `mail` and `invitations`.
   4. The event hub and `realtime`, ending with "removing a member closes their stream". Readiness then includes the listener, and a deploy with an open stream loses no event.

   Together these exercise every mechanism the document reviews couldn't settle: D32 to D36, idempotency, jobs, email and the real-time hub.

   Three constraints apply to steps 1 and 2:
   - **Tenancy is enforced by the application.** Every query filters by tenant itself, checked by `check-tenancy` and by the foreign-workspace probes (TEST-09). There is no row-level security (D4).
   - **A short list of custom CI checks.** Steps 1 and 2 build only the checks that [CI checks](#ci-checks) assigns them, as one checker tool.
   - **A typed client generated from `openapi.json`.** The integration tests call the API through it, and the desktop client uses it later.
3. **Rule fixes.** Apply one round of rule changes from what steps 1 and 2 taught, through [Changing a rule](standards/README.md#changing-a-rule). The round deletes rules as well as fixing them: a rule that no code needed, or that only restates another, goes. It settles each CI check that [CI checks](#ci-checks) leaves to it, and records the number of rules, behaviors and CI checks before and after.
4. **Afternoon test.** A developer or agent new to the codebase, using only the docs, adds a small feature to the slice. Anything that slows them down is fixed by removing complexity, not by adding it.
5. **Remaining modules, one at a time.** `just new-module` and the guide to adding a module come first, shaped by the afternoon test, and every remaining module starts from them. Each module's spec is written just before the module is built, and carries the commitments that [inventory/decision-trace.md](inventory/decision-trace.md) lists for it.
6. **Close the migration.** The remaining parts under [Foundation](#foundation) are built, architecture.md is rewritten to describe the system as built (DOC-01), and `docs/inventory/` and this file are deleted.

Each CI check is built when the code it checks exists. A check that never catches a real problem is removed.

## Foundation

The parts that aren't modules, from [architecture.md](architecture.md) and the documentation set (DOC-08). [inventory/decision-trace.md](inventory/decision-trace.md) lists the commitments each one carries. A part's step reads "built" once it is.

| Part | Decisions | Step |
|---|---|---|
| The checker tool and the conformance reviewer | D23, D27 | 1.1 |
| The request pipeline: problem responses, trace context, the middleware stack, readiness and shutdown | D8, D9, D10, D11, D17, D20, D26 | 1.2 |
| The capabilities endpoint and `openapi.json` | D8, D9 | 1.2 |
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
5. **Conformance:** a review against the spec and the standards finds no violation, and what architecture.md says about the module matches the code. Every finding cites a rule ID, a behavior ID, or the section of architecture.md it contradicts.

"First slice" means the stage is done for the behaviors the first slice builds. `platform` goes through the stages one area at a time, and "database" means its `db` area.

| Module | Draws on inventory | Triage | Spec | Tests | Code | Conformance |
|---|---|---|---|---|---|---|
| `platform` | HTTP, DATA, RT, OBS, OPS | database | first slice | database | database | |
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
| `fmt`, `test`, `deny` | built |
| `check-test-map`, `check-docs` | 1.1 |
| `openapi` | 1.2 |
| `check-instrument`, `check-telemetry-config`, `check-metrics`, `alert-rules` | 1.3 |
| `check-toolchain`, `check-pins`, `check-protection`, `image-scan`, `verify-release` | 1.4 |
| `e2e` | 1.5, extended by every later step |
| `check-layers`, `check-schema`, `check-deps`, `check-tenancy` | 1.2 to 2.2, each with the first code it checks |
| `check-layout`, `check-table-ownership`, `check-config`, `check-migrations`, `sqlx-check`, `setup-smoke` | 3 decides: built, or the rule's check changes |
| `check-personal-data` | 5, with export and erasure in `accounts` |
| `check-removable` | 5, with the first optional part |
| `test-migrations` | 6, with the first release |
| `check-rename` | 6, with `just rename` |

## Standards review

Follows the review procedure in [standards/README.md](standards/README.md#reviews). The review is capped at six passes. If it stops before reporting zero `must-fix` findings, the fallback resolutions apply.

| Pass | Findings | Must-fix | Must-fix regressions | Result |
|---|---:|---:|---:|---|
| 1 | 39 | 11 | n/a | all applied |
| 2 | 31 | 4 | not tagged | all applied |
| 3 | 27 | 6 | not tagged | all applied, except the queued proposal below |
| 4 | 22 | 5 | 5 | Stopped by condition 2 (`must-fix` regressions). The 17 `should-fix` findings are applied. The five `must-fix` findings are resolved below, with no further full pass. |

### Resolutions after pass 4

| # | Finding | Resolution | Status |
|---|---|---|---|
| R1 | Key rotation: a lookup must match a key under every active key version | Prove, then decide. Proven against Postgres 18.6. | accepted as D32, which D4 (c) later simplified |
| R2 | Creating a workspace fails RLS, because its ID doesn't exist yet | Prove, then decide. Proven: generating the ID first and opening the unit of work in that workspace context passes RLS. | accepted as D33, which D4 (c) later simplified |
| R3 | An invitee can't see or accept an invitation under RLS | Prove, then decide. Proven: an "addressed to the caller's email" policy plus a switch into the invitation's workspace context passes RLS. | accepted as D34, which D4 (c) later simplified |
| R4 | Erasure deletes the access-change event that should close a deleted user's streams (SEC-06, DATA-28) | Decide | accepted as D35 |
| R5 | ARCH-11 says a commit means success, but a recorded security failure commits and answers 401 (ARCH-11, DATA-27) | Decide | accepted as D36 |

### Queued rule proposals

Handled in the rule fixes (Build order, step 3), as in [Changing a rule](standards/README.md#changing-a-rule).

| From | Proposal |
|---|---|
| Pass 3 | An OPS rule for D14: TLS on every connection that leaves the host, and storage encrypted at rest as a stated deployment requirement. |
| `platform::db` | DATA-13: state the whole check PLATFORM-009 makes: the role's name, its attributes, what it owns, and the roles it belongs to. |
| `platform::db` | DATA-21: the check bans `sqlx::Pool`, `sqlx::PgConnection` and `sqlx::Transaction` everywhere except `platform::db` and the test support, not only `PgPool` and `PgConnection` in `app`. |
| `platform::db` | D20: drop "runtime threads come from configuration". The async runtime sizes itself to the CPUs the container may use, so no two deployments would set it differently (OPS-02). |
| Parallel review | Right after the rule fixes, renumber rule and behavior IDs once, closing the gaps. The rule fixes retire the last large group of IDs, and every later test would carry an old number. From then on, IDs are never reused. |
| Decision trace | DOC-04: extend the migration workspace's exception to CLAUDE.md's pointer to `docs/inventory/`. |

### Open design questions

| Question | Settled by |
|---|---|
| AUDIT-005 gives `app` only `INSERT` and `SELECT` on `audit_entries`. Retention (D6) deletes entries and erasure (D31) anonymizes them, and every job runs as `app`. Which role runs them, or which narrower grant allows them? | the full `audit` module's spec |

## Cross-cutting decisions

From [inventory/SYNTHESIS.md](inventory/SYNTHESIS.md). [coverage.md](coverage.md) maps every foundational area to one of them. Each accepted decision becomes an ADR.

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

## Owner questions

| ID | Question | Status |
|---|---|---|
| Q1 | Which license does the template ship under? | Deferred. It blocks the first public release, not the build. Until then the repo stays private, and `cargo-deny` allows only permissive dependency licenses, so selling and open-sourcing both stay possible. |
| Q2 | Should the API offer a user data export? | Yes, as D31. |
