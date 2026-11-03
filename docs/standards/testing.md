# Testing rules (`TEST`)

A test exists to prove a behavior from a module spec or a rule from these standards. Coverage is measured as the map from those IDs to tests, not as a percentage of lines (D23).

## What is tested

### TEST-01 Every behavior has a test, and every test has an ID
Every behavior in `docs/spec/` has at least one test whose name starts with the behavior's ID, for example `workspaces_006_the_owner_cannot_be_removed`. A test that proves a rule starts with the rule ID instead, for example `data_19_the_app_role_reads_and_writes_rows_and_changes_nothing_else`. Every test name starts with an ID that exists. The tests named by a behavior's ID together assert every clause of its statement. A rule's tests prove what its `Check` line gives to `test`.
- Why: the map from spec to tests is checked by a machine, not by a reviewer's sense of what is missing (D23).
- Check: `ci: check-test-map` (every test's ID exists, and every behavior has a test, with the migration's exception in DOC-10), plus `review` (the tests cover every clause).

### TEST-03 Security behaviors are tested from both sides
Each permission is tested twice: a role that has it succeeds, and a role without it is refused. Each pre-authentication path is tested for the absence of enumeration, except the paths where SEC-05 accepts it.
- Why: authorization bugs hide on the refusal side.
- Check: `test` (TEST-09 generates part of this).

## Levels

### TEST-04 Three levels, each with one job
| Level | Where | What it covers |
|---|---|---|
| Unit | `#[cfg(test)]` in the module | Pure logic: domain types, the permission matrix, cursors, parsing |
| Integration | `crates/app/tests/` | Behaviors through HTTP, against real Postgres and S3-compatible storage. A behavior with no HTTP surface, such as a subcommand or a startup check, is tested by running the binary, or through `platform`'s API |
| End to end | `crates/app/tests/e2e/` | Cross-cutting guarantees against the built image: the image itself, trace IDs, deploys, alerts, backups |

- Why: every behavior is proven at the cheapest level that proves it for real (D23).
- Check: `review`.

### TEST-05 No mocks of our own code
Tests use the real database, the real routes and the real services. Fakes exist only for systems outside the codebase: the mail server, reached over the Simple Mail Transfer Protocol (SMTP), which Mailpit, the mail catcher that `just services` starts, or a scripted mail server of a test's own replaces; OAuth providers (a local fake server, reached through the configured client in SEC-09), the clock (a test clock), the kernel's report of the process's resident memory, which `platform::telemetry::testing::watch_memory` takes from the test, the OpenTelemetry Collector, which a stand-in receiver of the OpenTelemetry Protocol (OTLP) replaces, as do the in-memory logger and meter of `platform::telemetry::testing`, the reference deployment's backup bucket, which SeaweedFS, serving the S3 protocol on the end-to-end tests' server, replaces, a managed observability backend, which a collector there replaces, receiving OTLP over HTTPS from one user, and its Grafana, which a Grafana there replaces, with the backend's data sources, an alert integration, which a collector there replaces, receiving Alertmanager's webhooks, the process environment and file reads, which tests hand to `platform::config::Settings` directly, and the operator who answers `just server-setup`'s questions, whom a script of answers replaces. Routes that only the tests serve, registered through the builders of API-14, show three kinds of behavior: the pipeline's that no route of the API shows, such as a panic or a request that never ends; those of `platform`'s mechanisms that only an optional part's routes show, such as an idempotency key's, so that removing the part leaves `platform`'s tests passing (ARCH-07); and those of a mechanism that the API's routes show, but not when a test asks, such as a unit of work that publishes many events at once, which the announcing routes do, or an event that a permission no role lacks would gate.
- Why: a mock tests your assumptions about the code, not the code (ARCH-13).
- Check: `review`.

### TEST-06 Every integration test gets its own database
Each integration test that uses the database runs on a fresh database of its own: one cloned from a migrated template, or, for tests of `migrate` and of `serve`'s schema check, one with no migrations. A test of a route that never reaches the database, such as the liveness probe, needs none. Migrations are applied by the production migrator. The application under test connects as the `app` role, with exactly its production privileges.
- Tests arrange state through an `owner` connection, or through the administrator's for what no role of the API may do, such as creating a role.
- Assertions on stored state, such as enqueued jobs and audit entries, read through a separate `owner` connection. Assertions on the database's catalogs, such as role attributes, may read through `app`. State only an administrator may read, such as password verifiers, is read through the administrator's connection.
- Why: tests are isolated, fast, and faithful to production (D23).
- Check: `test` (the harness asserts the role it connects as).

## How tests are written

### TEST-07 Tests are deterministic
Tests never sleep, and act at states they observe, never after a time they guess. Time in application code comes from the test clock, and a wait in the code under test completes when the test advances that clock. A timeout inside a dependency, such as the request timeout, runs on tokio's time, which a test that talks to no other system pauses and lets the runtime advance. Expiry, which runs in SQL against `now()`, is tested by arranging rows whose timestamps already lie in the past, written through the `owner` connection. A test waits for a state in one of three ways:
- An integration test waits for a state that the code under test, or Postgres, reaches on its own, such as a gauge's value, a log line or a query waiting on a lock, by polling it, yielding to the runtime between polls, with no bound of its own: what the test did leads to that state, so a wait that never ends is a failing test, which the test runner names once it has run for 60 seconds, and which CI's job time limit ends. A count of polls bounds no time, so it would fail a correct run on a slow machine, and no test uses one.
- An end-to-end test that waits until another system reaches a state asks that system from inside the test environment, such as a shell's loop in a container, under a timeout, and a wait that times out fails the test. A test that reads a command's output until a line, such as `deploy.sh`'s, needs no timeout of its own: each of the command's waits has one.
- A test may act inside a window that the code under test holds open for a configured time, such as the replicas' shutdown delay that a restore waits out (PLATFORM-038), once the code's output says the window has opened, when the action takes a small part of the window: on a machine that runs the code as it should, the action never comes after the window closes.

Tests make no network calls outside the test environment. A test that fails intermittently counts as failing, and CI never retries a failed test.
- Why: a flaky test trains everyone to ignore failures, and a wait bounded by a guess fails a correct run on a slower machine.
- Check: `clippy` (`disallowed_methods` covers sleeps and timed waits in tests), plus `review`.

### TEST-08 Tests assert outcomes, not internals
Tests assert what a client or operator can observe: HTTP responses, database rows, emitted events, enqueued jobs, sent emails and audit entries. A test is laid out as arrange, act, assert, and proves one behavior.
- Why: tests survive refactoring, and a failure points at one behavior.
- Check: `review`.

### TEST-09 The permission matrix is tested exhaustively
A generated test runs every workspace route against every role, plus a non-member, and asserts success, 403 or 404 according to the matrix in `access`. For every route that addresses a row by ID, it also sends the ID of a row in another workspace the caller belongs to, and asserts 404 (DATA-22).
- Why: a change to the matrix or to a route's permission is proven everywhere at once (D2).
- Check: `test`.

### TEST-10 Shared test support, no copy-paste
Test data comes from builders in one test-support module, such as a user, a workspace with members, a session, or an event committed as a unit of work commits it. Tests don't build fixtures inline when a builder exists. A test builds inline only what no builder arranges: a row that the code never writes, which a test arranges to show what the code does when it meets one, such as a row that breaks a constraint the builder keeps, a job of a kind that no release runs, or a second copy of a job, as a second delivery runs it; a row inside a transaction the test holds open; rows whose timestamps lie in the past, for expiry (TEST-07); and the rows of an end-to-end test's own server.
- Why: changing a fixture is a one-place change.
- Check: `review`.

### TEST-11 Local and CI runs are identical
`just test` runs the same suites, against the same services, locally and in CI.
- Why: "works on my machine" means it works in CI (D21).
- Check: `ci: test`, which is `just test`.
