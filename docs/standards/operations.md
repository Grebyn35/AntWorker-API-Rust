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
A setting exists only if two deployments would set it differently, such as a URL, a credential, a capacity or a feature switch. A value every deployment would share is a constant in code. Every setting of the API appears in `.env.example`, and every setting of the reference deployment in `deploy/settings.env.example`, each with its type, default and meaning. Compose passes each container only the settings it reads.
- Why: a small configuration surface is one a fork can understand, and no container holds settings it doesn't use (D9, D15).
- Check: `ci: check-config` (the settings the API reads must match `.env.example` both ways, and the API container's Compose environment must match them), plus `review` for the third-party containers and for the "would two deployments differ" question.

### OPS-03 Startup failures have one format
A failure that ends a subcommand of the API binary, at startup or, for a one-shot subcommand such as `migrate`, in its work, is logged as one `ERROR` event, with the message `startup failed` and the fields `startup.area` and `exception.message`, through the configured logging setup (OBS-01, OBS-05). The process then exits with code 1. A failure before the logging setup exists, such as invalid configuration, or a failure of the logging setup itself, is written by `main` to stderr as one JSON line with the same message and fields. That is the one place in the API's crates where `print_stderr` is allowed (CODE-03). Nothing panics.
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

While no replica is ready, such as during a restore, the reference deployment's proxy answers each request within two seconds with the 503 problem `request.unavailable` and `Retry-After`, whose trace ID leads to the proxy's span and log line of the request (D10, D53), and a probe with a bare 503. Both carry the headers that the API sets on every response (SEC-11), and the problem its trace context in `server-timing` too (API-16).
- Why: the load balancer only routes to replicas that can serve, and a client gets the API's answer even when none can (D9, D20, D53).
- Check: `test` (PLATFORM-019, PLATFORM-037), plus `ci: e2e` (the proxy's answer while no replica runs), plus `review` (a probe never runs long enough to show that no timeout applies to it).

### OPS-06 Shutdown is bounded and graceful
The instance installs its handlers for the shutdown signals before it connects to anything, and fails startup if it can't (OPS-03). On SIGTERM, the instance shuts down in this order:
1. Turn readiness off.
2. Wait one health-check interval, so the load balancer stops sending traffic.
3. Stop accepting connections.
4. Drain requests, up to a deadline.
5. Cancel jobs and release their leases.
6. Flush telemetry.

The whole sequence fits inside the container's grace period, with margin.
- Why: deploys drop no requests, and SIGKILL never cuts a flush short (D20). An instance that can't receive SIGTERM is killed mid-request by every deploy.
- Check: `ci: e2e` (the deploy scenario), `test` (the sequence through `platform::lifecycle::serve`), plus `review` (the handlers are installed before anything connects, because no test can make installing them fail).

### OPS-07 Overload sheds load
Every request that a route serves, except the health probes, has a timeout. Each route group has a default, and a module spec may override it. A request that no route serves gets its 404 or 405 problem at once. A concurrency limit rejects excess requests with the 503 problem and `Retry-After`. The health probes are exempt from it.
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
Every CI step that builds, checks, publishes or releases runs a `just` task. The steps that prepare the runner, such as checking out the repository, installing tools, restoring a cache, signing in to the registry and installing QEMU, are the workflow's own, because `just` isn't installed, or the runner isn't ready for a task, until they run. So is the registry's upkeep, which a pinned action does. The workflows only orchestrate: `ci.yml` for pull requests and `main`, whose runs on `main` also publish and release, and `scheduled.yml` for scheduled jobs. The checks required for merging are listed in `docs/operations/ci.md`, which says how to require them, and why requiring them is the repository owner's choice.
- Why: local and CI runs are identical, and a repository that gates merging requires exactly the documented checks (D21).
- Check: `review`.

### OPS-13 Everything is pinned
The toolchain comes from `rust-toolchain.toml`, in CI and in the image's build. Actions are pinned by commit SHA, images by digest, downloads by checksum, the crates the image's build installs by exact version with `--locked`, and the runner by version. The images the tasks run are pinned in `compose.yaml`, beside the services' images, or in the reference deployment's Compose files that `compose.yaml` extends, which name the ones a server runs too. The other tools the tasks use, such as cargo-deny and watchexec, are pinned by version in the justfile: `just setup` checks them, and CI installs them from it. `just` itself is the exception: CI installs it before any task can run, so the workflows pin its version too. Dependabot proposes updates to the pins it can read, and `docs/operations/ci.md` lists the ones updated by hand.
- Why: a build today is reproducible tomorrow, and a pin moves only in a change that CI checks (D21).
- Check: `ci: check-pins`, plus `review` (an image or a download that a recipe or a workflow's `run:` step names, which `check-pins` doesn't read, and the list in `docs/operations/ci.md` of the pins updated by hand).

### OPS-14 Build once, then promote that digest
Each pipeline run builds each image once, for linux/amd64 and linux/arm64, with its software bill of materials (SBOM) and its provenance attached: the API's image, and the reference deployment's Postgres (D44). Every later step scans, boots, tests and publishes those same digests: on `main`, as `sha-<commit>`, and as `edge` while the commit is the head of `main`. `just release` asks for a release: it sets the app's version, refusing one that doesn't come after every release, and commits it. The first run on `main` that then passes with that version releases it, unless a release has it already: it gives each digest the version's tag, `X.Y.Z`, or `X.Y.Z-rc.N` for a prerelease, signs it, and only then tags the commit `v<version>`, such as `v1.4.0` or `v1.4.0-rc.1`. A version that no `just release` asked for releases nothing. Nothing is rebuilt.
- Why: what ships is exactly what was tested, and a release is always a commit that passed every check (D15, D21).
- Check: `review` (the workflows' structure), `ci: e2e` (each platform's image boots), `ci: verify-release` (each image's version tag names the tested digest, signed by the run on `main`, with the SBOM and provenance of both platforms), plus `test` (what `just release` refuses).

### OPS-15 The image is minimal and unprivileged
One image serves every subcommand. It holds the API's static binary on a base with no shell, no package manager and no C library, which provides the operating system's certificate store and time zone data. It runs as a non-root user with a read-only root filesystem, and has a `HEALTHCHECK`, which runs the `health` subcommand (PLATFORM-044).
- Why: a small attack surface, and the same artifact everywhere.
- Check: `ci: image-scan` (no vulnerability that has a fix and is rated high or critical), plus `ci: e2e` (bootstrap, migrate and serve run from the image as its user on a read-only root, and the health check, which runs `health`, reports the serving instance healthy).

## Deploy and recovery

### OPS-16 Deploys roll without downtime and roll back on failure
A deploy takes a release by its version, and refuses a version that no release has. It applies the release's migrations, starts new replicas, waits until the load balancer finds every one of them ready, then stops the old replicas, which drain (OPS-06). If a new replica doesn't become ready, the deploy removes the new replicas, and the old ones go on serving. A deploy that refuses, or ends before its replicas serve in place of the old ones, puts back the deployment's files that it replaced. When it can't, because it was stopped by force or the copy failed, which it reports, the next deploy, backup or restore puts them back before anything else (D50). The proxy and the bundled database, which the API's requests pass through and which each run as one instance, are restarted by a deploy only when the operator allows it, because restarting either interrupts requests. A service of the observability stack restarts, or reloads, when a deploy changes its configuration, which interrupts no request of the API. A deploy runs only the services that its settings choose (D42), and stops those an earlier choice started once no replica uses them. One deploy, backup or restore runs at a time, and its lock goes when its processes end, however they end. The alert rules are the release's files, which a deploy installs as they are and the services read again, and which no deploy generates or rewrites.
- Why: deploying is routine, not a risk, and an alert means the same whatever deployed the system (D15).
- Check: `ci: e2e` (a deploy of the real images under continuous requests, none of which fails; the rollback, with the files it puts back; the files that the next task puts back after a deploy killed before its replicas serve, and after a put-back that failed; the files a deploy keeps when it ends after its replicas serve; the restart only with consent; the services a change of settings stops; the rules a deploy installs, also after a refused one; and the refusal while another task runs), plus `test` (what `just deploy` refuses, and that the files it sends keep the line endings the repository holds).

### OPS-17 Backups are continuous and restores are tested
The bundled Postgres archives its write-ahead log continuously, and takes base backups on a schedule, to an S3-compatible bucket off its server, encrypted before they leave it, with retention. A restore recovers the database into a fresh one, as it stood at any moment the retention covers. `just backup` takes a base backup on demand, and `just restore` restores the latest state, or the state at a given moment. CI restores to a moment, and checks that the restored database holds what was written before the moment and not what came after, and that the API serves through the proxy; it also restores onto a new server, and after a deploy that failed. `docs/operations/backups.md` states the recovery targets, and requires a managed Postgres to provide point-in-time recovery of its own.
- Why: losing the server loses at most about a minute of data, a mistake can be undone up to the moment before it, and an untested backup is not a backup (D30, D44).
- Check: `ci: e2e` (the restore scenario).

### OPS-18 Capacity is measured
`just load-test` runs one replica of the image, with the reference deployment's CPU and memory limits for a replica, and finds the highest constant rate of requests it serves with no request failed and the 99th percentile within the latency objective. That rate is the replica's capacity. The task runs before each release, and its result is part of the release notes.
- Why: capacity is a number, not a guess (D20).
- Check: `review` (the release checklist in `docs/operations/release.md`).

### OPS-20 Data is encrypted in transit and at rest
Every connection that leaves the server the API runs on uses TLS, and verifies the certificate of the service it reaches. Clients reach the API over HTTPS only. A managed database is reached with `sslmode=verify-full`, and the backup bucket and a managed observability backend over HTTPS. Stored data is encrypted below the API: the deployment's volumes are on encrypted disks, a managed database and a bucket encrypt what they store, and backups are encrypted before they leave the server. The API itself encrypts only secrets (SEC-02).
- Why: TLS and storage-level encryption protect personal data in transit and at rest, without application-level encryption of every column (D14).
- Check: `ci: e2e` (the proxy answers HTTP with a redirect to HTTPS, and HTTPS with HTTP Strict Transport Security (HSTS); and the Postgres image holds the certificate authorities that a bucket's certificate is checked against by default), plus `review` (what `docs/operations/deploy.md` requires of a server and its providers).

### OPS-21 The connection budget fits
An instance opens at most `DATABASE_MAX_CONNECTIONS` pooled connections, plus one of its own for readiness and the listener, and `serve` logs that number once it connects. A deploy refuses to start replicas when every replica it would run at once, at that number each, together with the connections of the one-shot subcommands and of the backups, would exceed the connections Postgres allows.
- Why: replicas can't exhaust the database's connections, even while a deploy runs two sets of them (D20).
- Check: `test` (PLATFORM-045), plus `ci: e2e` (the refusal).

### OPS-22 The server is operated from the developer's machine
Every task on the reference deployment's server runs from a developer's machine, through `just` over Secure Shell (SSH), and none needs a shell on the server. The settings and the secrets stay on the server, in one place. `just server-setup` installs Docker, prepares the user the deployment runs as, logs it in to the registry, and writes the settings and the secrets the operator holds, the credentials of the alert integrations among them, asking for each one that the settings' choices need, and generates the backups' passphrase and the bundled database's superuser password, which it shows once, unless the operator gives a lost server's; no deploy writes them. `just server-check` checks what a deploy needs, and changes nothing of the deployment. `just status` shows which color serves, each of the deployment's containers with its state and its image's version, when the backups last succeeded, and the server's free disk and memory, and changes nothing. `just logs` shows the services' last log lines, and follows them on request.
- Why: one place to operate a deployment from, and a server set up and checked the same way every time, also by a developer who has never set one up (D51).
- Check: `ci: e2e` (the status and the logs after a deploy; the setup of a new server, its check, and a deploy on it; and what the check finds), plus `test` (the questions the setup asks, what it sends the server, and what the check finds of the addresses and the ports from outside), plus `review` (Docker's installation, which the tests' servers already have: a change to it runs on a new Debian or Ubuntu server before it merges).

## Local development

### OPS-19 Two commands from clone to running
`just setup` checks the required tools and the versions the justfile pins, and writes `.env` with generated development secrets. `just dev` starts Postgres and the local stand-ins for object storage and email, then runs the API with auto-reload, and `just dev observe` also starts the observability stack and exports the API's telemetry to it. Both work on Windows, macOS and Linux, with Docker available. Development secrets are never committed.
- Why: a fork is productive within minutes (D28).
- Check: `ci: setup-smoke`. On Linux it runs `just setup` and `just dev` end to end. On Windows and macOS, where hosted runners can't run Linux containers, it runs `just setup` and builds the workspace.
