# Releases

A release is a version of the API that you have decided is ready to deploy. This document says what a release is, when to make one, how `just release` makes it, and what to do when it fails. It then describes the images a release ships, and how anyone can verify them. [ci.md](ci.md) describes the pipeline. An ID in parentheses names what a sentence follows: a rule of the [standards](../standards/README.md), such as OPS-12, a behavior of a [spec](../spec/README.md), such as PLATFORM-063, or a decision, such as D44, whose records [architecture.md](../architecture.md) says where to find. The [glossary](../architecture.md#11-glossary) defines the terms these documents share.

## What a release is

Each run of CI on `main` that passes publishes its commit: the commit's images go to the GitHub Container Registry, tagged with the commit's hash. A release takes one of those commits and gives it a version, such as 1.4.0, so that people can name it, deploy it, and go back to it.

- **A version** is three numbers, `MAJOR.MINOR.PATCH`, as semantic versioning defines them. Each release raises one of them, by what changed since the release before:
  - the patch, as from 1.4.0 to 1.4.1, for fixes;
  - the minor version, as from 1.4.1 to 1.5.0, for new features that keep everything else working;
  - the major version, as from 1.5.0 to 2.0.0, for a change that makes a fork, a client or a deployment change something to upgrade, such as a setting that is removed.
- **A prerelease**, such as 1.5.0-rc.1, is a release to try before the final release of its number. It comes before 1.5.0, and the next prerelease of the same number is 1.5.0-rc.2. Only a final release fixes the API's contract, while a prerelease fixes its migrations as a final release does ([architecture.md](../architecture.md#11-glossary)).
- **A tag** is a name that git gives one commit for good. The release of 1.4.0 tags its commit `v1.4.0`, so anyone can find exactly the code of that release. CI creates the tag; nobody runs git's tag commands.
- **The release's images** are the images CI tested for that commit, which the registry then also tags `1.4.0`, signed: the API's, and the reference deployment's Postgres. Nothing is rebuilt (OPS-12).

## When to release

Release when `main` holds changes you want deployed. Release a fix as soon as it is on `main`. Features can wait, and go out together in one release.

## Before you release

Measure what one replica of the release serves, since a release that serves less needs more replicas (OPS-16). On your machine, with `main` checked out:

```
just image
just load-test
```

Before it measures, `just load-test` starts the services of `compose.yaml`, as `just services` does, the development Postgres among them. It fails unless `target/image` holds the image that the working tree's files build, and fails at once while a run of `just e2e`, or another load test, holds the machine (OPS-21).

`just load-test` runs one replica of the image on your machine's platform, on a database of its own on the development Postgres, which goes when the run ends, with the reference deployment's default limits for a replica, `API_CPUS` and `API_MEMORY`, and sends it requests at constant rates, raising the rate until a step fails. Each step reports the replica's memory after it, which stays flat from one step to the next; a step whose replica stopped fails, and says whether its memory limit killed it. It ends with a line that starts `Capacity:`: the highest rate the replica served with no request failed and the 99th percentile within the latency objective ([observability.md](observability.md#service-level-objectives)), and how many spans the collector accepted for its requests. A request for the capabilities writes two spans, its own and that of its rate limit's count (PLATFORM-104), so a run whose replica dropped none names twice as many spans as requests. The capacity depends on the machine, so compare a release with the one before it on the same machine. The line belongs in a final release's notes.

`just load-test me` measures `GET /v1/users/me` instead, as one signed-in user, so each request also verifies its session and counts in its user's bucket. `just load-test sign-ins` measures `POST /v1/sessions`, each request signing one of 20 users in with their password, whose hash is the API's CPU-heavy work, of which a replica runs one at once for each processor it may use (PLATFORM-060). Beside the sign-ins, each step sends 50 requests a second to `GET /v1/capabilities`, and passes only when none of those failed either and their 99th percentile is within the objective too, so its capacity is the rate of sign-ins that a replica serves while it serves its other requests (AUTH-093). Run it when a change touches how passwords are hashed, or what a sign-in does around the hash; a release's notes don't need its line.

Then measure its hub, which delivers events to the streams of server-sent events, with `just load-test streams`. It runs one replica with the same limits, opens 1,000 streams on it, 10 for each of 100 users, and reports how much memory each stream holds. Each user owns a workspace of their own, of which they are the only member. A second replica, without limits, takes the requests that publish events, as a deployment's other replicas do: each user renames their workspace, at constant rates, raising the rate until a step fails, which publishes `workspace.updated` to the user's 10 streams on the measured replica, an event that changes no one's access, so no stream closes. A step passes when no request failed, k6 dropped none for want of a client, every event reached each of its user's streams once and in order, no stream closed, and the 99th percentile of the measured hub's lag, from commit to delivery, which the task reads from Prometheus, is within the latency objective. It ends with a line that starts `Hub capacity:`, which belongs in the release's notes beside the other.

Then, for a final release, write its notes: its section of [CHANGELOG.md](../../CHANGELOG.md), which tells a fork what the release changes and what to port (DOC-11). A prerelease has none: what it holds stays under `## [Unreleased]` until the final release of its number.

1. Change the heading `## [Unreleased]` to the version and the day's date, as `## [1.4.0] - 2026-01-31`, and start a new `## [Unreleased]` above it.
2. Under the headings of [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), such as `### Added` and `### Fixed`, say what the release adds, changes, removes and fixes, and put each fix of a vulnerability under `### Security`, so a fork ports it first.
3. Under `### Upgrade notes`, say what a fork does to take the release: the migrations it adds, the settings it adds, renames or removes, and what a deployment runs by hand. If there is nothing to do, say that.
4. Under `### Capacity`, put the two lines that the load tests ended with.
5. Commit it.

`just release` refuses a final release whose section it doesn't find, or whose section lacks the upgrade notes or either line of the capacity. The doc comment of `lacks` in [`notes.rs`](../../crates/xtask/src/release/notes.rs) says what it reads of the changelog.

## How to release

On your machine, with `main` checked out, `just release` alone shows the latest release, how many commits `main` has since, and the versions that can come next, with what each is for, and changes nothing. While `main` asks for a version that no run has released yet, it shows that version, and the versions that can follow it. To release one, run:

```
just release 1.4.0
```

It checks that `main` can release that version, sets the version in `crates/app/Cargo.toml`, the lock file and `openapi.json`, commits "Release 1.4.0", and pushes `main`, with any commits of yours that GitHub's `main` doesn't have yet, so one run checks them all: there is no need to push first. That commit is the request: CI releases a version only when a commit since the latest release asks for it, so a version set any other way, or the version that a repository made from the template starts with, releases nothing. CI then runs on that commit, as on every commit. When the run passes every check, its `release` job releases 1.4.0, about 20 minutes after the push on a public repository's runners, whose processors are twice a private one's. The repository's Actions page on GitHub shows the run, and the `release` job ends with "Released 1.4.0" and the images' digests.

`just release` refuses to start, and says what to do, when:

| It refuses when | What to do |
|---|---|
| There are changes that aren't committed, untracked files among them | Commit them, or set them aside with `git stash --include-untracked` |
| Another branch than `main` is checked out | `git switch main` |
| GitHub's `main` has commits that yours doesn't | `git pull` |
| The version is released already, or comes before the latest release | Run it with one of the versions it suggests: the next patch, minor or major version |
| The version is a final release, and `CHANGELOG.md` lacks its section, the section's upgrade notes, or a line of its capacity | Write what it names ([Before you release](#before-you-release)), and commit it |
| Nothing has changed since the latest release | Nothing: the latest release is current |
| `main` holds that version already, waiting for a run that passes | Fix what failed, and push the fix: its run releases the version |

If `main` takes changes only through pull requests, GitHub refuses the push, and `just release` says how to release through a pull request instead. Any way of merging it works, a squash merge included, as long as its commit's subject stays "Release 1.4.0", or becomes "Release 1.4.0 (#12)" as GitHub writes it.

## When a release fails

A run on `main` that fails before its `release` job releases nothing, and creates no tag. Fix what failed, and push the fix: the first run that passes with the version releases it. A run whose version is released already publishes its commit and releases nothing: to release that commit, run `just release` with a new version.

A failure of the `release` job that this table names shows its message, which says what to do, on the run's page:

| The message starts | What to do |
|---|---|
| "crates/app/Cargo.toml sets the version `<version>`, which no release can have" | Set the version with `just release` |
| "crates/app/Cargo.toml sets `<version>`, so this commit isn't released. `<version>` comes before the latest release" | Run `just release` with one of the versions the message suggests |
| "CHANGELOG.md has no section for `<version>`", or "The section of `<version>` in CHANGELOG.md has no upgrade notes", or "The section of `<version>` in CHANGELOG.md lacks what one replica serves", as when the request reached `main` through a pull request that changed it | Write what the message names ([Before you release](#before-you-release)), and push it: its run releases the version |
| "This run's publish job pushed no image" | Re-run the run's failed jobs |
| "The registry refused the tag", "Signing failed", "The release of `<version>` doesn't verify", or "The release of `<version>` names other images than the tested ones" | Re-run the job. If the signing fails again, Sigstore may be down: its status page is [status.sigstore.dev](https://status.sigstore.dev) |
| "The images of `<version>` are released, but GitHub refused the tag" | Re-run the job |

The tag comes last, so a version that has a tag is released completely, with its images tagged, signed and verified.

## The images

The API's image holds the API's binary, built static against musl for linux/amd64 and linux/arm64, on distroless's static base, which holds the operating system's certificate store, time zone data and the unprivileged user 65532, and no shell, package manager or C library (OPS-13).
- Its entry point is the binary, and its default command is `serve`. Every other subcommand runs from the same image, such as `docker run <image> migrate`, with the settings that `.env.example` lists in its environment.
- Only `openapi` writes a file, the one it is given, so the image runs with a read-only root filesystem, and `openapi` writes to a mounted directory.
- Its health check runs the `health` subcommand, which succeeds only while `serve` answers 200 on `/health/ready` at `LISTEN_ADDR` (PLATFORM-063). While the instance starts, checks run often and their failures don't count, so it is healthy soon after it is ready; after that, a few failures in a row make it unhealthy. The `HEALTHCHECK` in the Dockerfile sets the intervals and the counts.
- Its labels name the app's version, as `org.opencontainers.image.version`, and the commit it was built from, as `org.opencontainers.image.revision`.
- Its image index holds a software bill of materials (SBOM) for each platform, in the format of the Software Package Data Exchange (SPDX), which lists the base's packages and the crates the binary records. It also holds the provenance of each platform's image, in the format of Supply-chain Levels for Software Artifacts (SLSA), which records how BuildKit built it.

The reference deployment's Postgres image is the official Postgres image, pinned by digest, with pgBackRest at an exact version, which archives the write-ahead log and takes the backups (D44, OPS-15). It carries the same labels as the API's image, and `build.inputs`, an identifier of the files it is built from, so a deploy restarts Postgres only when a release brings an image built from other files (OPS-14). Its index holds the same SBOM and provenance.

`just image` builds both, into `target/image` and `target/image-postgres`, `just image-scan` scans them, and `just e2e` runs the end-to-end tests against them.

## Where it is published

`main` publishes every commit it tested to the GitHub Container Registry: the API's image as `ghcr.io/<owner>/<repository>`, and the Postgres image as `ghcr.io/<owner>/<repository>-postgres`. The images take the name of the repository that publishes them, in lowercase, so no two repositories' images share a name. Both have these tags:

| Tag | Image | Kept |
|---|---|---|
| `X.Y.Z` | A final release | Always |
| `X.Y.Z-rc.N` | A prerelease | 90 days |
| `edge` | The commit at the head of `main` when its run published it | Until a later run moves it |
| `sha-<commit>` | Each commit that `main` tested | 30 days |

A deployment pins a release by the digests its tags name. The glossary in [architecture.md](../architecture.md#11-glossary) defines releases, digests and image indexes.

## Signing

A release's signatures are keyless: cosign signs each image by the Elliptic Curve Digital Signature Algorithm (ECDSA), with a key on the curve P-256 that it makes for that one signature, so the repository holds no signing key, and none is there to keep or to replace. Sigstore's certificate authority issues a certificate for that key, valid for 10 minutes, to the identity of the run on `main` that releases it, which GitHub's identity token proves, and Sigstore's transparency log records each signature. Both are public: each release makes the repository's name, the workflow's path, the commit and the images' digests public, even for a private repository. They reveal nothing else: not the code, and not the image, which only those with access to the repository can pull.

## Verifying a release

`just verify-release <owner>/<repository> <version> https://github.com/<owner>/<repository>` checks a release the way anyone who pulls it can, through [deploy/verify-release.sh](../../deploy/verify-release.sh), which the reference deployment also runs before it pulls a release. It runs on Linux or macOS, in a checkout, after `just env` and a sign-in to the registry, so anyone who can pull the images can run it. Its first argument is the repository, whose name it writes in lowercase for the images' ([Where it is published](#where-it-is-published)), and its third the repository's address, with the name as GitHub writes it, capitals included, which the identity of the run that signed the release holds. For each image, it does what these commands do, then prints `<image>@<digest>`, as the `release` job's last lines do:

```
crane digest <image>:<version>
cosign verify <image>@<digest> \
  --certificate-identity <address>/.github/workflows/ci.yml@refs/heads/main \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
docker buildx imagetools inspect <image>@<digest> --format '{{ json .Image }}'
docker buildx imagetools inspect <image>@<digest> --format '{{ json .SBOM }}'
docker buildx imagetools inspect <image>@<digest> --format '{{ json .Provenance }}'
```

`<image>` is `ghcr.io/<owner>/<repository>`, or that name with `-postgres`, in lowercase, `<address>` the third argument, and `<digest>` what the first command prints, the digest that the version's tag names. A signature covers the index, and so every platform's image, SBOM and provenance it lists. The commands check that a run of `ci.yml` on `main` signed the image, and not which commit that run built: the image's label `org.opencontainers.image.revision` names the commit ([The images](#the-images)). The third command reads each platform's image, whose label `org.opencontainers.image.version` names the version that it was built as: the script refuses an image that names another version than the tag, as the image of an earlier release would under a later version's tag.
