# Decision trace

Where each commitment in the recommendation text of D1 to D36 ([SYNTHESIS.md](SYNTHESIS.md)) is carried: by a rule in `docs/standards/`, a behavior or design section in `docs/spec/`, or `docs/architecture.md`. It checks a fixed list, so it converges: each commitment is carried, contradicted or not yet carried.

It lists the commitments that the rules refine or contradict, and those not yet carried, each with the step that carries it. Every other commitment is carried as its recommendation text states it.

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
| D15 | A single-host rolling deploy | OPS-16 and architecture.md §9: a deploy starts every new replica, as a second color, before it stops the old ones (D45) |
| D20 | Startup logs the computed connection budget | OPS-21: each instance logs the connections it opens, since it can't know how many replicas run, and a deploy refuses replicas whose budget doesn't fit |
| D11 | Caddy sets security headers | SEC-11: the API sets its security headers on every response, and the proxy sets HSTS |
| D21 | Workflows are split by purpose: pull requests and `main`, release tags, and scheduled jobs | OPS-12: `ci.yml` for pull requests and `main`, whose runs on `main` also publish and release, and `scheduled.yml` for scheduled jobs |
| D21 | A release tag attaches build provenance | OPS-14: the build attaches the SBOM and the provenance to every image, and a release signs the index that holds them |
| D21 | The required checks are applied as branch protection by a script | OPS-12: `docs/operations/ci.md` lists the required checks, says how to require them, and leaves requiring them to the repository owner |
| D20 | Runtime threads come from configuration | OPS-02: the async runtime sizes itself to the CPUs the container may use, so no two deployments would set it differently, and it isn't a setting |
| D27 | Nothing outside `docs/inventory/` refers to the old API | DOC-04 extends the exception to `docs/MIGRATION.md`, to CLAUDE.md's pointer to the inventory, and to decision IDs and the links that architecture.md and the tracker make to the decisions' records |
| D32 | A keyed hash only for verification codes and rate-limit keys | SEC-16 adds emails in audit entries, which D32's own principle covers: they are guessable |
| D36 | A security failure recorded during sign-in answers with that failure | ARCH-11 covers every recorded security failure, such as refresh-token reuse (AUTH-012) |
| D18 | The cookie transport, CSRF protection, a CORS allowlist for web origins, and signed URLs for private downloads are built later, as additions | D61: each is built with the part it belongs to: the cookie, CSRF protection and the allowlist with `auth`, a stream through the cookie with `realtime`, and signed URLs with `files` |
| D26 | OWASP ASVS Level 2 is the security bar | The security rules' introduction: Level 2 of version 5.0.0, but for requirement 6.3.3, since every user may add a second factor and every superadmin must (D63) |

## Contradictions in the first slice

None.

## Not yet carried

### In the first slice

| Decision | Commitment | Settled in |
|---|---|---|
| D11 | Each user holds a capped number of concurrent streams | the `realtime` step |
| D18 | A browser's stream authenticates through the same cookie (D61) | the `realtime` step |

### For later steps

Modules carry their own; the build order step of each other area is in parentheses, as in [MIGRATION.md](../MIGRATION.md#foundation).

| Module or area | Commitments |
|---|---|
| `access` | the permission that reveals IP addresses in audit entries; the ADR on custom roles (D2) |
| `audit` | reads filtered by permission (D2); the target filterable and visible to that user; bounded retention (D6) |
| `auth` | the ADR on accepted enumeration (D7); forgot-password answers 503 with email off (D9); the OAuth redirect allowlist (D18); password reset that sets the first password of an account created without one, and verifies its address (the `workspaces` spec's setup subcommand, D39); with the key that encrypts provider tokens (SEC-02), a backup of that key kept apart from the database's (D30) |
| `accounts` | deletion refused while the user owns a shared workspace, sole-member workspaces deleted with the account, and the ADR on immediate deletion (D16); the export endpoint, one per 24 hours, its archive, notice and download, and the optional erasure precondition (D31) |
| `files` | a bucket for anonymous public-file downloads (D11); versioning on the files' bucket, recommended and documented (D30); signed URLs for private downloads, which a browser's `<img>` needs (D18, D61) |
| Development (2.4) | the OpenAPI UI in development only (D28) |
| Template (5, 6) | a GitHub template repository and `just new-module` (D29); the ADR on hard deletes (D24) |
| Security (6) | the ASVS Level 2 conformance pass (D26) |
