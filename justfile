# Every task, run the same way locally and in CI (OPS-12).

set dotenv-load
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

# Runs every check CI runs.
ci: fmt-check lint test deny

# Formats the code.
fmt:
    cargo fmt --all

# Fails if the code isn't formatted.
fmt-check:
    cargo fmt --all -- --check

# Runs clippy with warnings as errors (CODE-03).
lint:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Runs every test.
test:
    cargo test --workspace --locked

# Checks dependencies for advisories, licenses and banned crates (CODE-11, CODE-12, SEC-13).
deny:
    cargo deny --locked check

# Runs the API with the settings in .env.
run:
    cargo run --locked --package app --bin antworker-api -- serve

# Writes .env from .env.example if it doesn't exist yet.
[unix]
setup:
    test -f .env || cp .env.example .env

# Writes .env from .env.example if it doesn't exist yet.
[windows]
setup:
    if (-not (Test-Path .env)) { Copy-Item .env.example .env }
