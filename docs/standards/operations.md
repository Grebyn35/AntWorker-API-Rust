# Operations rules (`OPS`)

Configuration, background jobs, runtime behavior, CI, releases, deploys, backups and local development.

## Configuration

### OPS-01 Configuration is strict and typed
`platform::config` loads every setting once at startup into typed values:
- Values are trimmed, and an empty value counts as unset.
- A setting can be read from a file named by `<NAME>_FILE` (SEC-01).
- Numbers are range-checked, and unknown values for a closed set are errors.
- Required settings have no defaults. `ENVIRONMENT` is required.
- Why: a misconfiguration fails at startup, never in the middle of a request (D9).
- Check: `test`.

### OPS-02 Every setting earns its place and is documented
A setting exists only if two deployments would set it differently, such as a URL, a credential, a capacity or a feature switch. A value every deployment would share is a constant in code. Every setting appears in `.env.example` with its type, default and meaning. Compose passes each container only the settings it reads.
- Why: a small configuration surface is one a fork can understand, and no container holds settings it doesn't use (D9, D15).
- Check: `ci: check-config` (the settings the API reads must match `.env.example` both ways, and the API container's Compose environment must match them), plus `review` for the third-party containers and for the "would two deployments differ" question.

### OPS-03 Startup failures have one format
A startup failure is logged as one ERROR event, with the message `startup failed` and the fields `area` and `error`, through the configured logging setup (OBS-01). The process then exits with code 1. A failure before the logging setup exists, such as invalid configuration, or a failure of the logging setup itself, is written by `main` to stderr as one JSON line with the same message and fields. That is the one place where `print_stderr` is allowed (CODE-03). Nothing panics.
- Why: operators and scripts handle one shape (D9).
- Check: `test`.

### OPS-04 Disabled features say so
A route whose feature is turned off answers the 503 problem with code `request.feature_disabled`. The capabilities endpoint lists which features are on.
- Why: clients can tell "off" apart from "broken" (D9).
- Check: `test`.

## Runtime

### OPS-05 Health means something
- `GET /health/live` answers 200 while the process runs.
- `GET /health/ready` answers 200 only while the database is reachable, the listener is connected and the instance isn't draining.

The probes are answered before the rate limit, load shedding and request timeout, and they carry no trace context (OBS-15). Their body is `{"status": "<status>"}`, never a problem response (API-07).
- Why: the load balancer only routes to replicas that can serve (D9, D20).
- Check: `test`.

### OPS-06 Shutdown is bounded and graceful
On SIGTERM, the instance shuts down in this order:
1. Turn readiness off.
2. Wait one health-check interval, so the load balancer stops sending traffic.
3. Stop accepting connections.
4. Drain requests, up to a deadline.
5. Cancel jobs and release their leases.
6. Flush telemetry.

The whole sequence fits inside the container's grace period, with margin.
- Why: deploys drop no requests, and SIGKILL never cuts a flush short (D20).
- Check: `ci: e2e` (the deploy scenario).

### OPS-07 Overload sheds load
Every request except the health probes has a timeout. Each route group has a default, and a module spec may override it. A concurrency limit rejects excess requests with the 503 problem and `Retry-After`. The health probes are exempt from it.
- Why: an overloaded instance degrades predictably, instead of queuing until it falls over (D20).
- Check: `test`.

### OPS-08 Postgres is the only shared state
Replicas share nothing except Postgres and object storage. A request can be served by any replica, with no sticky sessions.
- Why: scaling out means adding replicas (D20).
- Check: `review`.

## Background jobs

### OPS-09 Jobs are declared once and are idempotent
Each job is declared in its module's `jobs.rs`, or in `platform::jobs` for platform jobs such as the dispatcher, and is registered in the job registry. It states its owner, its schedule or trigger, and the tables it touches. Running a job handler twice has the same effect as running it once.
- Why: jobs are delivered at least once (D1, D13).
- Check: `test` (each job's test runs its handler twice).

### OPS-10 Failed jobs retry, then go to a dead letter
A failing job retries with exponential backoff and jitter, up to its maximum attempts. It then moves to a dead-letter state, which is counted in metrics and has an alert (OBS-14). A job's payload is deleted when the job succeeds. A dead-lettered job keeps its payload for a configured retention period, so it can be investigated, and then loses it.
- Why: transient failures heal, and permanent ones are seen (D13).
- Check: `test`.

### OPS-11 Periodic jobs run once per interval across the fleet
A periodic job claims its schedule row with `FOR UPDATE SKIP LOCKED`, and extends its lease with a heartbeat while it runs.
- Why: N replicas don't mean N runs (D13).
- Check: `test`.

## Build, CI and release

### OPS-12 CI calls `just`
Every CI step runs a `just` task. The workflows only orchestrate: one for pull requests and `main`, one for release tags, and one for scheduled jobs. The checks required for merging are listed in `docs/operations/ci.md` and applied as branch protection by `just protect-branch`.
- Why: local and CI runs are identical, and merging requires exactly the documented checks (D21).
- Check: `review`, plus `ci: check-protection` (a scheduled job that compares the repository's branch protection with the documented list).

### OPS-13 Everything is pinned
The toolchain comes from `rust-toolchain.toml`. Actions are pinned by commit SHA, images by digest, and the runner by version.
- Why: a build today is reproducible tomorrow (D21).
- Check: `ci: check-pins`.

### OPS-14 Build once, then promote that digest
Each pipeline run builds the image once, for both architectures. Every later step tests, scans, boots and promotes that same digest. A release tag `vX.Y.Z` retags the digest, signs it and attaches its SBOM and provenance. A prerelease tag `vX.Y.Z-rc.N` publishes a prerelease image only.
- Why: what ships is exactly what was tested (D15, D21).
- Check: `review` (workflow structure), plus `ci: verify-release` (the digest is signed and matches the tested one).

### OPS-15 The image is minimal and unprivileged
One image serves every subcommand. It runs as a non-root user with a read-only root filesystem, and has a `HEALTHCHECK`.
- Why: a small attack surface, and the same artifact everywhere.
- Check: `ci: image-scan`, plus `ci: e2e`.

## Deploy and recovery

### OPS-16 Deploys roll without downtime and roll back on failure
A deploy starts new replicas, waits until they are ready, shifts traffic to them, then stops the old replicas. If a new replica doesn't become ready, the deploy rolls back. CI runs a deploy against the real image.
- Why: deploying is routine, not a risk (D15).
- Check: `ci: e2e`.

### OPS-17 Backups are scheduled and restores are tested
The bundled Postgres is backed up on a schedule to object storage, with retention. `just backup` takes a backup on demand, and `just restore` restores one into a fresh database. CI restores a backup and runs the smoke tests against it. `docs/operations/backups.md` states the recovery targets.
- Why: an untested backup is not a backup (D30).
- Check: `ci: e2e` (the restore scenario).

### OPS-18 Capacity is measured
A load-test task records requests per second per replica at the latency objective. It runs before each release, and its result is part of the release notes.
- Why: capacity is a number, not a guess (D20).
- Check: `review` (the release checklist in `docs/operations/release.md`).

## Local development

### OPS-19 Two commands from clone to running
`just setup` checks the required tools and writes `.env` with generated development secrets. `just dev` starts Postgres, SeaweedFS and Mailpit, then runs the API with auto-reload. Both work on Windows, macOS and Linux, with Docker available. Development secrets are never committed.
- Why: a fork is productive within minutes (D28).
- Check: `ci: setup-smoke`. On Linux it runs `just setup` and `just dev` end to end. On Windows and macOS, where hosted runners can't run Linux containers, it runs `just setup` and builds the workspace.
