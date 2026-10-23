# Migration tracker

Tracks the rewrite from `AntWorker-API` (commit `0210895`) to this repository.

## Spec documents

| Document | Status |
|---|---|
| [architecture.md](architecture.md) | draft |
| [standards/](standards/README.md) | 160 rules. Proposed changes wait in [Queued rule proposals](#queued-rule-proposals). |
| [spec/](spec/README.md) | The first slice's nine specs are written. The triage behind them is in [inventory/triage-slice.md](inventory/triage-slice.md). |
| `docs/adr/` | Not started. Written after the rule fixes (Build order, step 3), from the rules as they then stand. The recommendation text in SYNTHESIS.md predates the standards, and [inventory/decision-trace.md](inventory/decision-trace.md) lists where the rules refine it. ADR-00NN records decision DNN. Before `docs/inventory/` is deleted, every `DNN` citation is replaced with a link to its ADR. |
| [CLAUDE.md](../CLAUDE.md) | written |

## Current work

**Built:**
- The workspace scaffold (`platform` and `app` crates), strict configuration with the startup-failure format (PLATFORM-023, OPS-03), and the liveness probe (PLATFORM-019).
- `platform::db`: Postgres 18 in `compose.yaml`; the `bootstrap` and `migrate` subcommands (PLATFORM-010, PLATFORM-027 to PLATFORM-029); the startup checks of the role and the schema (PLATFORM-009, PLATFORM-030); the unit of work; the first migration, with the default privileges (DATA-19); the test support, with one database per test (TEST-06); and the committed query cache (DATA-20).
- The review tools (build order 1.1): the checker in `xtask`, with `check-test-map` and `check-docs`, and the conformance reviewer in `.claude/agents/`.

`just ci` passes locally and in GitHub Actions, where CI starts Postgres through `just setup` and Compose: formatting, clippy, 71 tests, `cargo-deny`, `check-test-map` and `check-docs`. A scheduled workflow runs `cargo-deny` every day.

**Next:** step 1.2 of the build order, the request pipeline. Each part ends with `just ci` passing.

## Build order

The design is settled by building, not by further review of the documents. How the plan for unbuilt parts is kept, corrected and proven is in [CLAUDE.md](../CLAUDE.md), under "Docs for unbuilt parts are the plan".

From the end of step 1 on, the system is always deployable and observable: every change runs in the image CI publishes, the reference deployment rolls it out, and its telemetry reaches the observability stack. A step that adds a behavior also adds what operating it takes: its metrics and the dashboard questions they answer, its alerts and their runbooks, and its end-to-end scenarios.

Every part of the system is listed under [Foundation](#foundation), [Modules](#modules) or [CI checks](#ci-checks), with the step that builds it. Each part is specified just before it is built, and ends with a conformance review of what it built or changed ([Conformance reviews](#conformance-reviews)).

1. **Walking skeleton.** The running system, end to end, before any feature:
   1. **Review tools:** the checker tool in `xtask` and the conformance reviewer in `.claude/` (D27). The checker starts with `check-test-map`, which reports every behavior that no test proves, and `check-docs`, which fails on any ID or link that doesn't resolve (DOC-13) and on any check a rule names that neither [operations/ci.md](operations/ci.md) nor [CI checks](#ci-checks) lists.
   2. **Request pipeline:** problem responses with trace IDs, trace context in and out, the middleware stack, readiness and bounded shutdown, and the capabilities endpoint as the first `/v1` route, with `openapi.json`. It starts with the typed-client spike: before any test calls the API through a client generated from `openapi.json`, a throwaway test compares the candidate generators, including their licenses (CODE-12) and the OpenAPI version they read.
   3. **Telemetry:** traces, logs and metrics exported over OTLP; the metric declaration macro; the collector, Prometheus, Loki, Tempo, Grafana and Alertmanager, with logs and traces linked both ways; the first dashboard questions and alerts; and the generated metric catalog. `just dev` runs the API with the stack one flag away.
   4. **Image and pipeline:** the image, and CI that builds it once, scans it, boots it, runs the end-to-end suite against it, and promotes its digest, with signed releases on tags.
   5. **Reference deployment:** the production setup on one server, its deploy tooling, scheduled backups and restore, and the load test. The end-to-end suite proves a deploy with no downtime, a rollback, a restore, and the trace-ID guarantee for a 4xx response (D10).
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
6. **Close the migration.** The remaining parts under [Foundation](#foundation) are built, and one conformance review covers the whole codebase, under the same stopping rule. Then architecture.md is rewritten to describe the system as built (DOC-01), and `docs/inventory/` and this file are deleted.

Each CI check is built when the code it checks exists. A check that never catches a real problem is removed.

## Foundation

The parts that aren't modules, from [architecture.md](architecture.md) and the documentation set (DOC-08). [inventory/decision-trace.md](inventory/decision-trace.md) lists the commitments each one carries. A part's step reads "built" once it is.

| Part | Decisions | Step |
|---|---|---|
| The checker tool and the conformance reviewer | D23, D27 | built |
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
| `platform` | HTTP, DATA, RT, OBS, OPS | database | first slice | database | database | database |
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

## Conformance reviews

Each pass of each review, run by the conformance reviewer under "When the review stops" in [standards/README.md](standards/README.md#when-the-review-stops).

Reviewer A reads rule by rule, and reviewer B file by file. "Shared" counts the findings both made.

| Scope | Pass | Findings (A, B, shared) | Must-fix | Must-fix regressions | Result |
|---|---:|---|---:|---:|---|
| `platform::db` | 1 | 14 (one reviewer) | 8 | n/a | Six code defects fixed, each with a test. Five doc statements corrected. Five rule wordings queued. |
| `platform::db` | 2 | 6 (3, 4, 1 shared) | 2 | 1 | Stopped by its must-fix regression. Both must-fix findings fixed, each with a test that failed first. The should-fix findings fixed, or queued as rule changes. |

## CI checks

Every check a rule names, and the step that builds it. [operations/ci.md](operations/ci.md) lists the checks that exist. Until a check exists, its rule is enforced by review.

| Check | Step |
|---|---|
| `fmt-check`, `test`, `deny` | built |
| `check-test-map`, `check-docs` | built |
| `check-docs`, for the names of other codebases (DOC-04) | 3, with the queued DOC-04 proposal |
| `check-docs`, for accepted ADRs (DOC-11) | 6, with the first ADR |
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

## Queued rule proposals

Handled in the rule fixes (Build order, step 3), as in [Changing a rule](standards/README.md#changing-a-rule).

| From | Proposal |
|---|---|
| Standards review | An OPS rule for D14: TLS on every connection that leaves the host, and storage encrypted at rest as a stated deployment requirement. |
| `platform::db` | DATA-13: state the whole check PLATFORM-009 makes: the role's name, its attributes, what it owns, and the roles it belongs to. |
| `platform::db` | DATA-21: the check bans `sqlx::Pool`, `sqlx::PgConnection` and `sqlx::Transaction` everywhere except `platform::db` and the test support, not only `PgPool` and `PgConnection` in `app`. |
| `platform::db` | D20: drop "runtime threads come from configuration". The async runtime sizes itself to the CPUs the container may use, so no two deployments would set it differently (OPS-02). |
| Parallel review | Right after the rule fixes, renumber rule and behavior IDs once, closing the gaps. The rule fixes retire the last large group of IDs, and every later test would carry an old number. From then on, IDs are never reused. |
| Decision trace | DOC-04: extend the migration workspace's exception to CLAUDE.md's pointer to `docs/inventory/`. |
| Conformance of `platform::db` | DATA-02: the standard columns, and the naming in DATA-01, apply to the tables that migrations create. The migrator's own table, `_sqlx_migrations`, follows sqlx's layout. |
| Conformance of `platform::db` | DATA-12: state what PLATFORM-010 states. Running `bootstrap` again keeps the roles, their attributes and the grants, and sets each password again from its URL. `owner` owns the tables it creates and has `CREATE` on the schema `public`, which the database's owner keeps. |
| Conformance of `platform::db` | SEC-01: "secrets never appear in URLs" covers the URLs the API sends, serves or logs. A database URL that carries a password is a secret setting as a whole: read from the environment or a file, held in `Secret`, and never printed. |
| Conformance of `platform::db` | TEST-06: tests of `migrate` and of `serve`'s schema check start from an empty database instead of the migrated template; tests may arrange state through the administrator's connection; and assertions on database catalogs, such as role attributes, may read through `app`. |
| Conformance of `platform::db` | DATA-09: say whether a migration that has been applied but is in no release may be edited. A file edited after it was applied fails `migrate` and `serve` in every database that applied it. |
| Conformance of `platform::db` | TEST-01: say whether each clause of a behavior needs a test of its own. |
| Conformance of `platform::db` | CODE-05: say whether the test support may block, as a test's cleanup and settings do. |
| Owner question on duplicated statements | A DOC rule: a fact lives in one place, and other documents point to it instead of restating it. The standards README states this for reviews' fixes, without an ID a finding can cite. |
| Conformance of `platform::db` | TEST-04: a level for behaviors with no HTTP surface, such as the subcommands and the startup checks, whose integration tests call `platform` directly or run the binary. |
| Conformance of `platform::db` | SEC-01: `DatabaseUrl::connect_options` is public, and the options it returns print the password through `Debug`. Narrow what it exposes. |
| Owner's direction on tools | A rule states a guarantee, and names a tool only where the tool is the guarantee, such as the cryptography allowlist in SEC-14. Reword the rules that name a tool for an unbuilt part: SEC-11 (Caddy), OPS-19 (SeaweedFS and Mailpit), TEST-05 (Mailpit), and the library names in ERR-04 and SEC-09. MAIL-005 is reworded when `mail` is built. |

## Open design questions

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
