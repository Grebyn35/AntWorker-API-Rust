# CI

CI runs on GitHub Actions, in workflows that only orchestrate `just` tasks (OPS-12). Every action, image, runner and tool they use is pinned (OPS-13).

## The pipeline

`ci.yml` runs on every pull request and on every push to `main`. Its jobs:

| Job | Runs | Needs |
|---|---|---|
| `checks` | `just setup`, then `just checks`: every check of the code in the table below that doesn't need the image | |
| `image` | `just image`: the API's image and the reference deployment's Postgres, each built once for linux/amd64 and linux/arm64 with its software bill of materials (SBOM) and its provenance, into an image layout of the Open Container Initiative (OCI), which the later jobs download (OPS-14) | |
| `scan` | `just image-scan`, against the digests that `image` built, on a runner of its own, so the scanner's database takes none of `verify`'s disk | `image` |
| `verify` | `just e2e` against the digests that `image` built, as two jobs side by side, each naming its platform in `E2E_PLATFORM`: `verify (linux/amd64)`, and `verify (linux/arm64)` through QEMU. Each first deletes its runner's preinstalled toolchains that no task uses, for their disk | `image` |
| `publish` | On `main` only: `just publish`, which pushes those digests to the GitHub Container Registry as `sha-<commit>`, and moves `edge` to them while the commit is the head of `main` | `checks`, `scan`, `verify` |
| `release` | On `main` only: `just release-image`, which releases from those digests the version that `just release` asked for, if no release has it yet, as [release.md](release.md) describes | `publish` |

A newer push to a pull request cancels the older run. On `main`, a run that has started finishes, and the registry takes one write at a time, so `edge` never moves to an older commit. While a run on `main` goes on, a newer push replaces a run that is still waiting, so a commit pushed in between gets no run, and no image, of its own; the newer run builds a commit that holds it. BuildKit's cache of the build tools and the compiled dependencies is kept between runs, keyed by the files that change them, and the build writes it back only when the key missed.

`scheduled.yml` runs every day at 05:17 UTC, every Monday at 04:43 UTC, and on demand:
- Every day, `just deny`, so an advisory published against a locked dependency fails CI without waiting for a push (SEC-13).
- Every Monday, `just image-rescan`, which scans `edge`, the newest final release and the newest prerelease of each image again (SEC-13).
- Every Monday, the registry's upkeep, in both images' packages: it deletes the tags older than [release.md](release.md#where-it-is-published) keeps them, then the images left with no tag, and the signatures of images that are gone. A pinned action does it, and keeps each platform's image of an index it keeps. If GitHub refuses the deletions, the package's settings grant the repository the Admin role under "Manage Actions access".

`just setup` writes `.env` with generated passwords, and the tests start Postgres through Compose with them, as they do on a developer's machine (TEST-11). A job that only runs the tools Compose holds runs `just env` instead, which writes `.env` alone, because Compose reads it.

On a developer's machine, `just ci` runs what CI runs: `just checks`, `just image`, `just image-scan`, and `just e2e` on the machine's platform.

## Checks that exist

| Check | `just` task | Enforces |
|---|---|---|
| Formatting | `fmt-check` | CODE-02 |
| Clippy, with warnings as errors, on every target with every feature | `lint` | CODE-03 to CODE-06, CODE-08, CODE-14, CODE-15, TEST-07 (sleeps in tests), ERR-11 (results dropped with `.ok()`), API-14 and API-17 (routes registered outside `platform::http`), DATA-20 (the query functions and string-SQL executor methods outside the macros), DATA-21 (pools, connections, transactions and the listener outside `platform::db`), OBS-11 (OpenTelemetry's meters outside the declaration types) |
| Tests, against Postgres | `test` | TEST-11, and every behavior and rule a test proves |
| Dependencies: advisories, bans and sources for every target, and licenses for each target the API is built for | `deny` | CODE-11, CODE-12, SEC-13, SEC-14 |
| The map from spec behaviors to tests | `check-test-map` | TEST-01, DOC-10 |
| Links, IDs and named CI checks resolve; no history or unfinished-work markers; documents only in the documentation set; acronyms spelled out where first used; specs, rules, tables and the tracker keep their format; each spec's Routes table matches `openapi.json` | `check-docs` | DOC-01, DOC-07, DOC-08, DOC-09, DOC-13, DOC-14, DOC-15, DOC-16 |
| The committed `openapi.json`: its paths, its property names, each operation's access level and response descriptions, the codes of its error responses, and references that don't resolve | `check-openapi` | API-01, API-05, API-17, ERR-02, ERR-13 |
| Log and span field names come from the list in `platform::telemetry::names`; constant messages; `skip_all` on every `#[instrument]`; `app` code other than `main.rs` writes only `DEBUG` events and opens no spans; print lints excepted only where printing is allowed | `check-instrument` | CODE-03, OBS-01, OBS-03, OBS-04, OBS-05, OBS-06 |
| Metrics are named and labelled as OBS-11 says; the metric catalog matches the declarations; every metric a dashboard or alert reads is declared or listed, and every one is read; every panel title is a question; the dashboards are read-only, and one overview links to every other | `check-metrics` | OBS-11, OBS-12, OBS-13 |
| The collector's tail sampling keeps the traces OBS-08 names, and the managed backend's configuration and the proxy's keep that sampling; the latency objective is the same in the sampling, the alerts and the latency dashboard | `check-telemetry-config` | OBS-08 |
| Every alert has a severity, a runbook section and a test; promtool checks the rules and runs their tests | `alert-rules` | OBS-14 |
| Actions pinned by commit SHA, runners by version, images by digest in every Dockerfile and compose file, or by a variable the deploy sets to a digest, downloads by checksum, and the crates and packages the images' builds install by exact version; exact tool versions in the justfile, and the same `just` in the workflows; the toolchain only from `rust-toolchain.toml`; edition 2024 in every crate | `check-pins` | CODE-01, OPS-13 |
| shellcheck finds nothing in a recipe of the justfile that runs on Linux and macOS, or in a shell script under `deploy/`, and each directive that disables a finding stands on the line before its command, after the script's first command, with its reason; and no `echo` of a script under `deploy/` writes an expansion | `check-shell` | CODE-16 |
| No vulnerability that has a fix and is rated high or critical, in either platform's image of either image: in its operating system packages, and in the crates and modules its binaries record | `image-scan` | SEC-13, OPS-15 |
| The same scan, every week, of the images SEC-13 names | `image-rescan` | SEC-13 |
| The end-to-end tests, against the images built from the commit, the deploys and restores of the reference deployment among them | `e2e` | OPS-14 to OPS-17, OPS-20, OPS-21, SEC-18, and the rules and behaviors they prove (TEST-04) |
| Each of a release's tags names the tested digest, which the run on `main` signed, with the SBOM and the provenance of both platforms | `verify-release` | OPS-14 |

`check-test-map`, `check-docs`, `check-openapi`, `check-instrument`, `check-metrics`, `check-telemetry-config`, `check-pins` and `check-shell` are subcommands of the `xtask` crate (ARCH-01). `alert-rules` runs the `xtask` subcommand of the same name, then promtool, from the Prometheus image that `deploy/observability/compose.yaml` pins, through Docker Compose. `check-shell` runs shellcheck from the image that `compose.yaml` pins, through Docker Compose. `just` lists only the recipes of the platform it runs on, so `check-shell` reads the justfile itself, and checks the same recipes on every platform: it writes each one as a script whose lines stand on the justfile's line numbers, so a finding names the justfile's line. `check-test-map` prints how many of each module's behaviors a test proves. A behavior that no test proves fails it once `docs/MIGRATION.md` is deleted, and is reported until then (DOC-10).

`check-openapi` covers the parts of the rules that name it whose subjects exist: the paths, the property names, the access levels, the response descriptions, the error codes, and the references. It checks list shapes (API-05), methods (API-03), statuses (API-04), scopes in paths (API-02) and formats (API-11) once a route has them, and the breaking-change diff (API-18, ERR-02) from the first release on. Tests, not this check, compare the committed document with the one the code writes, and every documented path's methods with the ones it serves (API-17).

`check-docs` doesn't cover two of the rules that name it: the names of other codebases (DOC-04), and accepted architecture decision records (ADRs), whose check waits for the first ADR (DOC-11). Review enforces both until then.

`image-scan` and `image-rescan` run Grype, which reads the packages of each image's base, the crates that cargo-auditable records in the API's binary, and the modules that the Go binaries of the Postgres image record. `e2e` pushes both image layouts to the registry that Compose runs on 127.0.0.1:5001, and the tests run the images from there by the digest of their platform's image, against the Postgres that `just services` starts. The tests of the reference deployment deploy to a server of their own: Docker in a privileged container, from the image that `compose.yaml` pins as `server`, joined to Compose's network, where it pulls the API's and the deployment's Postgres from that registry, and reaches Compose's Postgres as the deployment's managed database. It pulls every other image, all of them Docker Hub's, through `mirror`, a cache of Docker Hub that Compose runs, and can't reach Docker Hub itself. Before the tests run, `just e2e` fetches every image they pull: the server's and the stack's on this machine, and, through `mirror`, the ones that `deploy/compose.yaml`, `deploy/observability/compose.yaml`, the import into a managed Grafana and the stand-in bucket name, for this machine's platform, so no test reaches a registry outside (TEST-07). `mirror` keeps what it fetched between runs. Each test runs `deploy/deploy.sh` there, as `just deploy` does on a real server, and sends its requests to Caddy through the ports the server publishes on 127.0.0.1. A server's store holds the images it pulls, and goes with the server when its test ends. A server that runs the self-hosted stack holds 3.4 gigabytes of them, so those tests run one at a time, which bounds the disk the suite takes. Even so, the suite, its build and its tools need more disk than a runner has free, so `verify` first deletes the toolchains it has no use for. They compile only with the `e2e` feature of the `app` crate, which `just e2e` and `just lint` turn on, so `just test` needs no image. `just e2e linux/arm64` runs the arm64 image on a machine that can run its binaries, as Docker Desktop can, and as CI's runner can once QEMU is installed. `publish`, `release-image` and `image-rescan` sign in to the registry with the runner's credentials, so they run only in CI. `verify-release` needs a checkout, `just env` and a sign-in to the registry, so anyone who can pull the images can run it, on Linux or macOS. It runs `deploy/verify-release.sh`, which a server can run without a checkout.

Builds check queries against the query cache committed in `.sqlx/` (DATA-20), so a query missing from the cache fails `lint` and `test`. `just prepare` rewrites the cache from the migrated database.

A `ci:` check that a rule names but that isn't listed in the table above doesn't exist yet. Until it is built, the rule is enforced by review. A check is built when the code it checks exists, and a check that never catches a real problem is removed.

## Dependency updates

Each week, Dependabot opens pull requests for the pins it can read, as `.github/dependabot.yml` configures: the crates, the actions, the images in both Dockerfiles and in every Compose file, at the root and under `deploy/`, and the toolchain in `rust-toolchain.toml`. Each runs the whole pipeline, as every pull request does. `compose.yaml` holds the images the tasks run, such as Grype's and BuildKit's, so that these updates reach them.

These pins are updated by hand, and `check-pins` checks their form:
- the versions of the tools in the justfile, and of `just` in the workflows;
- the runner's version in the workflows, such as `ubuntu-24.04`;
- in the Dockerfiles, the frontend on their first line; in the API's, the zig download with its checksums and the versions of the crates it installs; in `deploy/postgres/Dockerfile`, the versions of pgBackRest and of the certificate authorities' package.

## Checks required for merging

These are the jobs of `ci.yml` to require before a pull request merges: `checks`, `image`, `scan`, `verify (linux/amd64)` and `verify (linux/arm64)`. `publish` and `release` run only on `main`, so no pull request has them.

To require them on GitHub, open the repository's Settings, then either:
- Rules, Rulesets, New branch ruleset: target the default branch, turn on "Require status checks to pass", and add `checks`, `image`, `scan`, `verify (linux/amd64)` and `verify (linux/arm64)`.
- Branches, Add branch protection rule: for `main`, turn on "Require status checks to pass before merging", and add the same five.

GitHub offers both for a private repository only on a paid plan, and a fork doesn't inherit either, so requiring them is the repository owner's choice (OPS-12).
