# Every task, run the same way locally and in CI (OPS-10).

set dotenv-load
# A variable is evaluated only when a recipe uses it, so reading an image's name from Compose
# costs only the tasks that need it.
set lazy
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# Builds check queries against the committed cache in .sqlx/, as CI does (DATA-14).
export SQLX_OFFLINE := "true"

api := "cargo run --locked --package app --bin antworker-api --"
xtask := "cargo run --locked --quiet --package xtask --"

# The versions of the tools the tasks use besides the Rust toolchain, which rust-toolchain.toml
# pins (OPS-11). `just setup` checks them, and CI installs the ones `just checks` needs, which
# `just ci-tools` lists. CI installs `just` itself before any task can run, so its workflows pin
# the same version. sqlx-cli matches the sqlx in Cargo.lock.
just_version := "1.59.0"
cargo_deny_version := "0.20.2"
watchexec_version := "2.8.0"
sqlx_cli_version := "0.9.0"

# The registry `publish` pushes the image to. There the image takes the name of the GitHub
# repository that publishes it, in lowercase, so no two repositories' images share a name.
image_registry := "ghcr.io"
# The OCI image layout that `just image` writes, and the other image tasks read.
image_dir := "target/image"
# The app's version, which the image's label and a release tag carry.
version := replace_regex(read("crates/app/Cargo.toml"), '(?s)^.*?\nversion = "([^"]+)".*$', '$1')
# The images Docker's builder runs, pinned in compose.yaml with the other images the tasks run.
buildkit_image := `docker compose config --images buildkit`
sbom_generator_image := `docker compose config --images sbom-generator`
# The images of the tools that verify a release, which deploy/verify-release.sh runs.
crane_image := `docker compose config --images crane`
cosign_image := `docker compose config --images cosign`
# The builder the image builds in, one for each BuildKit image.
image_builder := "buildkit-" + sha256(buildkit_image)
# A directory that keeps BuildKit's cache between runs of `just image`, as CI's cache does, and
# whether the build writes its cache back there. CI writes it only when its cache missed.
image_cache := env("IMAGE_CACHE", "")
image_cache_from := if image_cache == "" { "" } else { "--cache-from type=local,src=" + image_cache }
image_cache_to := if image_cache == "" { "" } else if env("IMAGE_CACHE_WRITE", "true") == "true" { "--cache-to type=local,mode=max,dest=" + image_cache + "-new" } else { "" }
# The commit the image is built from, which its label names, and an identifier of the files the
# Postgres image is built from, which its label `build.inputs` carries (OPS-14).
image_revision := `git rev-parse HEAD`
postgres_inputs := `cargo run --locked --quiet --package xtask -- inputs deploy/postgres`
# This machine's platform, on which `just e2e` runs the images unless E2E_PLATFORM names another,
# such as linux/arm64, which needs the system to run its binaries, such as through QEMU.
native_platform := if arch() == "aarch64" { "linux/arm64" } else { "linux/amd64" }
export E2E_PLATFORM := env("E2E_PLATFORM", native_platform)
# The image's name in the registry the end-to-end tests run it from.
e2e_image := "image"
export E2E_IMAGE_NAME := e2e_image

# The other tasks, in a file for each group that `just` lists them in, which `just` reads as if
# they were here (CODE-08).
import 'just/develop.just'
import 'just/generate.just'
import 'just/check.just'
import 'just/fork.just'
import 'just/image.just'
import 'just/release.just'
import 'just/server.just'

# Lists the tasks, by group: `just` with no task named runs it.
[private]
default:
    @just --list

# Runs every check CI runs: the checks of the code, on the repository and on its copies without
# each run's optional parts, then the build of the image and its checks, on this machine's platform.
[doc("Runs every check CI runs, the image's included")]
[group('check')]
ci: checks check-removable image image-scan e2e

# Runs the checks of the code that need neither the image nor a copy of the repository.
[doc("Runs the checks of the code that need no image and no copy of the repository")]
[group('check')]
checks: fmt-check lint test deny check-test-map check-docs check-openapi check-instrument check-metrics check-telemetry-config alert-rules check-pins check-shell check-sql-bans check-layers check-deps check-layout check-schema check-tenancy check-table-ownership check-migrations sqlx-check check-parts
