# Migration tracker

Tracks the rewrite from `AntWorker-API` (commit `0210895`) to this repository. It holds status and pointers only (DOC-16). Every other fact lives in its home (DOC-14).

## Current work

**Next:** the rest of step 3, the rule fixes: the threat model; `check-config` and `setup-smoke`; the OPS-17 and OPS-22 gaps and proposals; OPS-21's test; the DATA-12 and DATA-13 proposal on refusing a shared managed server; the deletion pass over OBS, OPS and SEC; a review of that part; then the renumbering. The review of 3284003..9de71a7, the round's rules before it, stopped at its second pass and is resolved. 3c57546's message holds the counts the round's last commit compares with. Nothing waits on the owner. A development database made before 2e0e179 is recreated, as CLAUDE.md says. The trial server runs 0.1.0-rc.11, its Grafana Cloud stack's alerts reach no contact point yet, and the first restore after its next release, whose deploy needs `--allow-restart`, measures the checks' waits that backups.md leaves unmeasured. The owner releases when it needs `main`.

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
   1. `accounts`, `auth` and the `audit` writer, with authentication (`Principal` and `SessionVerifier`) and rate limiting: the IP and per-user limits and the per-email sign-in limit, built once with all three callers. Authentication reads the Bearer header and, from browsers, a cookie, with CSRF protection and the CORS allowlist (D61). It settles the `auth` items in [inventory/decision-trace.md](inventory/decision-trace.md), and extends the trace-ID guarantee to a 5xx response.
   2. `access` and `workspaces`, with `Authorized<P>`, the security event sink, and idempotency, which creating a workspace uses.
   3. Jobs, email and `invitations`.
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
| The capabilities endpoint and `openapi.json` | D8, D9, D68 | built |
| Rate limiting: the IP, per-user and per-action limits, with the client IP taken from trusted proxies | D11 | built |
| Telemetry, the observability stack and `just dev`, with the observability docs, the metric catalog and the runbooks | D10, D19, D28 | built |
| The image and the delivery pipeline, with the release docs | D15, D21, D43 | built |
| The reference deployment, backups and the load test, with the deploy and backup docs | D15, D20, D30, D44, D45, D47, D49, D50, D51, D52, D53, D54, D55, D56, D57, D59, D60, D74, D75 | built |
| Grafana on an HTTPS address of its own with sign-in, and the path to a managed observability backend through the collector, with Grafana Cloud as the worked example | D19, D42 | built |
| The command that removes an optional part, which `check-removable` runs | D29, D39 | built |
| `just seed` and the API reference in development | D28, D69, D71 | built |
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
| `platform` | HTTP, DATA, RT, OBS, OPS, MAIL | database, request pipeline | first slice | database, configuration, request pipeline, lifecycle, telemetry, authentication, rate limits, authorization, idempotency, events and the hub, lists, jobs, mail | database, configuration, errors, types, request pipeline, lifecycle, telemetry, authentication, rate limits, authorization, idempotency, events and the hub, lists, jobs, mail | database, configuration, errors, types, request pipeline, lifecycle, telemetry, authentication, rate limits, authorization, idempotency, events and the hub, lists, jobs, mail |
| `accounts` | USER | first slice | first slice | first slice | first slice | first slice |
| `auth` | AUTH | first slice | first slice | first slice | first slice | first slice |
| `access` | WS | first slice | first slice | first slice | first slice | first slice |
| `workspaces` | WS | first slice | first slice | first slice | first slice | first slice |
| `invitations` | WS, MAIL | first slice | first slice | first slice | first slice | first slice |
| `audit` | AUDIT | first slice | first slice | the writer, the security event sink | the writer, the security event sink | the writer, the security event sink |
| `realtime` | RT | first slice | first slice | first slice | first slice | first slice |
| `files` | FILE | | | | | |
| `client_telemetry` | OBS | | | | | |
| `superadmins` | none: a new part (D62) | | | | | |
| `system` | HTTP, OPS | capabilities, seed | capabilities, seed | capabilities, seed | capabilities, seed | capabilities, seed |

## CI checks

Every check a rule names, and the step that builds it. [operations/ci.md](operations/ci.md) lists the checks that exist. Until a check exists, its rule is enforced by review.

| Check | Step |
|---|---|
| `fmt-check`, `test`, `deny` | built |
| `check-test-map`, `check-docs` | built |
| `check-docs`, for accepted ADRs (DOC-11) | 6, with the first ADR |
| `check-openapi`, with list shapes, `check-instrument`, `check-layers`, `check-deps`, `check-schema`, `check-sql-bans`, `check-tenancy` | built |
| `check-metrics`, `check-telemetry-config`, `alert-rules` | built |
| `check-openapi`, for the breaking-change diff (API-18, ERR-02) | the first final release |
| `check-pins`, `image-scan`, `image-rescan`, `verify-release`, `check-shell`, `setup-smoke` | built |
| `e2e` | built, and extended by every later step |
| `check-personal-data` | 5, with export and erasure in `accounts` |
| `check-removable`, `check-parts`, `check-table-ownership`, `check-migrations`, `sqlx-check`, `check-layout` | built |
| `test-migrations` | 6, with the first final release |
| `check-rename` | 6, with `just rename` |

## Known gaps

Built code that doesn't follow a rule or a behavior yet ([Changing a rule](standards/README.md#changing-a-rule)).

| Rule or behavior | Built code that doesn't follow it | Closed by |
|---|---|---|
| OPS-18 | A release's capacity, which `just load-test` measures, has no release notes to go in: `CHANGELOG.md` isn't written | 6, with `CHANGELOG.md` |
| TEST-07 | `traces::obs_08_the_trace_id_of_a_4xx_response_leads_to_its_logs_and_its_trace` fails now and then on amd64 in CI, when nothing reaches Loki or Tempo; its output says why, from the stack's start times to the API's export errors | the fix of what its next failure shows |
| TEST-07 | `invitations::sec_17_member_joined_reaches_the_members_and_no_replay_from_before_joining` failed once in `just test` on 2026-11-03, and passed alone five times: a stream that opened with 200 ended its response without `stream.closed`, which the route always sends before it ends; the support's `event` now names what the stream held when it ended | the fix of what its next failure shows |
| DOC-12 | A release has no upgrade notes: `CHANGELOG.md` isn't written | 6, with `CHANGELOG.md` |
| OPS-17 | Postgres treats a segment of the log that its restore command can't fetch as the end of the archive, so a restore whose replay can't fetch one, as when the bucket stops answering, ends the replay there: a restore of the latest state, or the second replay of one to a moment, then serves the database as of that segment, naming its last commit, rather than refusing | 3 |
| DATA-28 | No personal data contract is registered for the tables that reference a user or hold personal data, `users`, `identities`, `sessions`, `audit_entries`, `workspaces`, `workspace_memberships`, `events`, `idempotency_records`, `jobs` and `invitations`: the registry, and the export and erasure that read it, arrive with the full `accounts` module (D31) | 5 |
| API-04 | A created workspace's `Location`, `/v1/workspaces/{id}`, names a resource that no route serves yet: reading one workspace arrives with the full module | 5, with the full `workspaces` module |
| API-04 | A created invitation's `Location`, `/v1/workspaces/{workspace_id}/invitations/{id}`, names a resource that no route serves yet: reading one invitation arrives with the full module | 5, with the full `invitations` module |
| OPS-17 | Backups expire by pgBackRest's rule, which reads no timelines: a full backup expires once the next full backup is `BACKUP_RETENTION_DAYS` old. After a restore that went back past a full backup, the moments of the new timeline before its first backup ends restore only from the backup the restore started from, whose next full backup lies on the abandoned timeline and is already old, so they leave the reach early, by up to most of the retention: with 28 days and weekly full backups, a restore that goes back 26 days leaves them reachable for about 4 days. backups.md's "never shorten the reach" doesn't hold for them | 3 |
| ARCH-07 | `just remove` knows `invitations` only: users creating workspaces through `POST /v1/workspaces` isn't marked as a part, since no run of `check-removable` can remove it until a part that also creates workspaces, the setup subcommand or `superadmins`, is built | 5, with the setup subcommand or `superadmins` |
| ARCH-07 | `invitations` owns the hub's load test, `just load-test streams`, and the sentences of OPS-18, the platform spec and release.md on it, which exist for the hub: its events are `invitation.received`, since no core module publishes an event that changes no one's access, so a fork without the part loses the hub's measurement | 5, with the first event of a core module that changes no one's access, such as one announcing a workspace's new name |
| ARCH-07 | `askama` is a dependency that only `invitations` uses, since its emails are the only ones yet, so removing the part removes it, while architecture.md §3 and §10 and the platform spec's Emails, which describe how every module's emails are built, still name it and `crates/app/templates/` | 5, with the full `auth` module, whose emails make it a dependency of the core |

## Rule proposals

Proposals that wait, each with the step that decides it ([Changing a rule](standards/README.md#changing-a-rule)).

| From | Proposal | Decided in |
|---|---|---|
| Conformance review of 3ae7168 | ARCH-07: whether password sign-in is an optional part, for a fork that signs in only through OAuth providers | 5, with the full `auth` module |
| The owner's question at step 3's start | SYSTEM-001: whether `features` also holds each optional part that changes what a client offers, such as `invitations`, `true` while the fork keeps the part and absent once it removes it, so that one client tells the workspace shapes apart from what each API has, rather than a field that names the shape, which a fork that keeps `superadmins` in any shape, or adds a part of its own, couldn't state | 5, with the setup subcommand or `superadmins` |
| Deciding D73 | ARCH-07: whether a part adds what it needs to the core modules through registration points they offer, such as its permission in the role matrix and its steps in `seed`, so that fewer of its lines are marked in their files, once step 5's parts show which of them recur | 5 |
| Conformance review of 3c57546 | D62, architecture §5: how a superadmin who created a workspace hands it over in a fork without `invitations`, where nobody else joins it, since an ownership transfer swaps the roles of two members | 5, with `superadmins` |
| Conformance review of 3c57546 | ARCH-07: how a part's panel leaves a dashboard that other parts share, whose JSON holds no comment for a marker, or whether a part's panels go in a dashboard it owns whole | 5, with the first part that declares a metric |
| Conformance review of 3c57546 | ARCH-07: `check-parts` also reads the tests in the files of a part without a module, such as an OAuth provider's, which it skips now | 5, with the first OAuth provider |
| Deciding D73 | ARCH-07: whether a fork chooses its shape with one command, which removes the parts the shape doesn't keep, then the markers of the parts it keeps, the runs, and the removal's tooling and checks, since a fork that keeps every part removes nothing, and its own code that names a kept part fails `check-parts` and `check-removable` until it carries the part's marker | 6, with `just rename` |
| The owner's decision on second factors (D63) | Whether a second factor can be required beyond superadmins: of every user, by a setting of the deployment, or of a workspace's members, by its owner, as Slack and GitHub let an organization require it of its members | 5, with the full `auth` module |

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
| a28df7b..1d13d63: step 1.5 after its review: status and logs, the trial's fixes, the managed Grafana import, the deploy lock and the put-back, server-setup and server-check, and the proxy's answer while no replica runs | 1 | 17 (13, 7, 3 shared) | 8 | 0 | `next pass`: 3392e71 |
| a28df7b..3392e71: step 1.5 after its review, as above | 2 | 7 (6, 1, 0 shared) | 2 | 0 | `next pass`: 4718850 |
| a28df7b..4718850: step 1.5 after its review, as above | 3 | 4 (4, 1, 1 shared) | 1 | 1 | `stopped: no progress`: a040339 |
| 57f140d..ed3dec5: the commits since step 1.5's review was decided: the wait for a managed Grafana that is still loading, the verify job's limit, a server that nothing a deploy leaves fills (D57), a release of unpushed commits in one run, a deploy of the latest release by default with the server's SSH `Host`, and the justfile's task list | 1 | 7 (6, 2, 1 shared) | 1 | 0 | `next pass`: f75e989 |
| 57f140d..f75e989: the commits since step 1.5's review was decided, as above | 2 | 7 (3, 5, 1 shared) | 3 | 1 | `stopped: no progress`: 21ecbdb |
| b299bf2..9a48aa8: a restore to a moment that an earlier restore went back past, and the task list's lines of `just e2e` and `just deploy` | 1 | 4 (3, 2, 1 shared) | 1 | 0 | `next pass`: 004d477 |
| b299bf2..004d477: as above, with the plan of a restore to a moment | 2 | 3 (3, 1, 1 shared) | 1 | 0 | `stopped: no progress`: 903739d |
| 903739d..860374f: a restore that reaches every moment up to its own start, and names the moment that undoes it (D58) | 1 | 6 (6, 1, 1 shared) | 3 | 0 | `next pass`: f7f3c46 |
| 903739d..f7f3c46: as above | 2 | 6 (5, 2, 1 shared) | 2 | 1 | `stopped: no progress`: 6106c8c |
| c5aecac..ba2c653: a restore that refuses while the database's log doesn't reach the bucket (D59) | 1 | 5 (4, 1, 0 shared) | 1 | 0 | `next pass`: a92d7a8 |
| c5aecac..a92d7a8: as above | 2 | 4 (3, 1, 0 shared) | 2 | 1 | `stopped: no progress`: ef662a6 |
| 30c65a1..c7a6431: a restore that reaches a moment that no commit or rollback in the bucket follows on its timeline (D60) | 1 | 4 (3, 1, 0 shared) | 2 | 0 | `next pass`: f0c9c2f |
| 30c65a1..f0c9c2f: as above | 2 | 4 (4, 1, 1 shared) | 2 | 0 | `stopped: no progress`: d2d1ca4 |
| a912c96..cfe2dcd: the end-to-end suite's fixed waits, Postgres's archiver on arm64, its health check and the programs its image runs, and a restore while a one-off of Postgres runs or Postgres replays | 1 | 3 (3, 0, 0 shared) | 2 | 0 | `next pass`: 6587c89 |
| a912c96..6587c89: as above | 2 | 1 (1, 0, 0 shared) | 0 | 0 | `clean`: a44f517 |
| 97c76dc..c3fb265: a restore that tells a replay ending short of its moment from a start that fails, and the tests' lock on the roles held only until a deploy starts its replicas | 1 | 0 (0, 0, 0 shared) | 0 | 0 | `clean` |
| c3fb265..e5d777c: the arm64 end-to-end tests on an arm64 runner, the runners that `check-pins` reads from a matrix, and the close of the review before | 1 | 2 (2, 0, 0 shared) | 0 | 0 | `clean`: eccac06 |
| f0d75e5..a7f2c02: each managed deployment of the end-to-end tests on a Postgres server of its own, with no roles lock on its deploys, and a managed Postgres server that holds no other deployment's database | 1 | 3 (2, 1, 0 shared) | 0 | 0 | `clean`: 09fc97e |
| 43773a5..b77e414: each platform's end-to-end tests split between two CI jobs | 1 | 3 (3, 0, 0 shared) | 0 | 0 | `clean`: 6997584 |
| 4e7c633..cef3b30: decisions D61 to D63, and step 2.1: accounts, auth, the audit writer, authentication, client IP, rate limits, browsers and request bodies, their deployment, metrics and load test, and its CI checks | 1 | 28 (26, 8, 6 shared) | 18 | 0 | `next pass`: 8a121a2 |
| 4e7c633..eac8668: as above | 2 | 9 (7, 4, 2 shared) | 5 | 1 | `stopped: no progress`: 290c430 |
| bd2d1fd..5330bf9: decisions D65 and D66, and step 2.2: access, workspaces, authorization, idempotency, publishing events, lists with search, `check-tenancy` and the list shapes of `check-openapi` | 1 | 30 (27, 11, 8 shared) | 13 | 0 | `next pass`: ec21099 |
| bd2d1fd..ec21099: as above | 2 | 13 (11, 4, 2 shared) | 5 | 2 | `stopped: no progress`: ebdfdc7 |
| d678a43..4eedca6: decision D67, and step 2.3's first part: jobs, `mail` and `invitations`, their deployment, alerts and end-to-end scenario, and the 0BSD license | 1 | 26 (24, 8, 6 shared) | 18 | 0 | `next pass`: 364d2e9 |
| d678a43..364d2e9: as above | 2 | 10 (9, 5, 4 shared) | 2 | 2 | `stopped: no progress`: 5bdf7a5 |
| 317909b..259b888: step 2.3's second part: sending email in `platform`, emails in their modules, `just remove` and `check-removable`, with `invitations` as the first part | 1 | 8 (4, 4, 0 shared) | 2 | 0 | `next pass`: 34b65f5 |
| 317909b..34b65f5: as above | 2 | 3 (2, 2, 1 shared) | 1 | 1 | `stopped: no progress`: 500b68d |
| 0d036d0..d59e8d0: step 2.4's first part: the event hub and `realtime`, their deployment, metrics, alert and end-to-end scenario, and the fixes made before the review | 1 | 21 (20, 16, 15 shared) | 14 | 0 | `next pass`: 29a0c2c, 1ff84e6 |
| 0d036d0..1ff84e6: as above | 2 | 15 (10, 10, 5 shared) | 8 | 5 | `stopped: no progress`: f7eb030 |
| c619d50..92a1d55: step 2.4's second part: the event catalog, the API reference in development, `seed`, the hub's owed events, the hub's load test, and the `invitations` part's markers on them | 1 | 13 (10, 9, 6 shared) | 8 | 0 | `next pass`: efa110e |
| c619d50..efa110e: as above | 2 | 6 (4, 5, 3 shared) | 4 | 1 | `stopped: no progress`: cdcb2ef |
| 06c2a30..151b5eb: the resolutions of the review before, its handoff, and the warning of a `.env` that `.env.example` has moved past | 1 | 6 (6, 4, 4 shared) | 1 | 0 | `next pass`: f6b1396 |
| 06c2a30..f6b1396: as above | 2 | 4 (3, 3, 2 shared) | 3 | 1 | `stopped: no progress`: e955df8 |
| 3c57546: how an optional part is removed (D73): ARCH-07, `xtask`'s parts and `parts.yaml`, `check-parts` and `check-removable`, and every marker of `invitations` | 1 | 6 (3, 3, 0 shared) | 1 | 0 | `next pass`: a4013a8 |
| 3c57546..a4013a8: as above, with the check that no section of a spec is empty | 2 | 5 (5, 3, 3 shared) | 1 | 1 | `stopped: no progress`: c8c7c0d |
| 3284003..9de71a7: step 3's rule fixes after D73: the proposals of the API, ARCH, DATA, ERR and SEC rules with their checks, those of the CODE, DOC and TEST rules with `check-layout`, the split of `deploy.sh` (D74), the deleted rules, and D75 | 1 | 21 (17, 13, 9 shared) | 8 | 0 | `next pass`: 897c7b3 |
| 3284003..897c7b3: as above, with the split of the justfile | 2 | 16 (14, 8, 6 shared) | 1 | 1 | `stopped: no progress`: b1850a1 |

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
| D18 | Authentication that is ready for web clients | accepted; what it left for later is built with the parts it belongs to (D61) |
| D19 | Observability stack design | accepted |
| D20 | Runtime topology and scaling | accepted |
| D21 | CI pipeline shape, on GitHub Actions | accepted |
| D22 | Module architecture | accepted |
| D23 | Testing strategy | accepted |
| D24 | Data access conventions | accepted |
| D25 | Domain types and input validation | accepted |
| D26 | Secrets and security baseline | accepted; its bar is Level 2 of ASVS 5.0.0, but for requirement 6.3.3 (D63) |
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
| D54 | Every task on the reference deployment's server runs through `just`, and the operations documents may also name commands for the operator to run there over SSH, which only read what no task shows, free disk space, or undo a restore that failed, rather than a `just` task for each (OPS-22) | accepted |
| D55 | The packages that `just server-setup` installs from apt, Docker's among them, are the distribution's current ones, which apt verifies by their repositories' signatures over whatever transport the mirrors serve, rather than pinned versions reached over TLS only, so that the system's security updates reach them (OPS-13, OPS-20) | accepted |
| D56 | Without telemetry, D10 holds for the proxy's 503 through `just logs`, which shows Caddy's lines of the requests it answered itself from the file that the collector reads (OPS-05, OPS-22) | accepted |
| D57 | Nothing a deploy leaves on the reference deployment's server grows without bound: every container's log is capped and rotated, and a deploy whose replicas serve removes the other releases' files and the images of the deployment's repositories that nothing uses or names, keeping the previous release's API image for a rollback, rather than leaving their removal to the operator and an alert on a filling disk. The data a restore moves aside stays until the operator removes it, and `just status` shows it (OPS-16, OPS-22, [deploy.md](operations/deploy.md#the-files-on-the-server)) | accepted |
| D58 | A restore reaches the bundled database as it stood at any moment the retention covers that a commit in the bucket follows on its timeline. One that finds Postgres running the database the bucket backs up, with its log reaching the bucket, commits a transaction of its own first, so it reaches every moment until it began, and names the moment that returns the database to how it found it. A moment that no commit follows, such as one while an earlier restore ran or after a lost server's last archived change, is left to the moment a restore named, or after the server's loss to the latest state, rather than recording each restore's run in the bucket for the plan to reach or refuse such a moment (OPS-17, [backups.md](operations/backups.md#restoring)) | superseded by D60 |
| D59 | A restore that finds Postgres running the database the bucket backs up, with its log not reaching the bucket, as pgBackRest's check finds, refuses before anything stops, and one whose transaction, or the check that archives it, fails once the replicas have stopped starts them again and refuses, unless `--allow-unarchived` lets it go on, rather than going on and saying so: it would leave the changes that the bucket lacks only in the data it moves aside, while the API serves the restored database, and name no moment that undoes it. On a new server, whose empty database the bucket doesn't back up, a restore goes on, as it does while Postgres still replays the log, as after a restore that failed during its replay: a database in recovery takes no writes, and holds only what the bucket gave it (OPS-17, [backups.md](operations/backups.md#restoring)) | accepted |
| D60 | A restore reaches the bundled database as it stood at any moment the retention covers. One that finds Postgres running the database the bucket backs up, with its log reaching the bucket, commits a transaction of its own first, and names the moment that returns the database to how it found it. A moment that no commit or rollback the bucket holds follows on its timeline, such as one while an earlier restore ran, after the moment it named and before its timeline began, or one after a lost server's last archived change, is restored as of the timeline's end in the bucket, which the restore names by its last commit's time, or by the backup it started from when it replayed none: once Postgres ends the replay to such a moment without reaching it, the restore restores the backup again and replays the timeline to its end, waiting for each replay as long as it advances, rather than failing with the API stopped, and leaving the operator to find a moment that an earlier restore named (OPS-17, [backups.md](operations/backups.md#restoring)) | accepted |
| D61 | Web clients are supported beside clients that set their own headers, such as the desktop client: a browser authenticates through an `HttpOnly` cookie, which CSRF protection guards, from the origins a CORS allowlist names, built with `auth` in step 2.1; its stream authenticates through the same cookie, in step 2.4; and its private downloads through signed URLs, with `files` in step 5. Each is built with the part it belongs to, rather than after the API as D18 left them, so no fork that exists by then has to add them by hand ([architecture](architecture.md#8-cross-cutting-mechanisms)) | accepted |
| D62 | A deployment may have superadmins: users whom the operator adds or removes through `just` on the server, never through the API. A superadmin sees every workspace's name, creation time, member count and owner, but not its content, and deletes any workspace, through routes of their own authorized by the caller being a superadmin, never by bypassing a membership. In the provisioned organizations shape, only superadmins create workspaces, a shape that a fork chooses by removing parts, like the others, rather than through a setting. The `superadmins` module is optional (ARCH-07), and is built in step 5 ([architecture](architecture.md#what-each-module-owns)) | accepted |
| D63 | The security bar is Level 2 of version 5.0.0 of ASVS, but for its requirement 6.3.3: every user may add a second factor, and every superadmin must, rather than every user having to, so a product's users can sign in without setting one up; the threat model records the controls that stand in for the requirement ([security rules](standards/security.md)) | accepted |
| D64 | A request that no route serves counts in its client IP's bucket, as every request but the health probes and a listed origin's preflight does, and gets its 404 or 405 problem once counted, rather than before: probing paths counts against the prober's limit, and the one rule stays simple, while a flood of a served route would cost the database the same ([OPS-07](standards/operations.md)) | accepted |
| D65 | The setup subcommand creates a workspace and its owner's account each time an operator runs it, so one organization's deployment can hold a workspace per subsidiary, rather than one workspace in all; a run with a name an earlier run used is refused, so a second run by mistake creates nothing, and an account an earlier run created may own more before its owner signs in ([workspaces spec](spec/workspaces.md#the-setup-subcommand)) | accepted |
| D66 | A superadmin who creates a workspace becomes its owner, as a user who creates one does, and hands it over through the ordinary ownership transfer, rather than naming an owner who joins at once or through an invitation; they see its content as a member, never as a superadmin ([architecture](architecture.md#workspace-shapes)) | accepted |
| D67 | Email stays optional, and what an address proves follows it. With email on, an account proves its address, by a code sent to it or a sign-in provider that vouches for it, before the API serves it anything but verifying, checked where every request is authenticated, and its session says whether it must still verify. With email off, nothing asks for a proof, and an invitation reaches whichever account holds its address. This rather than a proof always, which would make email required wherever invitations are, a setting of its own, a check on invitations alone, or a check left to the client, which any other caller skips ([auth spec](spec/auth.md#email-verification)) | accepted |
| D68 | The event catalog is documented in `openapi.json` at OpenAPI 3.1: one component per frame type, each in the shape OpenAPI 3.2 gives an item of a `text/event-stream`, with its payload's type as a component of its own, and the stream's media type naming their union in the extension `x-itemSchema`, rather than moving the document to OpenAPI 3.2 for `itemSchema`, which the generators most clients use don't read yet (REALTIME-008) | accepted |
| D69 | `seed` creates its data by sending the API's own routes, in process, the requests a client would, so its units of work open as any request's do, as `app`, rather than through services or rows of its own (the system spec's Seed, DATA-12) | accepted |
| D70 | An error that only a subcommand reports implements no `ErrorCode`, since no client receives it, rather than carrying a code that the OpenAPI document can't list (ERR-02, ERR-13) | accepted |
| D71 | The API reference that development serves lives at `/docs`, outside `/v1`, and outside the document, on the API's own origin, rather than in a container beside the API or under the versioned contract (API-01, API-14, PLATFORM-075) | accepted |
| D72 | `just setup` and `just dev` warn of a `.env` that `.env.example` has moved past: a setting it lacks, and a value the example once had and has since changed, which git's history of the example tells from a value the developer chose, rather than regenerating `.env`, whose new passwords the existing database doesn't have, or warning of every value that differs, which would warn of every deliberate choice at every run (OPS-19) | accepted |
| D73 | Optional parts stay removable: the template keeps every part, whose code is compiled, tested and deployed as the template's own, and a fork removes the parts it doesn't keep with `just remove`, which deletes what comments in each file's own syntax mark, rather than adding parts with a command, whose code and edits to the core modules would live outside the build and go stale, or gating them with Cargo features, which only Rust code can read and which a fork would keep. A marker in Rust is a `//` comment alone on its line once rustfmt has formatted the file, and `check-parts` does each removal in memory in `just checks` (ARCH-07) | accepted |
| D74 | The reference deployment's tasks on its server stay in POSIX sh, under CODE-07's 500 lines a file: `deploy.sh` dispatches, and sources a file of `deploy/tasks/` for each task, and `check-shell` checks each script with the files it sources in place, and refuses a function that assigns a variable its top level sets, rather than moving the tasks into Rust, which would need a binary on the server and a rewrite of what the end-to-end suite proves deploy by deploy (CODE-07, CODE-16, [deploy.md](operations/deploy.md)) | accepted |
| D75 | A deployment's Postgres server holds its database alone, with `bootstrap`'s roles named `owner` and `app`, rather than roles named after each deployment's URLs so that deployments, such as a staging and a production one, share a server: a deployment on a shared server would share its roles' names, its connections, locks, disk and administrator with the others, and the grants, PLATFORM-009's check and `check-schema` would each follow a name per deployment. A staging deployment runs the bundled Postgres on a server of its own instead ([deploy.md](operations/deploy.md#choosing-the-services)) | accepted |
| D76 | The justfile holds its settings, its variables and the tasks that run the others, and imports the rest from `just/`, a file for each group that `just --list` shows, rather than CODE-07 leaving the justfile out: a developer finds a task by its group, and each file of code stays one a reader can hold in mind, as the server's tasks are split by task (D74) (CODE-07) | accepted |
