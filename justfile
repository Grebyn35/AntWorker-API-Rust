# Every task, run the same way locally and in CI (OPS-12).

set dotenv-load
# A variable is evaluated only when a recipe uses it, so reading an image's name from Compose
# costs only the tasks that need it.
set lazy
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# Builds check queries against the committed cache in .sqlx/, as CI does (DATA-20).
export SQLX_OFFLINE := "true"

api := "cargo run --locked --package app --bin antworker-api --"
xtask := "cargo run --locked --quiet --package xtask --"

# The versions of the tools the tasks use besides the Rust toolchain, which rust-toolchain.toml
# pins (OPS-13). `just setup` checks them, and CI installs the ones `just checks` needs, which
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
# This machine's platform, on which `just e2e` runs the image unless told another.
native_platform := if arch() == "aarch64" { "linux/arm64" } else { "linux/amd64" }
# The image's name in the registry the end-to-end tests run it from.
e2e_image := "image"
export E2E_IMAGE_NAME := e2e_image

# Runs every check CI runs: the checks of the code, then the build of the image and its checks,
# on this machine's platform.
ci: checks image image-scan e2e

# Runs the checks of the code, the part of CI that needs no image.
checks: fmt-check lint test deny check-test-map check-docs check-openapi check-instrument check-metrics check-telemetry-config alert-rules check-pins check-shell

# Lists the tools `just checks` needs besides just and the toolchain, as CI's install step takes
# them.
ci-tools:
    @echo cargo-deny@{{ cargo_deny_version }}

# Prepares this machine (OPS-19): checks the tools the tasks need and their pinned versions,
# installs the toolchain that rust-toolchain.toml pins, and writes .env. watchexec and sqlx-cli
# only get a warning, because only `dev` and `prepare` need them.
[unix]
setup: && env
    for tool in rustup git docker; do command -v "$tool" > /dev/null || { echo "just setup needs $tool" >&2; exit 1; }; done
    docker compose version > /dev/null || { echo "just setup needs Docker Compose" >&2; exit 1; }
    [ "$(just --version | head -n 1 | cut -d ' ' -f 2)" = "{{ just_version }}" ] || { echo "just setup needs just {{ just_version }}" >&2; exit 1; }
    [ "$(cargo deny --version 2> /dev/null | head -n 1 | cut -d ' ' -f 2)" = "{{ cargo_deny_version }}" ] || { echo "just setup needs cargo-deny {{ cargo_deny_version }}" >&2; exit 1; }
    [ "$(watchexec --version 2> /dev/null | head -n 1 | cut -d ' ' -f 2)" = "{{ watchexec_version }}" ] || echo "warning: just dev needs watchexec {{ watchexec_version }}" >&2
    [ "$(cargo sqlx --version 2> /dev/null | head -n 1 | cut -d ' ' -f 2)" = "{{ sqlx_cli_version }}" ] || echo "warning: just prepare needs sqlx-cli {{ sqlx_cli_version }}" >&2
    rustup show active-toolchain > /dev/null || rustup toolchain install

# Writes .env from .env.example, with generated passwords, if .env doesn't exist yet. Compose
# reads it too, so a CI job that runs only Compose's tools runs this instead of `just setup`.
[unix]
env:
    test -f .env || sed \
        -e "s/__ADMIN_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        -e "s/__OWNER_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        -e "s/__APP_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        .env.example > .env

# Prepares this machine (OPS-19): checks the tools the tasks need and their pinned versions,
# installs the toolchain that rust-toolchain.toml pins, and writes .env. watchexec and sqlx-cli
# only get a warning, because only `dev` and `prepare` need them.
[windows]
setup: && env
    foreach ($tool in 'rustup', 'git', 'docker') { if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { throw "just setup needs $tool" } }
    docker compose version | Out-Null; if ($LASTEXITCODE -ne 0) { throw 'just setup needs Docker Compose' }
    function Version($command) { try { (((& $command[0] $command[1..9] 2> $null) | Select-Object -First 1) -split ' ')[1] } catch { '' } }; if ((Version 'just', '--version') -ne '{{ just_version }}') { throw 'just setup needs just {{ just_version }}' }; if ((Version 'cargo', 'deny', '--version') -ne '{{ cargo_deny_version }}') { throw 'just setup needs cargo-deny {{ cargo_deny_version }}' }; if ((Version 'watchexec', '--version') -ne '{{ watchexec_version }}') { Write-Warning 'just dev needs watchexec {{ watchexec_version }}' }; if ((Version 'cargo', 'sqlx', '--version') -ne '{{ sqlx_cli_version }}') { Write-Warning 'just prepare needs sqlx-cli {{ sqlx_cli_version }}' }
    rustup show active-toolchain | Out-Null; if ($LASTEXITCODE -ne 0) { rustup toolchain install }

# Writes .env from .env.example, with generated passwords, if .env doesn't exist yet. Compose
# reads it too, so a CI job that runs only Compose's tools runs this instead of `just setup`.
[windows]
env:
    if (-not (Test-Path .env)) { $text = Get-Content .env.example -Raw; foreach ($name in 'ADMIN', 'OWNER', 'APP') { $bytes = New-Object byte[] 16; [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes); $text = $text.Replace("__$($name)_PASSWORD__", -join ($bytes | ForEach-Object { $_.ToString('x2') })) }; [IO.File]::WriteAllText((Join-Path $PWD '.env'), $text) }

# Starts the services in compose.yaml, and waits until they are healthy.
services:
    docker compose up --detach --wait

# Creates the database roles and their grants (DATA-12).
bootstrap: services
    {{ api }} bootstrap

# Applies the migrations the database hasn't recorded. Run it after adding a migration, before
# writing queries against it.
migrate: bootstrap
    {{ api }} migrate

# Runs the API with the settings in .env.
run: migrate
    {{ api }} serve

# Runs the API on this machine with auto-reload (OPS-19): starts Postgres and bootstraps the
# database, then applies new migrations and restarts the API whenever its code or a migration
# changes. `just dev observe` also starts the observability stack and exports the API's
# telemetry to it; docs/operations/observability.md lists where each service answers.
[unix]
dev $mode="": bootstrap
    case "$mode" in "") ;; observe) docker compose --profile observability up --detach --wait ;; *) echo "just dev takes nothing, or observe" >&2; exit 1 ;; esac
    {{ if mode == "observe" { "OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 " } else { "" } }}watchexec --restart --shell=none --watch crates --watch migrations --exts rs,sql,toml -- just _dev-serve

# Runs the API on this machine with auto-reload (OPS-19): starts Postgres and bootstraps the
# database, then applies new migrations and restarts the API whenever its code or a migration
# changes. `just dev observe` also starts the observability stack and exports the API's
# telemetry to it; docs/operations/observability.md lists where each service answers.
# watchexec runs `just` without a shell: through its default shell on Windows, `just` receives
# no recipe name and runs `ci`.
[windows]
dev mode="": bootstrap
    if ('{{ mode }}' -eq 'observe') { docker compose --profile observability up --detach --wait; if ($LASTEXITCODE -ne 0) { throw 'the observability stack did not start' }; $env:OTEL_EXPORTER_OTLP_ENDPOINT = 'http://127.0.0.1:4318' } elseif ('{{ mode }}' -ne '') { throw 'just dev takes nothing, or observe' }; watchexec --restart --shell=none --watch crates --watch migrations --exts rs,sql,toml -- just _dev-serve

# Applies new migrations, then serves. `just dev` runs it again whenever the code changes.
_dev-serve:
    {{ api }} migrate
    {{ api }} serve

# Formats the code.
fmt:
    cargo fmt --all

# Fails if the code isn't formatted.
fmt-check:
    cargo fmt --all -- --check

# Runs clippy with warnings as errors (CODE-03), on every target with every feature, so the
# end-to-end tests are linted too.
lint:
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Runs every test. The integration tests use the services.
test: services
    cargo test --workspace --locked

# Rewrites the query cache in .sqlx/ from the migrated database. Run it after changing a query
# (DATA-20). It needs the sqlx-cli of `sqlx_cli_version`: `cargo install sqlx-cli --version
# <version> --no-default-features --features rustls,postgres --locked`.
prepare: services
    cargo sqlx prepare --workspace -- --all-targets --locked

# Checks dependencies: advisories, bans and sources for every target (SEC-13, SEC-14, CODE-11),
# and licenses for each target the API is built for, the platforms a developer builds on (OPS-19)
# and the image's (OPS-14), one at a time, as cargo-deny takes one target per run. A crate that
# only other targets compile, such as one for WebAssembly, never reaches a build, so its license
# doesn't matter (CODE-12).
deny:
    cargo deny --locked check advisories bans sources
    cargo deny --locked --target x86_64-unknown-linux-gnu check licenses
    cargo deny --locked --target aarch64-unknown-linux-gnu check licenses
    cargo deny --locked --target x86_64-unknown-linux-musl check licenses
    cargo deny --locked --target aarch64-unknown-linux-musl check licenses
    cargo deny --locked --target x86_64-pc-windows-msvc check licenses
    cargo deny --locked --target aarch64-pc-windows-msvc check licenses
    cargo deny --locked --target x86_64-apple-darwin check licenses
    cargo deny --locked --target aarch64-apple-darwin check licenses

# Reports which spec behaviors tests prove, and fails on a test whose name doesn't start with a
# behavior or rule ID that exists (TEST-01, DOC-10).
check-test-map:
    {{ xtask }} check-test-map

# Writes openapi.json, the OpenAPI document of every route (API-17). Run it after changing a
# route; a test fails while the committed document differs from the one the code writes.
openapi:
    {{ api }} openapi openapi.json

# Fails on a part of openapi.json that breaks the rules for the HTTP contract (API-01, API-05,
# API-17, ERR-02, ERR-13).
check-openapi:
    {{ xtask }} check-openapi

# Fails on a log or span field name outside the list in platform::telemetry::names, a message
# that isn't constant, an #[instrument] without skip_all, an event at any level but DEBUG, or a
# span, in app code other than main.rs, and an exception to a print lint outside the places
# printing is allowed (CODE-03, OBS-01, OBS-03 to OBS-06).
check-instrument:
    {{ xtask }} check-instrument

# Writes the metric catalog, docs/operations/metrics.md, from the metric declarations (OBS-12).
# Run it after declaring or changing a metric; check-metrics fails while the committed catalog
# differs.
metrics:
    {{ xtask }} metric-catalog

# Fails on a metric declared or named outside OBS-11, a catalog that differs from the
# declarations, a dashboard or alert that reads a metric neither declared nor listed in
# deploy/observability/metrics.yaml, a metric nothing reads, a panel title that isn't a question,
# a dashboard that Grafana's users can change, or dashboards no overview links to all of (OBS-11
# to OBS-13).
check-metrics:
    {{ xtask }} check-metrics

# Fails on a collector configuration whose tail sampling doesn't keep the traces OBS-08 names, a
# managed backend's configuration that replaces that sampling, or a latency objective that differs
# between the sampling, the alerts and the latency dashboard.
check-telemetry-config:
    {{ xtask }} check-telemetry-config

# Fails on an alert without a severity, a runbook section or a test, then runs promtool's checks
# and tests of the rules, with the Prometheus image that deploy/observability/compose.yaml pins
# (OBS-14).
alert-rules:
    {{ xtask }} alert-rules
    docker compose --profile observability run --rm --no-deps --volume ./deploy/observability/prometheus:/rules:ro --workdir /rules --entrypoint promtool prometheus check rules rules.yaml
    docker compose --profile observability run --rm --no-deps --volume ./deploy/observability/prometheus:/rules:ro --workdir /rules --entrypoint promtool prometheus test rules rules.test.yaml

# Fails on a link, ID or named CI check that doesn't resolve, a history marker, an
# unfinished-work marker, a document outside the documentation set, an acronym not spelled out
# where a document first uses it, a spec, rule or tracker whose format breaks DOC-15 or DOC-16,
# or a spec's Routes table that doesn't match openapi.json (DOC-01, DOC-07 to DOC-09, DOC-13 to
# DOC-16).
check-docs:
    {{ xtask }} check-docs

# Fails on an action, runner, image, download, crate install or tool version that isn't pinned, a
# toolchain other than the one rust-toolchain.toml pins, or a crate whose edition isn't the
# workspace's 2024 (CODE-01, OPS-13).
check-pins:
    {{ xtask }} check-pins

# Fails on a finding of shellcheck, run from the image compose.yaml pins, in a recipe of this
# justfile that runs on Linux and macOS, or in a shell script under deploy/ (CODE-16). It reads
# the recipes from this file, so it checks the same ones on every platform.
check-shell:
    {{ xtask }} check-shell

# Prints the names of the packages that the images of `repository`, such as owner/name, are in
# the registry, separated by commas, for the workflows.
image-packages repository:
    @echo {{ lowercase(file_name(repository)) }},{{ lowercase(file_name(repository)) }}-postgres

# Builds each image once for linux/amd64 and linux/arm64, with its software bill of materials
# (SBOM) and provenance, into an OCI image layout (OPS-14, OPS-15): the API's into target/image,
# and the reference deployment's Postgres into target/image-postgres (D44). The Postgres image
# carries an identifier of the files it is built from as `build.inputs`, so a deploy restarts
# Postgres only for an image built from other files (OPS-16). With IMAGE_CACHE naming a directory,
# BuildKit reads its cache for the API's image from there, and replaces it with the cache of this
# build unless IMAGE_CACHE_WRITE is false. While the two platforms write the layer they share,
# buildx can print "ref ... locked" errors: they are retries, and the exit status says whether the
# build succeeded.
image revision=`git rev-parse HEAD` postgres_inputs=`cargo run --locked --quiet --package xtask -- inputs deploy/postgres`: _image-builder && _image-cache
    docker buildx build --builder {{ image_builder }} --platform linux/amd64,linux/arm64 \
        --attest type=sbom,generator={{ sbom_generator_image }} --attest type=provenance,mode=max \
        --label org.opencontainers.image.version={{ version }} --label org.opencontainers.image.revision={{ revision }} \
        {{ image_cache_from }} {{ image_cache_to }} \
        --output type=oci,dest={{ image_dir }},tar=false .
    docker buildx build --builder {{ image_builder }} --platform linux/amd64,linux/arm64 \
        --attest type=sbom,generator={{ sbom_generator_image }} --attest type=provenance,mode=max \
        --label org.opencontainers.image.version={{ version }} --label org.opencontainers.image.revision={{ revision }} \
        --label build.inputs={{ postgres_inputs }} \
        --output type=oci,dest={{ image_dir }}-postgres,tar=false deploy/postgres

# Creates the builder, if this machine doesn't have it yet.
[unix]
_image-builder:
    docker buildx inspect {{ image_builder }} > /dev/null 2>&1 || docker buildx create --name {{ image_builder }} --driver docker-container --driver-opt image={{ buildkit_image }} > /dev/null

# Creates the builder, if this machine doesn't have it yet.
[windows]
_image-builder:
    docker buildx inspect {{ image_builder }} *> $null; if ($LASTEXITCODE -ne 0) { docker buildx create --name {{ image_builder }} --driver docker-container --driver-opt image={{ buildkit_image }} | Out-Null }

# Replaces the cache in IMAGE_CACHE with the one the build wrote, if it wrote one, so the cache
# holds only what the last build used.
[unix]
_image-cache:
    if [ -n "${IMAGE_CACHE:-}" ] && [ -d "$IMAGE_CACHE-new" ]; then rm -rf "$IMAGE_CACHE" && mv "$IMAGE_CACHE-new" "$IMAGE_CACHE"; fi

# Replaces the cache in IMAGE_CACHE with the one the build wrote, if it wrote one, so the cache
# holds only what the last build used.
[windows]
_image-cache:
    if ('{{ image_cache }}' -and (Test-Path '{{ image_cache }}-new')) { Remove-Item -Recurse -Force '{{ image_cache }}' -ErrorAction SilentlyContinue; Move-Item '{{ image_cache }}-new' '{{ image_cache }}' }

# Fails on a vulnerability that has a fix and is rated high or critical, in each platform's image
# of each image in target (SEC-13). The scanner reads the operating system's packages and the
# crates the binary records.
image-scan:
    docker compose run --rm grype oci-dir:/image --platform linux/amd64 --only-fixed --fail-on high
    docker compose run --rm grype oci-dir:/image --platform linux/arm64 --only-fixed --fail-on high
    docker compose run --rm grype oci-dir:/image-postgres --platform linux/amd64 --only-fixed --fail-on high
    docker compose run --rm grype oci-dir:/image-postgres --platform linux/arm64 --only-fixed --fail-on high

# Runs the end-to-end tests against the images in target, on the platform E2E_PLATFORM, which is
# this machine's unless named (TEST-04). The tests run the images from the registry that Compose
# runs, against the Postgres that `just services` starts. Another platform needs the system to run
# its binaries, such as through QEMU. Every other image the tests pull is fetched first: this
# machine's, and, through the mirror, their servers', which run on this machine's platform, so no
# test reaches a registry outside (TEST-07).
e2e $E2E_PLATFORM=native_platform: services
    docker compose up --detach --wait registry mirror
    docker compose run --rm crane push --insecure /image registry:5000/{{ e2e_image }}:e2e
    docker compose run --rm crane push --insecure /image-postgres registry:5000/{{ e2e_image }}-postgres:e2e
    docker compose pull --quiet server
    docker compose --profile observability pull --quiet collector prometheus loki tempo
    {{ xtask }} mirror {{ native_platform }}
    cargo test --locked --package app --features e2e --test e2e

# Measures one replica's capacity, for the release notes (OPS-18): runs the API's image in
# target/image, on this machine's platform, with the reference deployment's limits on CPU and
# memory for a replica, and sends it requests at rising constant rates through k6, from the image
# that compose.yaml pins. It prints each step, then the highest rate served with none failed and
# the 99th percentile within the latency objective. docs/operations/release.md says when to run it.
load-test: services
    docker compose --profile observability up --detach --wait collector prometheus loki tempo
    docker compose up --detach --wait registry
    docker compose run --rm crane push --insecure /image registry:5000/{{ e2e_image }}:e2e
    {{ xtask }} load-test

# Pushes each image in target to the registry as sha-<commit>, under the name of `repository`,
# such as owner/name, or that name with `-postgres` for the Postgres image, and moves `edge` to it
# while the commit is still the head of main (OPS-14). CI runs it on main, once every check has
# passed, after a `docker login` to the registry.
[unix]
publish repository commit:
    #!/usr/bin/env bash
    set -euo pipefail
    repository={{ image_registry }}/{{ lowercase(repository) }}
    crane() { docker compose run --rm --user "$(id -u):$(id -g)" --volume "$HOME/.docker:/docker:ro" --env DOCKER_CONFIG=/docker crane "$@"; }
    for suffix in "" -postgres; do
        tested=$(grep -o '"digest":"sha256:[0-9a-f]*"' "{{ image_dir }}$suffix/index.json" | head -n 1 | cut -d '"' -f 4)
        # The registry can refuse a manifest moments after the first one created a new package,
        # so a failed push is tried again. Pushing again pushes only what is missing.
        for attempt in 1 2 3 4 5; do
            if crane push "/image$suffix" "$repository$suffix:sha-{{ commit }}"; then break; fi
            if [ "$attempt" = 5 ]; then exit 1; fi
            echo "the push failed; pushing again in 15 seconds" >&2
            sleep 15
        done
        pushed=$(crane digest "$repository$suffix:sha-{{ commit }}")
        if [ "$pushed" != "$tested" ]; then echo "the registry holds $pushed, not the tested $tested" >&2; exit 1; fi
    done
    if [ "$(git ls-remote origin refs/heads/main | cut -f 1)" = "{{ commit }}" ]; then
        for suffix in "" -postgres; do crane tag "$repository$suffix:sha-{{ commit }}" edge; done
    else
        echo "{{ commit }} is no longer the head of main, so edge stays where it is"
    fi

# Releases main as `version`, X.Y.Z, or X.Y.Z-rc.N for a prerelease (OPS-14): checks that main can
# release it, sets the app's version to it, commits, and pushes main. The run on main that passes
# with that version releases it. docs/operations/release.md describes a release.
release version:
    {{ xtask }} release {{ version }}

# Deploys the release `version` to the reference deployment on `host`, an SSH destination such as
# deploy@203.0.113.7 or a Host of ~/.ssh/config (OPS-16): refuses a version that no release has,
# copies the deployment's files from the release's tag to the server, and runs that release's
# deploy/deploy.sh there, which verifies the release's images before it pulls them. `--allow-restart`
# lets it restart Postgres or Caddy. docs/operations/deploy.md describes a deploy.
deploy host version *options:
    {{ xtask }} deploy {{ host }} {{ version }} {{ options }}

# Takes a full base backup of the bundled database of the reference deployment on `host` (OPS-17).
backup host:
    {{ xtask }} backup {{ host }}

# Restores the bundled database of the reference deployment on `host`, as it stood at `moment`, a
# time with its offset from UTC as Postgres writes it, or at the latest moment its backups hold
# (OPS-17).
# docs/operations/backups.md describes a restore.
restore host moment="":
    {{ xtask }} restore {{ host }} "{{ moment }}"

# Releases the app's version from the images this run on main published for `commit`, if a commit
# since the latest release asks for it and no release has it yet (OPS-14): gives each image the
# version's tag, signs it, verifies the release, and only then tags the commit v<version>. The
# request is the commit `just release` makes, whose subject is "Release <version>". `repository`
# is the GitHub repository, such as owner/name, and `source` its address. CI runs it on main once
# every check has passed, after a `docker login`, and each failure says what to do next.
[unix]
release-image repository commit source:
    #!/usr/bin/env bash
    set -euo pipefail
    image={{ image_registry }}/{{ lowercase(repository) }}
    crane() { docker compose run --rm --user "$(id -u):$(id -g)" --volume "$HOME/.docker:/docker:ro" --env DOCKER_CONFIG=/docker crane "$@"; }
    # cosign keeps Sigstore's trust data in its home, which the runner's user can write only in /tmp.
    cosign() { docker compose run --rm --user "$(id -u):$(id -g)" --env HOME=/tmp --volume "$HOME/.docker:/docker:ro" --env DOCKER_CONFIG=/docker --env ACTIONS_ID_TOKEN_REQUEST_URL --env ACTIONS_ID_TOKEN_REQUEST_TOKEN cosign "$@"; }
    # An error annotation, which GitHub shows on the run's page.
    fail() { echo "::error::$1" >&2; exit 1; }
    version={{ version }}
    git fetch --quiet --tags origin
    if [ -n "$(git tag --list "v$version")" ]; then
        echo "v$version is released already, so this commit is published but not released. To release it, run \`just release\` with a new version."
        exit 0
    fi
    if latest=$(git describe --tags --abbrev=0 --match 'v[0-9]*' "{{ commit }}" 2> /dev/null); then since="$latest..{{ commit }}"; else since="{{ commit }}"; fi
    # Captured first: grep stops reading at the first match, which would fail git log's write. The
    # request matches as `asks_for` in crates/xtask/src/release.rs does: its subject alone, or with
    # the number a squash merge appends.
    subjects=$(git log --format=%s "$since")
    if ! grep --quiet --extended-regexp --line-regexp "Release ${version//./\\.}( \(#[0-9]+\))?" <<< "$subjects"; then
        echo "No commit since the latest release asks for the release of $version, so this commit is published but not released. To release it, run \`just release\`."
        exit 0
    fi
    if ! refused=$({{ xtask }} release-check "$version" 2>&1); then fail "$refused"; fi
    released=""
    for suffix in "" -postgres; do
        digest=$(crane digest "$image$suffix:sha-{{ commit }}") \
            || fail "This run's publish job pushed no image of {{ commit }} to $image$suffix, so $version isn't released. Re-run this run's failed jobs."
        crane tag "$image$suffix@$digest" "$version" \
            || fail "The registry refused the tag $version of $image$suffix, so $version isn't released. Re-run this job."
        cosign sign --yes "$image$suffix@$digest" \
            || fail "Signing failed, so $version isn't released. Re-run this job; if it fails again, Sigstore may be down: https://status.sigstore.dev."
        released+="$image$suffix@$digest"$'\n'
    done
    verified=$(just verify-release {{ repository }} "$version" {{ source }}) \
        || fail "The release of $version doesn't verify, so it isn't released; the error above says why. Re-run this job."
    if [ "$verified"$'\n' != "$released" ]; then fail "The release of $version names other images than the tested ones, so it isn't released: $verified. Re-run this job."; fi
    { git tag "v$version" "{{ commit }}" && git push --quiet origin "v$version"; } \
        || fail "The images of $version are released, but GitHub refused the tag v$version. Re-run this job."
    echo "Released $version:"
    echo "$released"

# Fails unless each image of `repository`, such as owner/name, tagged `version` is signed by the
# run on main of the repository at `source`, with the SBOM and the provenance of both platforms,
# and prints each one's digest (OPS-14). It runs deploy/verify-release.sh, which the reference
# deployment's deploy also runs before it pulls a release (OPS-16).
[unix]
verify-release repository version source:
    CRANE="{{ crane_image }}" COSIGN="{{ cosign_image }}" deploy/verify-release.sh "{{ version }}" "{{ source }}" \
        "{{ image_registry }}/{{ lowercase(repository) }}" "{{ image_registry }}/{{ lowercase(repository) }}-postgres"

# Scans the published images of `repository`, such as owner/name, again: `edge`, the newest final
# release and the newest prerelease of each image, for vulnerabilities published since they were
# tested (SEC-13). The scheduled workflow runs it every week, after a `docker login`.
[unix]
image-rescan repository:
    #!/usr/bin/env bash
    set -euo pipefail
    # crane runs as the runner's user, who alone can read the registry login. Grype runs as its
    # image's root, which can read it too, and write the database to the volume Compose made.
    crane() { docker compose run --rm --user "$(id -u):$(id -g)" --volume "$HOME/.docker:/docker:ro" --env DOCKER_CONFIG=/docker crane "$@"; }
    grype() { docker compose run --rm --volume "$HOME/.docker:/docker:ro" --env DOCKER_CONFIG=/docker grype "$@"; }
    for suffix in "" -postgres; do
        repository={{ image_registry }}/{{ lowercase(repository) }}$suffix
        tags=$(crane ls "$repository")
        release=$(grep -E '^[0-9]+\.[0-9]+\.[0-9]+$' <<< "$tags" | sort -V | tail -n 1 || true)
        prerelease=$(grep -E '^[0-9]+\.[0-9]+\.[0-9]+-rc\.[0-9]+$' <<< "$tags" | sort -V | tail -n 1 || true)
        for tag in edge $release $prerelease; do
            for platform in linux/amd64 linux/arm64; do
                grype "registry:$repository:$tag" --platform "$platform" --only-fixed --fail-on high
            done
        done
    done
