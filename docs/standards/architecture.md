# Architecture rules (`ARCH`)

The enforceable rules behind [docs/architecture.md](../architecture.md). That document explains the design; this one states what every change must obey.

## Crates

### ARCH-01 `platform` never depends on `app`
The API is built from exactly two crates, `platform` and `app`, and `platform` has no dependency on `app`. The workspace has one other crate, `xtask`, which holds the custom CI checks and the other tasks that `just` runs. Neither API crate depends on `xtask`, so no check ships in the binary.
- Why: shared mechanisms must never learn about features, and tooling never ships in the API (D22).
- Check: `compiler` (Cargo rejects the dependency cycle, and `xtask` is a binary with no library, so no crate can use its code).

### ARCH-02 `platform` holds no product rules
`platform` contains no role definitions, no concrete permissions, no membership rules, and no types belonging to a feature module. It defines the generic `Permission` trait and the `Authorized<P>` and `Memberships` types, whose meaning comes entirely from the `AccessPolicy` that `access` implements. The only tenancy concepts it knows are a user ID and a workspace ID. The list of telemetry names (OBS-05) may name a module's concepts, such as `auth.session.id`, because a name isn't a rule.
- Why: mechanisms live in `platform`, policy lives in modules (architecture §2).
- Check: `review`.

## Modules

### ARCH-03 One directory per module, with fixed file names
Each feature module is a directory under `crates/app/src/modules/`. Its files come only from this set, and only those it needs: `mod.rs`, `routes.rs`, `service.rs`, `repo.rs`, `model.rs`, `error.rs`, `events.rs`, `audit.rs`, `jobs.rs`, `emails.rs`, `metrics.rs`, `personal_data.rs`. The three module-specific entries listed in [architecture.md §5](../architecture.md#inside-a-module) are also allowed: `access/permissions.rs`, `auth/providers/` and `system/seed.rs`. A module's email templates are in a directory named after it under `crates/app/templates/`. A file that outgrows the limit in CODE-08 becomes a directory with the same name.
- Why: every module reads the same way, so a developer who knows one knows all of them (D22, D29).
- Check: `ci: check-layout` (a module's entries, and its templates' directory).

### ARCH-04 A module is used only through its `mod.rs`
Submodules are private (`mod repo;`, never `pub mod repo;`). Other modules use only the items `mod.rs` re-exports.
- Why: a module's internals can change without touching its callers.
- Check: `compiler` (visibility), plus `ci: check-deps` (a module's submodules are private, and no code outside it names one).

### ARCH-05 Module dependencies follow the allowed graph
A module references another module only along an edge in the graph in [architecture.md §5](../architecture.md#dependencies). The graph has no cycles.
- Why: a cycle couples modules permanently, and a fork can't remove one of them (D22, D29).
- Check: `ci: check-deps`.

### ARCH-06 A module reads and writes only its own tables
SQL in a module references only the tables that module owns: those its own migrations create, whose names hold the module's name (DATA-09). `platform` owns the tables of its own migrations, and code of `app` outside its modules runs no SQL. Data owned by another module comes through that module's API, using a batch read when loading a list.
- Why: ownership stays clear, and a module's schema can change without breaking others.
- Check: `ci: check-table-ownership` (each table a query of a `sqlx` macro names, against the module whose name the table's creating migration holds, and no query in `app` outside its modules), plus `review` (a list loads another module's data in a batch read).

### ARCH-07 Optional parts are removable
These parts are optional:
- Each OAuth provider is one file in `auth/providers/` plus one line in that directory's registry.
- The `client_telemetry` module is registered only in `composition.rs`.
- Each part that gives a product its workspace shape ([architecture.md §5](../architecture.md#workspace-shapes)):
  - users creating workspaces through `POST /v1/workspaces`
  - the setup subcommand
  - the `superadmins` module, with its subcommand
  - the `invitations` module <!-- part: invitations -->

An optional part consists of everything that exists only for it: its code and its registration; the items of other modules that only it uses, such as a permission or a function; the dependencies that only it uses; its emails, with their templates; its tables, each created and changed only by migrations of its own; its tests; its section of the module spec, or for a module its whole spec and its row in the spec index; its settings in `.env.example` and in the reference deployment's files; the metrics it declares, with the dashboard panels and alerts that use them; its routes in `openapi.json`; and every sentence, list item or table row of a document that exists only for it. `just remove <part>` deletes all of these, together with every link and ID that points into them, and leaves a codebase that passes `just ci`:
1. It deletes the files that the part owns whole, which `xtask` lists for each part. A module owns its directory, its templates' directory, its test files, its spec, and the migrations whose names hold its name (DATA-09).
2. In every other file, it deletes what the part's markers mark. A marker is a comment that reads `part: <part>`, in the file's own syntax: `// part: <part>` in Rust, `# part: <part>` wherever `#` starts a comment, as in `clippy.toml`, Compose's files, the justfile, shell scripts and the settings files, `<!-- part: <part> -->` in Markdown, and `%% part: <part>` in a Mermaid diagram. A line that holds a marker after other text, as a table row does in its last cell, is deleted. A marker alone on its line opens a block, which the next line that holds only `end part: <part>`, in the same syntax, closes; the block is deleted with both lines. In a Rust file, every marker is Rust's `//` comment, alone on its line once rustfmt has formatted the file. rustfmt joins the short elements of an array or of a call's arguments onto one line, and moves a comment among them onto the element before it, so what a part adds to such a list goes outside it, such as in a statement of its own that extends the list.
3. It deletes each entry of the query cache (DATA-14) whose query no code it kept holds, updates `Cargo.lock` to the dependencies left, writes `openapi.json` and the metric catalog again, writes "None." in each section of a spec that it left empty, as the [spec README](../spec/README.md#sections) asks of a section with nothing in it, and formats the code.
4. It fails while any word of the part, which `xtask` lists with its files, appears in a file it kept or in the path of one, naming each line and path.

Steps 2 and 4, and the Check's reading of markers, leave out the migration workspace (DOC-13), which the migration's close deletes.

So a line that names an optional part either exists only for it, and carries its marker, or is written so that it doesn't name it, with the part's words in a line of their own, such as a sentence of a paragraph on its own source line. A fork removes optional parts before its first release: its history starts fresh (D29), so no release of its own contains the migrations it deletes (DATA-09, DATA-10). Every other module is a core module. No Cargo feature flag selects modules or optional parts.
- Why: a fork that doesn't need a feature removes it by deleting it, and the workspace parts it keeps give its product its shape (D29, D39). The template keeps every part, so the code of the parts most forks keep is compiled, tested and deployed as the template's own (D73).
- Check: `ci: check-parts`, in `just checks`, does in memory, in seconds, the removal of each run: the parts that one workspace shape doesn't keep, one OAuth provider, or `client_telemetry`, and never every part that creates workspaces, since every shape keeps one. It fails on a marker that names no part, a block that no end marker closes, a marker in a Rust file that isn't Rust's `//` comment alone on its line, a test, in a file that a part owns whole, named by another module's behavior, which removing the part would take with it (TEST-04), a Rust file left that doesn't parse, and a line or path left that holds a word of a part removed, outside the files that a removal writes again. `ci: check-removable`, which runs on `main`, does each run's removal with `just remove` on a copy of the repository, then runs `just checks` on the copy, as on a fork before its first release. `review` covers what only the image and the end-to-end tests would show of a copy, and a line that exists only for a part without naming it.

## Layers

### ARCH-08 Handlers only extract, authorize, call and respond
A handler in `routes.rs` extracts its inputs, receives its authorization through its extractor types, calls one service function, and shapes the response. It contains no SQL and no business rules.
- Why: business rules live in exactly one layer.
- Check: `ci: check-layers` (no `sqlx` in a module's routes, no handler calls `check` on the `RateLimits` it extracts, which the service counts, and no handler calls a second method of what its `State` extracts, or a second function of its module's `service`), plus `review`.

### ARCH-09 Services contain no HTTP types
`service.rs` doesn't use `axum`, `http`, or request and response types.
- Why: business rules can be called from jobs and tests without HTTP.
- Check: `ci: check-layers`.

### ARCH-10 Repositories contain only SQL
`repo.rs` runs queries and maps rows to domain types. It makes no decisions beyond translating database outcomes (ERR-05).
- Why: a rule hidden in a query is invisible to readers of the service.
- Check: `review`.

### ARCH-11 One unit of work per operation, and the commit decides the outcome
An operation that spans modules passes one `&mut UnitOfWork` through every call. A service never opens a second transaction for the same operation, but for one read: what decides which rate limit counts the request, read before the unit of work opens, in a short transaction that writes nothing, as a sign-in reads its device token (AUTH-019), and as authentication, before the service runs, reads the session whose user's limit counts the request (PLATFORM-054). Two records are written outside it, each so that what happens to the operation can't undo it: a rate limit's count, which commits before the unit of work opens, so a request refused afterwards still counts (PLATFORM-011), and a refused authorization, which the `SecurityEventSink` records in a short transaction of its own, once the operation's has rolled back, so nothing of the refused operation commits while its refusal does (PLATFORM-010). Once the unit of work commits, the response reports what it recorded. A unit of work that recorded a security failure (PLATFORM-008) answers with that failure, such as 401 for a failed sign-in, and any other commit answers with success. Work after the commit happens only through jobs and events, so it can't turn a committed change into an error.
- Why: the whole operation commits or none of it does, and the response always matches what was committed (D1, D36).
- Check: `review`, plus `test` (PLATFORM-008).

## Composition

### ARCH-12 One composition root
`composition.rs` builds every service once at startup, hands each one its dependencies, registers the four `platform` traits (architecture §5), and mounts every module's routes. Nothing else constructs services. The process's resources aren't services: `main.rs` opens them before the services are built, each that can fail as a step of startup with its own `startup.area` (OPS-03), and hands them to `composition.rs`. They are the signal handlers, the database, the clock, readiness, the listener, and the workers, the turns of CPU-heavy work. Global mutable state is not allowed. The only exceptions are metric handles and other read-only values that are initialized once, and the state the test support keeps for the tests of one process, such as the template database's name, or for the thread of one test, such as the watch of its lines.
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
Every task that runs application logic against the database, such as bootstrap, migrate and seed, is a subcommand of the app binary. A script reaches the database only through Postgres's own tools, for what Postgres itself does: pgBackRest, the reference deployment's backup tool, backs it up and restores it (D44, OPS-15), and a restore asks Postgres, through `pg_isready` and `psql`, whether it runs and how far its replay has come, and commits a transaction that changes nothing ([backups.md](../operations/backups.md#restoring)). No statement a script sends names a table of the product schema (DATA-01). The repository's own tools reach the database too: the CI checks that inspect the schema or the compiled queries, and `just prepare`, which rewrites the query cache from the migrated database (DATA-14).
- Why: application tasks use the same configuration, types and checks as the server.
- Check: `ci: check-shell` (each `psql` of a script under `deploy/` takes its statement with `-c`, and the statement names no table of the product schema), plus `review` (what a script asks of Postgres's tools).

### ARCH-15 The product name is renamable
The product name and its derived forms appear only in the files that `just rename` rewrites: crate and binary names, service names, telemetry service names, and email branding. The image takes the name of the repository that publishes it, so a fork's image has the fork's name.
- Why: a fork becomes its own product with one command (D29).
- Check: `ci: check-rename` (runs `just rename` on a scratch copy, then checks that the old name appears nowhere and that the build and tests pass).
