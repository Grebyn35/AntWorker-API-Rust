# Migration tracker

Tracks the rewrite from `AntWorker-API` (commit `0210895`) to this repository.

## Spec documents

| Document | Status |
|---|---|
| [architecture.md](architecture.md) | draft |
| [standards/](standards/README.md) | Review stopped after pass 4; its resolutions are D32 to D36. Tenancy is enforced by the application, with no row-level security (D4). 160 rules. |
| [spec/](spec/README.md) | The first slice's nine specs are written. The triage behind them is in [inventory/triage-slice.md](inventory/triage-slice.md). |
| `docs/adr/` | Not started. Written after the rule fixes (Build order, step 2), from the rules as they then stand. The recommendation text in SYNTHESIS.md predates the standards, and [inventory/decision-trace.md](inventory/decision-trace.md) lists where the rules refine it. ADR-00NN records decision DNN. Before `docs/inventory/` is deleted, every `DNN` citation is replaced with a link to its ADR. |
| [CLAUDE.md](../CLAUDE.md) | written |

## Current work

**Built:**
- The workspace scaffold (`platform` and `app` crates), strict configuration with the startup-failure format (PLATFORM-023, OPS-03), and the liveness probe (PLATFORM-019).
- `platform::db`: Postgres 18 in `compose.yaml`; the `bootstrap` and `migrate` subcommands (PLATFORM-010, PLATFORM-027 to PLATFORM-029); the startup checks of the role and the schema (PLATFORM-009, PLATFORM-030); the unit of work; the first migration, with the default privileges (DATA-19); the test support, with one database per test (TEST-06); and the committed query cache (DATA-20).

`just ci` passes locally and in GitHub Actions, where CI starts Postgres through `just setup` and Compose: formatting, clippy, 41 tests and `cargo-deny`.

**Next, in order.** Each mechanism of `platform` is built together with the first module that calls it, and each step ends with `just ci` passing:
1. `accounts`, `auth` and the `audit` writer, with the HTTP errors (D17), authentication (`Principal` and `SessionVerifier`) and the per-email rate limit. The step starts with `check-test-map`, the first custom CI check, which reports every behavior that no test proves, so the gap between the specs and the code is a report rather than a judgment. It also settles the `auth` items in [inventory/decision-trace.md](inventory/decision-trace.md).
2. `access` and `workspaces`, with `Authorized<P>`, the security event sink, and idempotency, which creating a workspace uses.
3. Jobs, `mail` and `invitations`.
4. The event hub and `realtime`, ending with "removing a member closes their stream".

Each custom CI check is built in the first step that has code for it to check: `check-tenancy`, `check-deps`, `check-layers`, `check-test-map`, `check-schema` and `openapi`.

## Build order

The design is settled by building, not by further review of the documents. Until a part is built, its spec and its sections of architecture.md are the plan, not a description of code. When building shows the plan is wrong, the plan is corrected in the same change as the code, and a rule changes through [Changing a rule](standards/README.md#changing-a-rule). A behavior counts as proven only when a test named by its ID passes.

1. **Thin slice.** Spec it, then build it test-first:
   - `platform`: the mechanisms in `docs/spec/platform.md`, each built with the first module that calls it.
   - `auth`: registration, sign-in and sessions.
   - `access` and `workspaces`: creating a workspace, and removing a member, which closes that member's open stream.
   - `invitations`: inviting a member, and accepting the invitation.

   Together these exercise every mechanism the document reviews couldn't settle: D32 to D36, idempotency, jobs, email and the real-time hub.

   Three constraints apply to the slice:
   - **Tenancy is enforced by the application.** Every query filters by tenant itself, checked by `check-tenancy` and by the foreign-workspace probes (TEST-09). There is no row-level security (D4).
   - **A short list of custom CI checks.** Alongside the standard tools, the slice builds only `check-deps`, `check-layers`, `check-schema`, `check-tenancy`, `openapi` and `check-test-map`, as one checker tool.
   - **A typed client generated from `openapi.json`.** The integration tests call the API through it, and the desktop client uses it later.
2. **Rule fixes.** Apply one round of rule changes from what the slice taught, through [Changing a rule](standards/README.md#changing-a-rule). The round deletes rules as well as fixing them: a rule that no code needed, or that only restates another, goes. It records the number of rules, behaviors and CI checks before and after.
3. **Afternoon test.** A developer or agent new to the codebase, using only the docs, adds a small feature to the slice. Anything that slows them down is fixed by removing complexity, not by adding it.
4. **Remaining modules, one at a time.** Each module's spec is written just before the module is built, and carries the commitments that [inventory/decision-trace.md](inventory/decision-trace.md) lists for it.
5. **Close the migration.** architecture.md is rewritten to describe the system as built (DOC-01), the ADRs are written, and `docs/inventory/` and this file are deleted.

Each CI check is built when the code it checks exists. A check that never catches a real problem is removed.

## Modules

Each module goes through these stages, in order:

1. **Triage:** every old behavior it draws on has a verdict: kept, changed or dropped.
2. **Spec:** its spec is written ([spec/](spec/README.md)).
3. **Tests:** every behavior in the spec has a test named by its ID.
4. **Code:** the module is implemented.
5. **Conformance:** a review against the spec and the standards finds no violation. Every finding cites a rule ID or behavior ID.

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

Handled in the rule fixes (Build order, step 2), as in [Changing a rule](standards/README.md#changing-a-rule).

| From | Proposal |
|---|---|
| Pass 3 | An OPS rule for D14: TLS on every connection that leaves the host, and storage encrypted at rest as a stated deployment requirement. |
| `platform::db` | DATA-13: state the whole check PLATFORM-009 makes: the role's name, its attributes, what it owns, and the roles it belongs to. |
| `platform::db` | DATA-21: the check bans `sqlx::Pool`, `sqlx::PgConnection` and `sqlx::Transaction` everywhere except `platform::db` and the test support, not only `PgPool` and `PgConnection` in `app`. |
| `platform::db` | CODE-10: the role `app` and the crate `app` share a name. Rename the role, for example to `api`. |
| `platform::db` | D20: drop "runtime threads come from configuration". The async runtime sizes itself to the CPUs the container may use, so no two deployments would set it differently (OPS-02). |
| Parallel review | Before the first release, renumber rule and behavior IDs once, closing the gaps the document reviews left. From then on, IDs are never reused. |
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
