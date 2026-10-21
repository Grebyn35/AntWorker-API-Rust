# Migration tracker

Tracks the rewrite from `AntWorker-API` (commit `0210895`) to this repository.

## Spec documents

| Document | Status |
|---|---|
| [architecture.md](architecture.md) | draft |
| [standards/](standards/README.md) | Review stopped after pass 4; its resolutions are D32 to D36. Tenancy is enforced by the application, with no row-level security (D4). |
| [spec/](spec/README.md) | The first slice's nine specs are written. The triage behind them is in [inventory/triage-slice.md](inventory/triage-slice.md). |
| `docs/adr/` | not started. ADR-00NN records decision DNN. Before `docs/inventory/` is deleted, every `DNN` citation is replaced with a link to its ADR. |
| [CLAUDE.md](../CLAUDE.md) | written |

## Current work

**Built:** the workspace scaffold (`platform` and `app` crates), strict configuration with the startup-failure format (PLATFORM-023, OPS-03), and the liveness probe (PLATFORM-019). `just ci` passes locally: formatting, clippy, 9 tests and `cargo-deny`.

**Blocked:** GitHub Actions doesn't start, because the account's billing needs attention (a failed payment or a spending limit). The workflow is unverified until a run starts.

**Next, in order:**
1. `platform::db`: Postgres 18 in `compose.yaml`, the first migrations for the slice's tables, the `bootstrap` subcommand (PLATFORM-010), the startup role check (PLATFORM-009), the unit of work (PLATFORM-024), and the test harness with one database per test (TEST-06).
2. `accounts` and `auth`, test-first from their specs.
3. `access` and `workspaces`, then `audit`, `mail`, `invitations` and `realtime`, with the rest of `platform` (errors, jobs, events, idempotency, rate limits) built as each needs it.
4. The first custom CI checks: `check-tenancy`, `check-deps`, `check-layers`, `check-test-map`, `check-schema` and `openapi`.

## Build order

The design is settled by building, not by further review of the documents.

1. **Thin slice.** Spec it, then build it test-first:
   - `platform`: the mechanisms in `docs/spec/platform.md`.
   - `auth`: registration, sign-in and sessions.
   - `access` and `workspaces`: creating a workspace, and removing a member, which closes that member's open stream.
   - `invitations`: inviting a member, and accepting the invitation.

   Together these exercise every mechanism the document reviews couldn't settle: D32 to D36, idempotency, jobs, email and the real-time hub.

   Three constraints apply to the slice:
   - **Tenancy is enforced by the application.** Every query filters by tenant itself, checked by `check-tenancy` and by the foreign-workspace probes (TEST-09). There is no row-level security (D4).
   - **A short list of custom CI checks.** Alongside the standard tools, the slice builds only `check-deps`, `check-layers`, `check-schema`, `check-tenancy`, `openapi` and `check-test-map`, as one checker tool.
   - **A typed client generated from `openapi.json`.** The integration tests call the API through it, and the desktop client uses it later.
2. **Rule fixes.** Apply one round of rule changes from what the slice taught, through [Changing a rule](standards/README.md#changing-a-rule).
3. **Afternoon test.** A developer or agent new to the codebase, using only the docs, adds a small feature to the slice. Anything that slows them down is fixed by removing complexity, not by adding it.
4. **Remaining modules, one at a time.** Each module's spec is written just before the module is built.

Each CI check is built when the code it checks exists. A check that never catches a real problem is removed.

## Modules

Each module goes through these stages, in order:

1. **Triage:** every old behavior it draws on has a verdict: kept, changed or dropped.
2. **Spec:** its spec is written ([spec/](spec/README.md)).
3. **Tests:** every behavior in the spec has a test named by its ID.
4. **Code:** the module is implemented.
5. **Conformance:** a review against the spec and the standards finds no violation. Every finding cites a rule ID or behavior ID.

"First slice" means the stage is done for the behaviors the first slice builds.

| Module | Draws on inventory | Triage | Spec | Tests | Code | Conformance |
|---|---|---|---|---|---|---|
| `platform` | HTTP, DATA, RT, OBS, OPS | first slice | first slice | | | |
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

Handled after the review completes, as in [Changing a rule](standards/README.md#changing-a-rule).

| From | Proposal |
|---|---|
| Pass 3 | An OPS rule for D14: TLS on every connection that leaves the host, and storage encrypted at rest as a stated deployment requirement. |

### Open design questions

None.

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
