# The checks that `just checks`, `just check-removable` and `just check-new-module` run, and the
# lists of the tools they need, which CI installs. The justfile imports this file.
# docs/operations/ci.md names the rules that each check enforces.

# Lists the tools `just checks` needs besides just and the toolchain, as CI's install step takes
# them.
[private]
ci-tools:
    @echo cargo-deny@{{ cargo_deny_version }},sqlx-cli@{{ sqlx_cli_version }}

# Lists the tool `just setup-smoke` needs besides those of `just checks`: watchexec, whose
# command-line crate is watchexec-cli, as CI's install step takes it.
[private]
smoke-tools:
    @echo watchexec-cli@{{ watchexec_version }}

# Lists the one tool `just setup` requires besides just and the toolchain, which the scheduled
# jobs that need no other install alone: the daily check of advisories, and the weekly smoke test
# on Windows.
[private]
setup-tools:
    @echo cargo-deny@{{ cargo_deny_version }}

# Runs `just dev` until the API it starts answers ready, then stops it. CI's `checks` job runs it
# after `just setup` and `just checks`, so a new clone's two commands run end to end; on a
# developer's machine, `just dev` itself is this check, so `just ci` leaves it out. A failure shows
# what `just dev` wrote.
[linux]
[private]
setup-smoke: bound-build
    #!/usr/bin/env bash
    set -euo pipefail
    mkdir -p target
    log=target/setup-smoke.log
    # just, watchexec and the API in a session of their own, which the end stops whole.
    setsid just dev > "$log" 2>&1 &
    dev=$!
    trap 'kill -TERM -- "-$dev" 2> /dev/null || true; wait "$dev" || true' EXIT
    waited=0
    until curl --silent --fail --max-time 5 --output /dev/null "http://$LISTEN_ADDR/health/ready"; do
        # The API's build is cached by `just checks`, so ten minutes means it stopped short of
        # serving, such as on a build error, after which watchexec waits for a change.
        if ! kill -0 "$dev" 2> /dev/null || grep -q 'startup failed' "$log" || [ "$waited" -ge 600 ]; then
            cat "$log" >&2
            echo "just dev didn't serve at $LISTEN_ADDR" >&2
            exit 1
        fi
        sleep 1
        waited=$((waited + 1))
    done
    echo "just dev serves at $LISTEN_ADDR"

# Builds every target of the workspace. Windows's hosted runners can't run the Linux containers that
# `just dev` starts, so CI runs `just setup` and this there every week.
[windows]
[private]
setup-smoke: bound-build
    cargo build --workspace --all-targets --locked

# The build directory of the dev and test profiles, in the directory that CARGO_TARGET_DIR names,
# as for the copies of check-removable and check-new-module, or in target/.
build_dir := env("CARGO_TARGET_DIR", "target") / "debug"

# Removes the build directory once it holds more than 40 gigabytes, so the build after it starts
# afresh (OPS-21): Cargo keeps the artifacts of every earlier set of dependencies and settings
# beside the current ones, and removes none of them itself.
[unix]
[private]
bound-build:
    if [ -d "{{ build_dir }}" ] && [ "$(du -sk "{{ build_dir }}" | cut -f1)" -gt 41943040 ]; then echo "{{ build_dir }} holds more than 40 GB, which the build after it starts afresh from nothing"; cargo clean --profile dev; fi

# Removes the build directory once it holds more than 40 gigabytes, so the build after it starts
# afresh (OPS-21): Cargo keeps the artifacts of every earlier set of dependencies and settings
# beside the current ones, and removes none of them itself.
[windows]
[private]
bound-build:
    if ((Test-Path -LiteralPath '{{ build_dir }}') -and ((Get-ChildItem -LiteralPath '{{ build_dir }}' -Recurse -File -Force -ErrorAction SilentlyContinue | Measure-Object -Property Length -Sum).Sum -gt 40GB)) { Write-Output '{{ build_dir }} holds more than 40 GB, which the build after it starts afresh from nothing'; cargo clean --profile dev; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE } }

[doc("Fails if the code isn't formatted")]
[group('check')]
fmt-check:
    cargo fmt --all -- --check

# Runs clippy with warnings as errors: on the targets the image builds, with their default
# features, where an item that only the tests' code uses is dead (CODE-15), then on every target
# with every feature, so the tests, the end-to-end ones included, are linted too.
[doc("Runs clippy, with warnings as errors")]
[group('check')]
lint: bound-build
    cargo clippy --workspace --locked -- -D warnings
    cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

# Checks dependencies: advisories, bans and sources for every target, and licenses for each target
# the API is built for, the platforms a developer builds on and the image's, one at a time, as
# cargo-deny takes one target per run. A crate that only other targets compile, such as one for
# WebAssembly, never reaches a build, so its license doesn't matter.
[doc("Checks the dependencies' advisories, bans, sources and licenses")]
[group('check')]
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
# behavior or rule ID that exists.
[doc("Reports which spec behaviors tests prove")]
[group('check')]
check-test-map:
    {{ xtask }} check-test-map

# Fails on a part of openapi.json that breaks the rules for the HTTP contract, of those that
# docs/operations/ci.md says it reads.
[doc("Fails on a part of openapi.json that breaks the HTTP contract's rules")]
[group('check')]
check-openapi:
    {{ xtask }} check-openapi

# Fails on a log or span field name outside the list in platform::telemetry::names, a message that
# isn't constant, an #[instrument] without skip_all, an event at any level but DEBUG, or a span, in
# app code other than main.rs, and an exception to a print lint outside the places printing is
# allowed.
[doc("Fails on logs, spans and prints that break the telemetry rules")]
[group('check')]
check-instrument:
    {{ xtask }} check-instrument

# Fails on a metric declared or named outside OBS-11, a catalog that differs from the declarations,
# a dashboard or alert that reads a metric neither declared nor listed in
# deploy/observability/metrics.yaml, a metric nothing reads, a panel title that isn't a question, a
# dashboard that Grafana's users can change, or dashboards no overview links to all of.
[doc("Fails on metrics, dashboards and alerts that break the metric rules")]
[group('check')]
check-metrics:
    {{ xtask }} check-metrics

# Fails on a tail sampling of the collector that doesn't keep the traces OBS-08 names, a
# configuration of the managed backend or of the proxy that doesn't keep that sampling, or a
# latency objective that differs between the sampling, the alerts and the latency dashboard.
[doc("Fails on a collector configuration that breaks the sampling rules")]
[group('check')]
check-telemetry-config:
    {{ xtask }} check-telemetry-config

# Fails on an alert without a severity, a runbook section or a test, then runs promtool's checks and
# tests of the rules, with the Prometheus image that deploy/observability/compose.yaml pins.
[doc("Checks the alert rules, and runs their tests")]
[group('check')]
alert-rules:
    {{ xtask }} alert-rules
    docker compose --profile observability run --rm --no-deps --volume ./deploy/observability/prometheus:/rules:ro --workdir /rules --entrypoint promtool prometheus check rules rules.yaml
    docker compose --profile observability run --rm --no-deps --volume ./deploy/observability/prometheus:/rules:ro --workdir /rules --entrypoint promtool prometheus test rules rules.test.yaml

# Fails on a link, ID or named CI check that doesn't resolve, a history marker, an unfinished-work
# marker, a document outside the documentation set, an acronym not spelled out where a document
# first uses it, a spec, rule or tracker whose format breaks DOC-08 or DOC-14, or a spec's Routes
# table that doesn't match openapi.json.
[doc("Fails on a document whose links, IDs or format break the rules")]
[group('check')]
check-docs:
    {{ xtask }} check-docs

# Fails on an action, runner, image, download, crate install or tool version that isn't pinned, a
# toolchain other than the one rust-toolchain.toml pins, or a crate whose edition isn't the
# workspace's 2024.
[doc("Fails on a tool, image, action or download that isn't pinned")]
[group('check')]
check-pins:
    {{ xtask }} check-pins

# Fails on a finding of shellcheck, run from the image compose.yaml pins, in a recipe of this
# justfile that runs on Linux and macOS, or in a shell script under deploy/, and on a psql of such a
# script whose statement isn't given with -c or names a table of the product schema. It reads the
# recipes from this file, so it checks the same ones on every platform.
[doc("Runs shellcheck on the recipes and the deployment's scripts")]
[group('check')]
check-shell:
    {{ xtask }} check-shell

# Fails on a function, method or macro that the locked sqlx exposes and that takes SQL text, outside
# the checked macros, and that clippy.toml doesn't ban, or on a ban of sqlx's that reaches nothing.
[doc("Fails on a function of sqlx that takes SQL text and that clippy.toml doesn't ban")]
[group('check')]
check-sql-bans:
    {{ xtask }} check-sql-bans

# Fails on sqlx in a module's routes, an HTTP type in its service, a routes type that deserializes
# without refusing unknown fields, an infrastructure error in a service or route signature, or an
# HTTP client built outside the places SEC-10 allows.
[doc("Fails on code that breaks a module's layers, or builds an HTTP client where SEC-10 forbids")]
[group('check')]
check-layers:
    {{ xtask }} check-layers

# Fails on a module's public submodule, code that reaches into another module's submodule, a use of
# a module that the graph in docs/architecture.md doesn't allow, a cycle in that graph, or another
# module's error in a module's public signatures, or a dependency that its crate's code never names.
[doc("Fails on a dependency between modules that the graph doesn't allow, or one no code uses")]
[group('check')]
check-deps:
    {{ xtask }} check-deps

# Fails on a source file before its tests, or a deploy script, over 500 lines, a module's file
# outside ARCH-03's set or a module without its error.rs, an exception to dead_code in the API's
# code, and a job declared outside its module's jobs.rs or with another module's kind.
[doc("Fails on a file too long, or out of its place")]
[group('check')]
check-layout:
    {{ xtask }} check-layout

# Fails on a name, a standard column, a check constraint, a function, a foreign key, a soft
# delete or a role's privilege in the migrations' schema that breaks the data rules, in the
# catalog of a database that the API's own bootstrap and migrate build on the Postgres that
# `services` starts.
[doc("Fails on a table, constraint, index or grant of the migrations that breaks the data rules")]
[group('check')]
check-schema: services
    {{ xtask }} check-schema

# Fails on a marker of an optional part that is wrong, or in a Rust file isn't Rust's `//` comment
# alone on its line, on an attribute above a Rust block that holds its item, on a marker before
# more of its line outside a table row, and, for each run that crates/xtask/parts.yaml lists and
# for each part alone, on a Rust file that removing the parts leaves unparseable, or a kept line
# or path that names one of them or cites a decision it deletes, removing them in memory.
[doc("Fails on a wrong marker of an optional part, or a removal that leaves broken Rust or a line or path naming the part")]
[group('check')]
check-parts:
    {{ xtask }} check-parts

# On a copy of the repository for each run that crates/xtask/parts.yaml lists, or for the runs
# named, such as `personal`, removes the run's optional parts with `just remove`, then runs `just
# checks` on the copy. Each copy and its build are in target/removable/<run>/.
[doc("Fails unless removing each run's optional parts, such as a workspace shape's, leaves a copy that passes the checks")]
[group('check')]
check-removable *runs:
    {{ xtask }} check-removable {{ runs }}

# On a copy of the repository, writes the modules order_items, then categories, with `just
# new-module`, then runs `just checks` on the copy (ARCH-03). The copy and its build are in
# target/new-module/modules/.
[doc("Fails unless two modules that `just new-module` writes leave a copy that passes the checks")]
[group('check')]
check-new-module:
    {{ xtask }} check-new-module

# Fails on a query of workspace data that names no tenant, or a table that holds no workspace data
# and isn't listed as account or platform data.
[doc("Fails on a query of workspace data that names no tenant, or a table placed nowhere")]
[group('check')]
check-tenancy:
    {{ xtask }} check-tenancy

# Fails on a query that names a table another module owns: the module whose name the table's
# creating migration holds.
[doc("Fails on a query that names a table another module owns")]
[group('check')]
check-table-ownership:
    {{ xtask }} check-table-ownership

# Fails on a migration whose name holds no module, and on one that the newest release holds and the
# working tree changed or deleted. It reads the release from git's tags.
[doc("Fails on a migration misnamed, or changed since a release holds it")]
[group('check')]
check-migrations:
    {{ xtask }} check-migrations

# Fails when the query cache in .sqlx/ differs from what sqlx-cli writes from a database that the
# migrations build: run `just prepare` after changing a query or a migration.
[doc("Fails on a query cache that `just prepare` would change")]
[group('check')]
sqlx-check: bound-build services
    {{ xtask }} sqlx-check
