# Decision trace

Where each commitment in the recommendation text of D1 to D36 ([SYNTHESIS.md](SYNTHESIS.md)) is carried: by a rule in `docs/standards/`, a behavior or design section in `docs/spec/`, or `docs/architecture.md`. It checks a fixed list, so it converges: each commitment is carried, contradicted or not yet carried.

Of 291 commitments, 229 are carried, 23 are refined or contradicted by the rules, 5 aren't yet carried in the first slice, and 29 wait for later steps.

## Use

- **Writing an ADR:** write it from the rules as they stand. The table below lists where the recommendation text and the rules differ, and the rules are the decision.
- **Writing the spec of a module or a foundation part:** carry every commitment listed for it under "Not yet carried".

## Recommendation text that the rules refine

The rules narrowed or completed these on purpose. The ADR states the rule's version.

| Decision | Recommendation text | The rule that refines it |
|---|---|---|
| D6 | Audit every successful state change | DATA-27: platform bookkeeping and scheduled maintenance, such as the invitation expiry sweep, aren't audited |
| D8 | Constraint violations map to 4xx | ERR-05: each constraint a repository relies on maps to a domain outcome; an unexpected violation is `Internal` |
| D8, D10, D11 | Every response is a problem with a trace ID; the SDK samples every trace; an IP limit applies to every route | API-07, API-16, OBS-08, OBS-15, SEC-12 and OPS-05 exempt the health probes |
| D10 | Every log line carries the trace and span IDs | OBS-02: a line written inside a span carries them |
| D31 | Export excludes other members' data and whole-workspace content | The `accounts` spec's "Export and erasure": the content of every workspace whose only member is the user is exported, the same workspaces that erasure deletes (D40) |
| D29 | OAuth providers and the client telemetry module are removable; the core modules are not | ARCH-07 also makes removable each part that gives a product its workspace shape, the `invitations` module among them (D39) |
| D22 | A Cargo workspace with two crates | ARCH-01: the API has two crates, and a third, `xtask`, holds the custom CI checks |
| D22 | The service holds the transaction boundary | The platform spec's unit of work: the route opens it and the service commits it |
| D22 | Traits exist only where there are two real implementations | ARCH-13 also allows traits that invert a dependency |
| D25 | Each type's constructor trims | AUTH-003: a password isn't trimmed |
| D15 | `vX.Y.Z` tags publish releases, and `vX.Y.Z-rc.N` tags prerelease images | OPS-14: `just release` sets the app's version, and the run on `main` that passes with it releases it and creates its tag `vX.Y.Z` or `vX.Y.Z-rc.N` |
| D21 | Workflows are split by purpose: pull requests and `main`, release tags, and scheduled jobs | OPS-12: `ci.yml` for pull requests and `main`, whose runs on `main` also publish and release, and `scheduled.yml` for scheduled jobs |
| D21 | A release tag attaches build provenance | OPS-14: the build attaches the SBOM and the provenance to every image, and a release signs the index that holds them |
| D21 | The required checks are applied as branch protection by a script | OPS-12: `docs/operations/ci.md` lists the required checks and says how to require them; requiring them is the repository owner's choice, since a fork doesn't inherit branch protection and GitHub offers it for a private repository only on a paid plan |
| D20 | Runtime threads come from configuration | OPS-02: the async runtime sizes itself to the CPUs the container may use, so no two deployments would set it differently, and it isn't a setting |
| D27 | Nothing outside `docs/inventory/` refers to the old API | DOC-04 extends the exception to `docs/MIGRATION.md`, to CLAUDE.md's pointer to the inventory, and to decision IDs and the links that architecture.md and the tracker make to the decisions' records |
| D32 | A keyed hash only for verification codes and rate-limit keys | SEC-16 adds emails in audit entries, which D32's own principle covers: they are guessable |
| D36 | A security failure recorded during sign-in answers with that failure | ARCH-11 covers every recorded security failure, such as refresh-token reuse (AUTH-012) |

## Contradictions in the first slice

| Item | Settled in |
|---|---|
| AUTH-004 answers 200 for a sign-in that creates a session, while API-04 requires 201 with `Location` for a create | the `auth` step |

## Not yet carried

### In the first slice

| Decision | Commitment | Settled in |
|---|---|---|
| D3 | The server stores no active or default workspace | the `workspaces` step: state it in the spec's Design |
| D11 | Each user holds a capped number of concurrent streams | the `realtime` step |
| D12 | Idempotency keys are compared exactly as sent | the idempotency step |
| D18 | Sign-in and refresh responses come from one function that issues the session | the `auth` step |
| D20 | Startup logs the connection budget | the request pipeline (build order 1.2), when `serve` keeps its pool |

### For later steps

Modules carry their own; the build order step of each other area is in parentheses, as in [MIGRATION.md](../MIGRATION.md#foundation).

| Module or area | Commitments |
|---|---|
| `access` | the permission that reveals IP addresses in audit entries; the ADR on custom roles (D2) |
| `audit` | reads filtered by permission (D2); the target filterable and visible to that user; bounded retention (D6) |
| `auth` | the ADR on accepted enumeration (D7); forgot-password answers 503 with email off (D9); the OAuth redirect allowlist (D18); password reset that sets the first password of an account created without one, and verifies its address (the `workspaces` spec's setup subcommand, D39) |
| `accounts` | deletion refused while the user owns a shared workspace, sole-member workspaces deleted with the account, and the ADR on immediate deletion (D16); the export endpoint, one per 24 hours, its archive, notice and download, and the optional erasure precondition (D31) |
| `files` | a bucket for anonymous public-file downloads (D11) |
| Deploy and CI (1.5 and 2.4) | Caddy's trusted proxies (D11); TLS everywhere and storage encrypted at rest (D14, queued); alerts that don't depend on the deploy tooling (D15); Compose memory and CPU limits (D20); shellcheck, with the deploy tooling's scripts (D21); the OpenAPI UI in development only, in step 2.4 (D28) |
| Template (5, 6) | a GitHub template repository and `just new-module` (D29); the ADR on hard deletes (D24) |
| Backups (1.5) | the backup tool, point-in-time recovery on managed Postgres, bucket versioning, and a separate backup of the secrets key (D30) |
| Security (6) | the ASVS Level 2 conformance pass (D26) |
