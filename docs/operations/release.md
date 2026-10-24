# Releases

A release is a version of the API that you have decided is ready to deploy. This document says what a release is, when to make one, how `just release` makes it, and what to do when it fails. It then describes the image a release ships, and how anyone can verify it. [ci.md](ci.md) describes the pipeline.

## What a release is

Every commit that reaches `main` and passes CI is published: its image goes to the GitHub Container Registry, tagged with the commit's hash. A release takes one of those commits and gives it a version, such as 1.4.0, so that people can name it, deploy it, and go back to it.

- **A version** is three numbers, `MAJOR.MINOR.PATCH`, as semantic versioning defines them. Each release raises one of them, by what changed since the release before:
  - the patch, as from 1.4.0 to 1.4.1, for fixes;
  - the minor version, as from 1.4.1 to 1.5.0, for new features that keep everything else working;
  - the major version, as from 1.5.0 to 2.0.0, for a change that makes a fork, a client or a deployment change something to upgrade, such as a setting that is removed.
- **A prerelease**, such as 1.5.0-rc.1, is a version to try before its release. It comes before 1.5.0, and the next prerelease of the same release is 1.5.0-rc.2.
- **A tag** is a name that git gives one commit for good. The release of 1.4.0 tags its commit `v1.4.0`, so anyone can find exactly the code of that release. CI creates the tag; nobody runs git's tag commands.
- **The release's image** is the image CI tested for that commit, which the registry then also tags `1.4.0`, signed. Nothing is rebuilt (OPS-14).

## When to release

Release when `main` holds changes you want deployed. Release a fix as soon as it is on `main`. Features can wait, and go out together in one release.

## How to release

On your machine, with `main` checked out, run:

```
just release 1.4.0
```

It checks that `main` can release that version, sets the version in `crates/app/Cargo.toml`, the lock file and `openapi.json`, commits "Release 1.4.0", and pushes `main`. That commit is the request: CI releases a version only when a commit since the latest release asks for it, so a version set any other way, or the version that a repository made from the template starts with, releases nothing. CI then runs on that commit, as on every commit. When the run passes every check, its `release` job releases 1.4.0, about 20 minutes after the push. The repository's Actions page on GitHub shows the run, and the `release` job ends with "Released 1.4.0" and the image's digest.

`just release` refuses to start, and says what to do, when:

| It refuses when | What to do |
|---|---|
| There are changes that aren't committed | Commit them, or set them aside with `git stash` |
| Another branch than `main` is checked out | `git switch main` |
| GitHub's `main` has commits that yours doesn't | `git pull` |
| Your `main` has commits that GitHub's doesn't | `git push`, so that CI checks them first |
| The version is released already, or comes before the latest release | Run it with one of the versions it suggests: the next patch, minor or major version |
| Nothing has changed since the latest release | Nothing: the latest release is current |
| `main` holds that version already, waiting for a run that passes | Fix what failed, and push the fix: its run releases the version |

If `main` takes changes only through pull requests, GitHub refuses the push, and `just release` says how to release through a pull request instead.

## When a release fails

A run on `main` that fails before its `release` job releases nothing, and creates no tag. Fix what failed, and push the fix: the first run that passes with the version releases it. A run whose version is released already publishes its commit and releases nothing: to release that commit, run `just release` with a new version.

Each failure of the `release` job shows its next step on the run's page:

| Failure | What to do |
|---|---|
| The version is released already, or comes before the latest release | Run `just release` with one of the versions the error suggests |
| This run's `publish` job pushed no image | Re-run the run's failed jobs |
| The registry refused the version's tag, the signing failed, or the release doesn't verify | Re-run the job. If the signing fails again, Sigstore may be down: its status page is [status.sigstore.dev](https://status.sigstore.dev) |
| GitHub refused the tag `v<version>` | Re-run the job |

The tag comes last, so a version that has a tag is released completely, with its image tagged, signed and verified.

## The image

The image holds the API's binary, built static against musl for linux/amd64 and linux/arm64, on distroless's static base, which holds the operating system's certificate store, time zone data and the unprivileged user 65532, and no shell, package manager or C library (OPS-15).
- Its entry point is the binary, and its default command is `serve`. Every other subcommand runs from the same image, such as `docker run <image> migrate`, with the settings that `.env.example` lists in its environment.
- It writes nothing to its filesystem, so it runs with a read-only root filesystem.
- Its health check runs the `health` subcommand every 10 seconds, which succeeds only while `serve` answers 200 on `/health/ready` at `LISTEN_ADDR` (PLATFORM-044). For the first 60 seconds, a check runs every second and a failure doesn't count, so a new instance is healthy within a second of being ready. After that, three failures in a row make it unhealthy.
- Its labels name the app's version, as `org.opencontainers.image.version`, and the commit it was built from, as `org.opencontainers.image.revision`.
- Its index holds a software bill of materials (SBOM) for each platform, in the format of the Software Package Data Exchange (SPDX), which lists the base's packages and the crates the binary records. It also holds the provenance of each platform's image, in the format of Supply-chain Levels for Software Artifacts (SLSA), which records how BuildKit built it.

`just image` builds it into `target/image`, `just image-scan` scans it, and `just e2e` runs the end-to-end tests against it.

## Where it is published

`main` publishes every commit it tested to the GitHub Container Registry, as `ghcr.io/<owner>/<repository>`: the image takes the name of the repository that publishes it, in lowercase, so no two repositories' images share a name. Its tags:

| Tag | Image | Kept |
|---|---|---|
| `X.Y.Z` | A release | Always |
| `X.Y.Z-rc.N` | A prerelease | 90 days |
| `edge` | The newest commit on `main` that passed | Until a newer one passes |
| `sha-<commit>` | Each commit that `main` tested | 30 days |

A deployment pins a release by the digest its tag names.

## Signing

A release's signature is keyless. Sigstore's certificate authority issues a certificate for the identity of the run on `main` that releases it, which GitHub's identity token proves, and Sigstore's transparency log records the signature. Both are public: each release makes the repository's name, the workflow's path, the commit and the image's digest public, even for a private repository. They reveal nothing else: not the code, and not the image, which only those with access to the repository can pull.

## Verifying a release

`just verify-release` checks a release the way anyone who pulls it can:

```
cosign verify ghcr.io/<owner>/<repository>@<digest> \
  --certificate-identity https://github.com/<owner>/<repository>/.github/workflows/ci.yml@refs/heads/main \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
docker buildx imagetools inspect ghcr.io/<owner>/<repository>@<digest> --format '{{ json .SBOM }}'
docker buildx imagetools inspect ghcr.io/<owner>/<repository>@<digest> --format '{{ json .Provenance }}'
```

The signature covers the index, and so every platform's image, SBOM and provenance it lists.
