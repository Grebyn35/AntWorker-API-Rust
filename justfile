# Every task, run the same way locally and in CI (OPS-12).

set dotenv-load
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# Builds check queries against the committed cache in .sqlx/, as CI does (DATA-20).
export SQLX_OFFLINE := "true"

api := "cargo run --locked --package app --bin antworker-api --"

# Runs every check CI runs.
ci: fmt-check lint test deny

# Writes .env from .env.example, with generated passwords, if .env doesn't exist yet.
[unix]
setup:
    test -f .env || sed \
        -e "s/__ADMIN_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        -e "s/__OWNER_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        -e "s/__APP_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        .env.example > .env

# Writes .env from .env.example, with generated passwords, if .env doesn't exist yet.
[windows]
setup:
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
# (DATA-20).
prepare: services
    cargo sqlx prepare --workspace -- --all-targets --locked

# Checks dependencies for advisories, licenses and banned crates (CODE-11, CODE-12, SEC-13).
deny:
    cargo deny --locked check
