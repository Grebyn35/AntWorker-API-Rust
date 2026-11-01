# Architecture rules (`ARCH`)

The enforceable rules behind [docs/architecture.md](../architecture.md). That document explains the design; this one states what every change must obey.

## Crates

### ARCH-01 `platform` never depends on `app`
The API is built from exactly two crates, `platform` and `app`, and `platform` has no dependency on `app`. The workspace has one other crate, `xtask`, which holds the custom CI checks, and the tasks that `just` runs to release, deploy and test. Neither API crate depends on `xtask`, so no check ships in the binary.
- Why: shared mechanisms must never learn about features, and tooling never ships in the API (D22).
- Check: `compiler` (Cargo rejects the dependency cycle, and `xtask` is a binary with no library, so no crate can use its code).

### ARCH-02 `platform` holds no product rules
`platform` contains no role definitions, no concrete permissions, no membership rules, and no types belonging to a feature module. It defines the generic `Permission` trait and the `Authorized<P>` and `Memberships` types, whose meaning comes entirely from the `AccessPolicy` that `access` implements. The only tenancy concepts it knows are a user ID and a workspace ID. The list of telemetry names (OBS-05) may name a module's concepts, such as `auth.session.id`, because a name isn't a rule.
- Why: mechanisms live in `platform`, policy lives in modules (architecture §2).
- Check: `review`.

## Modules

### ARCH-03 One directory per module, with fixed file names
Each feature module is a directory under `crates/app/src/modules/`. Its files come only from this set, and only those it needs: `mod.rs`, `routes.rs`, `service.rs`, `repo.rs`, `model.rs`, `error.rs`, `events.rs`, `audit.rs`, `jobs.rs`, `metrics.rs`, `personal_data.rs`. The two module-specific entries listed in [architecture.md §5](../architecture.md#inside-a-module) are also allowed: `access/permissions.rs` and `auth/providers/`. A file that outgrows the limit in CODE-07 becomes a directory with the same name.
- Why: every module reads the same way, so a developer who knows one knows all of them (D22, D29).
- Check: `ci: check-layout`.

### ARCH-04 A module is used only through its `mod.rs`
Submodules are private (`mod repo;`, never `pub mod repo;`). Other modules use only the items `mod.rs` re-exports.
- Why: a module's internals can change without touching its callers.
- Check: `compiler` (visibility), plus `ci: check-deps` (a module's submodules are private, and no code outside it names one).

### ARCH-05 Module dependencies follow the allowed graph
A module references another module only along an edge in the graph in [architecture.md §5](../architecture.md#dependencies). The graph has no cycles.
- Why: a cycle couples modules permanently, and a fork can't remove one of them (D22, D29).
- Check: `ci: check-deps`.

### ARCH-06 A module reads and writes only its own tables
SQL in a module references only the tables that module owns. Data owned by another module comes through that module's API, using a batch read when loading a list.
- Why: ownership stays clear, and a module's schema can change without breaking others.
- Check: `ci: check-table-ownership` (table references in each module's SQL against the ownership list), plus `review`.

### ARCH-07 Optional parts are removable
These parts are optional:
- Each OAuth provider is one file in `auth/providers/` plus one line in that directory's registry.
- The `client_telemetry` module is registered only in `composition.rs`.
- Each part that gives a product its workspace shape ([architecture.md §5](../architecture.md#workspace-shapes)): users creating workspaces through `POST /v1/workspaces`, the setup subcommand, the `superadmins` module with its subcommand, and the `invitations` module.

An optional part consists of everything that exists only for it: its code and its registration; the items of other modules that only it uses, such as a permission, an email or a function; its tables, each created and changed only by migrations of its own; its tests; its section of the module spec, or for a module its whole spec and its row in the spec index; its settings in `.env.example`; the metrics it declares, with the dashboard panels and alerts that use them; and its routes in `openapi.json`. One command removes a named part by deleting all of these, together with every link and ID that points into them, and leaves a codebase that passes `just ci`. A fork removes optional parts before its first release: its history starts fresh (D29), so no release of its own contains the migrations it deletes (DATA-09, DATA-10). Every other module is a core module. No Cargo feature flag selects modules or optional parts.
- Why: a fork that doesn't need a feature removes it by deleting it, and the workspace parts it keeps give its product its shape (D29, D39).
- Check: `ci: check-removable`, which runs on `main`. On a copy of the repository, it runs the command for each OAuth provider, for `client_telemetry`, and for the parts each workspace shape removes, then runs `just ci` on the copy, as on a fork before its first release. No run removes every part that creates workspaces, since every shape keeps one.

## Layers

### ARCH-08 Handlers only extract, authorize, call and respond
A handler in `routes.rs` extracts its inputs, receives its authorization through its extractor types, calls one service function, and shapes the response. It contains no SQL and no business rules.
- Why: business rules live in exactly one layer.
- Check: `ci: check-layers` (no `sqlx` in `routes.rs`), plus `review`.

### ARCH-09 Services contain no HTTP types
`service.rs` doesn't use `axum`, `http`, or request and response types.
- Why: business rules can be called from jobs and tests without HTTP.
- Check: `ci: check-layers`.

### ARCH-10 Repositories contain only SQL
`repo.rs` runs queries and maps rows to domain types. It makes no decisions beyond translating database outcomes (ERR-05).
- Why: a rule hidden in a query is invisible to readers of the service.
- Check: `review`.

### ARCH-11 One unit of work per operation, and the commit decides the outcome
An operation that spans modules passes one `&mut UnitOfWork` through every call. A service never opens a second transaction for the same operation. Once the unit of work commits, the response reports what it recorded. A unit of work that recorded a security failure (PLATFORM-006) answers with that failure, such as 401 for a failed sign-in, and any other commit answers with success. Work after the commit happens only through jobs and events, so it can't turn a committed change into an error.
- Why: the whole operation commits or none of it does, and the response always matches what was committed (D1, D36).
- Check: `review`, plus `test` (PLATFORM-006).

## Composition

### ARCH-12 One composition root
`composition.rs` builds every service once at startup, hands each one its dependencies, registers the four `platform` traits (architecture §5), and mounts every module's routes. Nothing else constructs services. The process's resources aren't services: `main.rs` opens them before the services are built, each as a step of startup with its own `startup.area` (OPS-03), and hands them to `composition.rs`. They are the signal handlers, the database, the clock, readiness and the listener. Global mutable state is not allowed. The only exceptions are metric handles and other read-only values that are initialized once, and the state the test support shares between the tests of one process, such as the template database's name.
- Why: every dependency is visible in one file, and tests build the same graph.
- Check: `review`.

### ARCH-13 Dependency traits only for inversion or multiple implementations
A trait that abstracts a dependency exists for one of two reasons:
- It inverts a dependency the crate or module graph forbids: `SessionVerifier`, `AccessPolicy`, `SecurityEventSink` and the personal data contract.
- It covers an external system with more than one implementation: object storage, email transport and the clock.

No trait exists just so something can be mocked. Traits that define a behavior many types share, such as `ErrorCode`, `Permission` and job handlers, aren't dependency abstractions, and this rule doesn't limit them.
- Why: an interface with no second implementation and no inversion adds indirection without flexibility. Tests use the real database (D23).
- Check: `review`.

### ARCH-14 Application tasks are binary subcommands
Every task that runs application logic against the database, such as bootstrap, migrate and seed, is a subcommand of the app binary. Backup and restore use pgBackRest, the reference deployment's backup tool (D44): the deployment's `backup` service takes the scheduled backups, a deploy prepares the bucket and takes the first full backup, and `just backup` and `just restore` run it on demand (OPS-17); a restore also uses `pg_isready`, to find whether Postgres runs before pgBackRest's check, and `psql`, to find whether Postgres is still replaying the log before that check, to commit a transaction that changes nothing before it stops Postgres, to ask Postgres how far its replay has come and when its recovery ends, and to ask the time of the last commit it replayed. CI checks that inspect the schema or the compiled queries also connect to the database, and so does `just prepare`, which rewrites the query cache from the migrated database (DATA-20). No other script does.
- Why: application tasks use the same configuration, types and checks as the server.
- Check: `review`.

### ARCH-15 The product name is renamable
The product name and its derived forms appear only in the files that `just rename` rewrites: crate and binary names, service names, telemetry service names, and email branding. The image takes the name of the repository that publishes it, so a fork's image has the fork's name.
- Why: a fork becomes its own product with one command (D29).
- Check: `ci: check-rename` (runs `just rename` on a scratch copy, then checks that the old name appears nowhere and that the build and tests pass).
