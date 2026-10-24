# Every task, run the same way locally and in CI (OPS-12).

set dotenv-load
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# Builds check queries against the committed cache in .sqlx/, as CI does (DATA-20).
export SQLX_OFFLINE := "true"

api := "cargo run --locked --package app --bin antworker-api --"
xtask := "cargo run --locked --quiet --package xtask --"

# The versions of the tools the tasks use besides the Rust toolchain, which rust-toolchain.toml
# pins (OPS-13). `just setup` checks them, and CI installs the ones `just ci` needs, which
# `just ci-tools` lists. CI installs `just` itself before any task can run, so its workflows pin
# the same version. sqlx-cli matches the sqlx in Cargo.lock.
just_version := "1.59.0"
cargo_deny_version := "0.20.2"
watchexec_version := "2.8.0"
sqlx_cli_version := "0.9.0"

# Runs every check CI runs.
ci: fmt-check lint test deny check-test-map check-docs check-openapi check-instrument check-metrics check-telemetry-config alert-rules

# Lists the tools `just ci` needs besides just and the toolchain, as CI's install step takes them.
ci-tools:
    @echo cargo-deny@{{ cargo_deny_version }}

# Prepares this machine (OPS-19): checks the tools the tasks need and their pinned versions,
# installs the toolchain that rust-toolchain.toml pins, and writes .env from .env.example, with
# generated passwords, if .env doesn't exist yet. watchexec and sqlx-cli only get a warning, because
# only `dev` and `prepare` need them.
[unix]
setup:
    for tool in rustup git docker; do command -v "$tool" > /dev/null || { echo "just setup needs $tool" >&2; exit 1; }; done
    docker compose version > /dev/null || { echo "just setup needs Docker Compose" >&2; exit 1; }
    [ "$(just --version | head -n 1 | cut -d ' ' -f 2)" = "{{ just_version }}" ] || { echo "just setup needs just {{ just_version }}" >&2; exit 1; }
    [ "$(cargo deny --version 2> /dev/null | head -n 1 | cut -d ' ' -f 2)" = "{{ cargo_deny_version }}" ] || { echo "just setup needs cargo-deny {{ cargo_deny_version }}" >&2; exit 1; }
    [ "$(watchexec --version 2> /dev/null | head -n 1 | cut -d ' ' -f 2)" = "{{ watchexec_version }}" ] || echo "warning: just dev needs watchexec {{ watchexec_version }}" >&2
    [ "$(cargo sqlx --version 2> /dev/null | head -n 1 | cut -d ' ' -f 2)" = "{{ sqlx_cli_version }}" ] || echo "warning: just prepare needs sqlx-cli {{ sqlx_cli_version }}" >&2
    rustup show active-toolchain > /dev/null || rustup toolchain install
    test -f .env || sed \
        -e "s/__ADMIN_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        -e "s/__OWNER_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        -e "s/__APP_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        .env.example > .env

# Prepares this machine (OPS-19): checks the tools the tasks need and their pinned versions,
# installs the toolchain that rust-toolchain.toml pins, and writes .env from .env.example, with
# generated passwords, if .env doesn't exist yet. watchexec and sqlx-cli only get a warning, because
# only `dev` and `prepare` need them.
[windows]
setup:
    foreach ($tool in 'rustup', 'git', 'docker') { if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { throw "just setup needs $tool" } }
    docker compose version | Out-Null; if ($LASTEXITCODE -ne 0) { throw 'just setup needs Docker Compose' }
    function Version($command) { try { (((& $command[0] $command[1..9] 2> $null) | Select-Object -First 1) -split ' ')[1] } catch { '' } }; if ((Version 'just', '--version') -ne '{{ just_version }}') { throw 'just setup needs just {{ just_version }}' }; if ((Version 'cargo', 'deny', '--version') -ne '{{ cargo_deny_version }}') { throw 'just setup needs cargo-deny {{ cargo_deny_version }}' }; if ((Version 'watchexec', '--version') -ne '{{ watchexec_version }}') { Write-Warning 'just dev needs watchexec {{ watchexec_version }}' }; if ((Version 'cargo', 'sqlx', '--version') -ne '{{ sqlx_cli_version }}') { Write-Warning 'just prepare needs sqlx-cli {{ sqlx_cli_version }}' }
    rustup show active-toolchain | Out-Null; if ($LASTEXITCODE -ne 0) { rustup toolchain install }
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
dev mode="": bootstrap
    case "{{ mode }}" in "") ;; observe) docker compose --profile observability up --detach --wait ;; *) echo "just dev takes nothing, or observe" >&2; exit 1 ;; esac
    {{ if mode == "observe" { "OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 " } else { "" } }}watchexec --restart --shell=none --watch crates --watch migrations --exts rs,sql,toml -- just _dev-serve

# Runs the API on this machine with auto-reload (OPS-19): starts Postgres and bootstraps the
# database, then applies new migrations and restarts the API whenever its code or a migration
# changes. `just dev observe` also starts the observability stack and exports the API's
# telemetry to it; docs/operations/observability.md lists where each service answers.
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

# Runs clippy with warnings as errors (CODE-03).
lint:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Runs every test. The integration tests use the services.
test: services
    cargo test --workspace --locked

# Rewrites the query cache in .sqlx/ from the migrated database. Run it after changing a query
# (DATA-20). It needs the sqlx-cli of `sqlx_cli_version`: `cargo install sqlx-cli --version
# <version> --no-default-features --features rustls,postgres --locked`.
prepare: services
    cargo sqlx prepare --workspace -- --all-targets --locked

# Checks dependencies for advisories, licenses and banned crates (CODE-11, CODE-12, SEC-13).
deny:
    cargo deny --locked check

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
# deploy/observability/metrics.yaml, a metric nothing reads, or a panel title that isn't a
# question (OBS-11 to OBS-13).
check-metrics:
    {{ xtask }} check-metrics

# Fails on a collector configuration whose tail sampling doesn't keep the traces OBS-08 names, or a
# latency objective that differs between the sampling, the alerts and the latency dashboard.
check-telemetry-config:
    {{ xtask }} check-telemetry-config

# Fails on an alert without a severity, a runbook section or a test, then runs promtool's checks
# and tests of the rules, with the Prometheus image that compose.yaml pins (OBS-14).
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
