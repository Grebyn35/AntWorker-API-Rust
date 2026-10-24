# Releases

The API ships as one image. CI builds it once for each commit and publishes it from `main`, and a release tag releases that image without rebuilding it (OPS-14). This document describes the image, how to release it, and how to verify a release. [ci.md](ci.md) describes the pipeline.

## The image

The image holds the API's binary, built static against musl for linux/amd64 and linux/arm64, on distroless's static base, which holds the operating system's certificate store, time zone data and the unprivileged user 65532, and no shell, package manager or C library (OPS-15).
- Its entry point is the binary, and its default command is `serve`. Every other subcommand runs from the same image, such as `docker run <image> migrate`, with the settings that `.env.example` lists in its environment.
- It writes nothing to its filesystem, so it runs with a read-only root filesystem.
- Its health check runs the `health` subcommand every 10 seconds, which succeeds only while `serve` answers 200 on `/health/ready` at `LISTEN_ADDR` (PLATFORM-044). For the first 60 seconds, a check runs every second and a failure doesn't count, so a new instance is healthy within a second of being ready. After that, three failures in a row make it unhealthy.
- Its labels name the app's version, as `org.opencontainers.image.version`, and the commit it was built from, as `org.opencontainers.image.revision`.
- Its index holds a software bill of materials (SBOM) for each platform, in the format of the Software Package Data Exchange (SPDX), which lists the base's packages and the crates the binary records. It also holds the provenance of each platform's image, in the format of Supply-chain Levels for Software Artifacts (SLSA), which records how BuildKit built it.

`just image` builds it into `target/image`, `just image-scan` scans it, and `just e2e` runs the end-to-end tests against it.

## Where it is published

`main` publishes every commit it tested to the GitHub Container Registry, as `ghcr.io/<owner>/antworker-api`:

| Tag | Image |
|---|---|
| `sha-<commit>` | Each commit that `main` tested in the last 30 days |
| `edge` | The newest commit on `main` that passed |
| `X.Y.Z` | A release |
| `X.Y.Z-rc.N` | A prerelease |

A deployment pins a release by the digest its tag names.

## Releasing

1. Set the version in `crates/app/Cargo.toml` to `X.Y.Z`, or `X.Y.Z-rc.N` for a prerelease, and merge the change into `main`. The version is the release's, and `service.version` in the API's telemetry.
2. Wait until the run on `main` of that commit has published it as `sha-<commit>`.
3. Tag the commit `vX.Y.Z` or `vX.Y.Z-rc.N`, and push the tag.

The release workflow then runs `just release`. It fails unless the tag has that form and names the version in `crates/app/Cargo.toml`, and unless `main` published the commit. It gives that image the tag `X.Y.Z` or `X.Y.Z-rc.N`, signs it, and runs `just verify-release`. Running the workflow again for the same tag is safe: tagging again changes nothing, and signing again adds a signature.

The signature is keyless. Sigstore's certificate authority issues a certificate for the identity of the release workflow's run, which GitHub's identity token proves, and Sigstore's transparency log records the signature. Both are public, so a release makes the repository's name and the path of its release workflow public, even for a private repository.

## Verifying a release

`just verify-release` checks a release the way anyone who pulls it can:

```
cosign verify ghcr.io/<owner>/antworker-api@<digest> \
  --certificate-identity https://github.com/<owner>/<repository>/.github/workflows/release.yml@refs/tags/vX.Y.Z \
  --certificate-oidc-issuer https://token.actions.githubusercontent.com
docker buildx imagetools inspect ghcr.io/<owner>/antworker-api@<digest> --format '{{ json .SBOM }}'
docker buildx imagetools inspect ghcr.io/<owner>/antworker-api@<digest> --format '{{ json .Provenance }}'
```

The signature covers the index, and so every platform's image, SBOM and provenance it lists.
