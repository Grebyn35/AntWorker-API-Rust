# The tasks of development: preparing a machine, and running the API, its tests and its
# formatting. The justfile imports this file.

# Prepares this machine (OPS-20): checks the tools the tasks need and their pinned versions,
# installs the toolchain that rust-toolchain.toml pins, and writes .env. watchexec and sqlx-cli
# only get a warning, because only `dev`, and `prepare` and `sqlx-check`, need them.
[unix]
[doc("Checks the tools and their pinned versions, and writes .env")]
[group('develop')]
setup: && env _env-drift
    for tool in rustup git docker; do command -v "$tool" > /dev/null || { echo "just setup needs $tool" >&2; exit 1; }; done
    docker compose version > /dev/null || { echo "just setup needs Docker Compose" >&2; exit 1; }
    [ "$(just --version | head -n 1 | cut -d ' ' -f 2)" = "{{ just_version }}" ] || { echo "just setup needs just {{ just_version }}" >&2; exit 1; }
    [ "$(cargo deny --version 2> /dev/null | head -n 1 | cut -d ' ' -f 2)" = "{{ cargo_deny_version }}" ] || { echo "just setup needs cargo-deny {{ cargo_deny_version }}" >&2; exit 1; }
    [ "$(watchexec --version 2> /dev/null | head -n 1 | cut -d ' ' -f 2)" = "{{ watchexec_version }}" ] || echo "warning: just dev needs watchexec {{ watchexec_version }}" >&2
    [ "$(cargo sqlx --version 2> /dev/null | head -n 1 | cut -d ' ' -f 2)" = "{{ sqlx_cli_version }}" ] || echo "warning: just prepare and just sqlx-check need sqlx-cli {{ sqlx_cli_version }}" >&2
    rustup show active-toolchain > /dev/null || rustup toolchain install

# Writes .env from .env.example, with generated passwords, if .env doesn't exist yet. Compose
# reads it too, so a CI job that runs only Compose's tools runs this instead of `just setup`.
[unix]
[doc("Writes .env from .env.example, with generated passwords, unless it exists")]
[group('develop')]
env:
    test -f .env || sed \
        -e "s/__ADMIN_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        -e "s/__OWNER_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        -e "s/__APP_PASSWORD__/$(od -An -N16 -tx1 /dev/urandom | tr -d ' \n')/g" \
        -e "s|__HMAC_KEY__|$(head -c 32 /dev/urandom | base64 | tr -d '\n')|g" \
        .env.example > .env

# Prepares this machine (OPS-20): checks the tools the tasks need and their pinned versions,
# installs the toolchain that rust-toolchain.toml pins, and writes .env. watchexec and sqlx-cli
# only get a warning, because only `dev`, and `prepare` and `sqlx-check`, need them.
[windows]
[doc("Checks the tools and their pinned versions, and writes .env")]
[group('develop')]
setup: && env _env-drift
    foreach ($tool in 'rustup', 'git', 'docker') { if (-not (Get-Command $tool -ErrorAction SilentlyContinue)) { throw "just setup needs $tool" } }
    docker compose version | Out-Null; if ($LASTEXITCODE -ne 0) { throw 'just setup needs Docker Compose' }
    function Version($command) { try { (((& $command[0] $command[1..9] 2> $null) | Select-Object -First 1) -split ' ')[1] } catch { '' } }; if ((Version 'just', '--version') -ne '{{ just_version }}') { throw 'just setup needs just {{ just_version }}' }; if ((Version 'cargo', 'deny', '--version') -ne '{{ cargo_deny_version }}') { throw 'just setup needs cargo-deny {{ cargo_deny_version }}' }; if ((Version 'watchexec', '--version') -ne '{{ watchexec_version }}') { Write-Warning 'just dev needs watchexec {{ watchexec_version }}' }; if ((Version 'cargo', 'sqlx', '--version') -ne '{{ sqlx_cli_version }}') { Write-Warning 'just prepare and just sqlx-check need sqlx-cli {{ sqlx_cli_version }}' }
    rustup show active-toolchain | Out-Null; if ($LASTEXITCODE -ne 0) { rustup toolchain install }

# Writes .env from .env.example, with generated passwords, if .env doesn't exist yet. Compose
# reads it too, so a CI job that runs only Compose's tools runs this instead of `just setup`.
[windows]
[doc("Writes .env from .env.example, with generated passwords, unless it exists")]
[group('develop')]
env:
    if (-not (Test-Path .env)) { $text = Get-Content .env.example -Raw; foreach ($name in 'ADMIN', 'OWNER', 'APP') { $bytes = New-Object byte[] 16; [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($bytes); $text = $text.Replace("__$($name)_PASSWORD__", -join ($bytes | ForEach-Object { $_.ToString('x2') })) }; $key = New-Object byte[] 32; [Security.Cryptography.RandomNumberGenerator]::Create().GetBytes($key); $text = $text.Replace('__HMAC_KEY__', [Convert]::ToBase64String($key)); [IO.File]::WriteAllText((Join-Path $PWD '.env'), $text) }

# Starts the services in compose.yaml, and waits until they are healthy.
[doc("Starts the services in compose.yaml, such as Postgres")]
[group('develop')]
services:
    docker compose up --detach --wait

# Creates the database roles and their grants (DATA-12).
[doc("Creates the database roles and their grants")]
[group('develop')]
bootstrap: services
    {{ api }} bootstrap

# Applies the migrations the database hasn't recorded. Run it after adding a migration, before
# writing queries against it.
[doc("Applies the migrations the database hasn't recorded")]
[group('develop')]
migrate: bootstrap
    {{ api }} migrate

[doc("Runs the API with the settings in .env")]
[group('develop')]
run: migrate
    {{ api }} serve

# Fills the development database with demo accounts and workspaces, through the API's routes, and
# prints how to sign in (SYSTEM-003). It fills a database once; README.md says how to start again.
[doc("Fills the development database with demo accounts and workspaces")]
[group('develop')]
seed: migrate
    {{ api }} seed

# Runs the API on this machine with auto-reload (OPS-20): starts Postgres and bootstraps the
# database, then applies new migrations and restarts the API whenever its code or a migration
# changes. `just dev observe` also starts the observability stack and exports the API's
# telemetry to it; docs/operations/observability.md lists where each service answers.
[unix]
[doc("Runs the API with auto-reload; `just dev observe` adds the observability stack")]
[group('develop')]
dev $mode="": _env-drift bootstrap
    case "$mode" in "") ;; observe) docker compose --profile observability up --detach --wait ;; *) echo "just dev takes nothing, or observe" >&2; exit 1 ;; esac
    {{ if mode == "observe" { "OTEL_EXPORTER_OTLP_ENDPOINT=http://127.0.0.1:4318 " } else { "" } }}watchexec --restart --shell=none --watch crates --watch migrations --exts rs,sql,toml -- just _dev-serve

# Runs the API on this machine with auto-reload (OPS-20): starts Postgres and bootstraps the
# database, then applies new migrations and restarts the API whenever its code or a migration
# changes. `just dev observe` also starts the observability stack and exports the API's
# telemetry to it; docs/operations/observability.md lists where each service answers.
# watchexec runs `just` without a shell: through its default shell on Windows, `just` receives
# no recipe name and only lists the tasks.
[windows]
[doc("Runs the API with auto-reload; `just dev observe` adds the observability stack")]
[group('develop')]
dev mode="": _env-drift bootstrap
    if ('{{ mode }}' -eq 'observe') { docker compose --profile observability up --detach --wait; if ($LASTEXITCODE -ne 0) { throw 'the observability stack did not start' }; $env:OTEL_EXPORTER_OTLP_ENDPOINT = 'http://127.0.0.1:4318' } elseif ('{{ mode }}' -ne '') { throw 'just dev takes nothing, or observe' }; watchexec --restart --shell=none --watch crates --watch migrations --exts rs,sql,toml -- just _dev-serve

# Warns of each setting of .env.example that .env lacks, or still sets to an earlier value of the
# example's (OPS-20): `just setup` writes .env once, and later commits add settings and change
# development values.
_env-drift:
    {{ xtask }} env-drift

# Applies new migrations, then serves. `just dev` runs it again whenever the code changes.
_dev-serve:
    {{ api }} migrate
    {{ api }} serve

[doc("Formats the code")]
[group('develop')]
fmt:
    cargo fmt --all

# Runs every test. The integration tests use the services.
[doc("Runs the tests, which start Postgres through Compose")]
[group('develop')]
test: services
    cargo test --workspace --locked
