# Every task, run the same way locally and in CI (OPS-12).

set dotenv-load
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# Builds check queries against the committed cache in .sqlx/, as CI does (DATA-20).
export SQLX_OFFLINE := "true"

api := "cargo run --locked --package app --bin antworker-api --"
xtask := "cargo run --locked --quiet --package xtask --"

# Runs every check CI runs.
ci: fmt-check lint test deny check-test-map check-docs

# Prepares this machine (OPS-19): checks the tools the tasks need, installs the toolchain that
# rust-toolchain.toml pins, and writes .env from .env.example, with generated passwords, if .env
# doesn't exist yet. A missing sqlx-cli only gets a warning, because only `prepare` needs it.
[unix]
setup:
    for tool in rustup git docker cargo-deny; do command -v "$tool" > /dev/null || { echo "just setup needs $tool" >&2; exit 1; }; done
    docker compose version > /dev/null || { echo "just setup needs Docker Compose" >&2; exit 1; }
    command -v cargo-sqlx > /dev/null || echo "warning: just prepare needs sqlx-cli" >&2
    rustup show active-toolchain > /dev/null || rustup toolchain install
    test -f .env || sed \
        -e "s/__ADMIN_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        -e "s/__OWNER_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        -e "s/__APP_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        .env.example > .env

# Prepares this machine (OPS-19): checks the tools the tasks need, installs the toolchain that
# rust-toolchain.toml pins, and writes .env from .env.example, with generated passwords, if .env
# doesn't exist yet. A missing sqlx-cli only gets a warning, because only `prepare` needs it.
[windows]
setup:
    foreach ($tool in 'rustup', 'git', 'docker', 'cargo-deny') { if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { throw "just setup needs $tool" } }
    docker compose version | Out-Null; if ($LASTEXITCODE -ne 0) { throw 'just setup needs Docker Compose' }
    if (-not (Get-Command cargo-sqlx -ErrorAction SilentlyContinue)) { Write-Warning 'just prepare needs sqlx-cli' }
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
# (DATA-20). It needs sqlx-cli 0.9: `cargo install sqlx-cli --version 0.9.0
# --no-default-features --features rustls,postgres --locked`.
prepare: services
    cargo sqlx prepare --workspace -- --all-targets --locked

# Checks dependencies for advisories, licenses and banned crates (CODE-11, CODE-12, SEC-13).
deny:
    cargo deny --locked check

# Reports which spec behaviors tests prove, and fails on a test whose name doesn't start with a
# behavior or rule ID that exists (TEST-01, DOC-10).
check-test-map:
    {{ xtask }} check-test-map

# Fails on a link, ID or named CI check that doesn't resolve, a history marker, an
# unfinished-work marker, or a document outside the documentation set (DOC-01, DOC-07, DOC-08,
# DOC-13).
check-docs:
    {{ xtask }} check-docs
