# Architecture rules (`ARCH`)

The enforceable rules behind [docs/architecture.md](../architecture.md). That document explains the design; this one states what every change must obey.

## Crates

### ARCH-01 `platform` never depends on `app`
The API is built from exactly two crates, `platform` and `app`, and `platform` has no dependency on `app`. The workspace has one other crate, `xtask`, which holds the custom CI checks and the other tasks that `just` runs. Neither API crate depends on `xtask`, so no check ships in the binary.
- Why: shared mechanisms must never learn about features, and tooling never ships in the API (D22).
- Check: `compiler` (Cargo rejects the dependency cycle, and `xtask` is a binary with no library, so no crate can use its code).

### ARCH-02 `platform` holds no product rules
`platform` contains no role definitions, no concrete permissions, no membership rules, and no types belonging to a feature module. It defines the generic `Permission` trait and the `Authorized<P>` and `Memberships` types, whose meaning comes entirely from the `AccessPolicy` that `access` implements.
It defines the `Superadmin` proof the same way, whose meaning comes entirely from the `SuperadminPolicy` that `superadmins` implements. <!-- part: superadmins -->
The only tenancy concepts it knows are a user ID and a workspace ID. The list of telemetry names (OBS-05) may name a module's concepts, such as `auth.session.id`, because a name isn't a rule.
- Why: mechanisms live in `platform`, policy lives in modules (architecture §2).
- Check: `review`.

## Modules

### ARCH-03 One directory per module, with fixed file names
Each feature module is a directory under `crates/app/src/modules/`. Its files come only from this set, and only those it needs: `mod.rs`, `routes.rs`, `service.rs`, `repo.rs`, `model.rs`, `error.rs`, `events.rs`, `audit.rs`, `jobs.rs`, `emails.rs`, `metrics.rs`, `personal_data.rs`. The three module-specific entries listed in [architecture.md §5](../architecture.md#inside-a-module) are also allowed: `access/permissions.rs`, `auth/providers/` and `system/seed/`. A module's email templates are in a directory named after it under `crates/app/templates/`. A file that outgrows the limit in CODE-08 becomes a directory with the same name. One may become a directory before that, where what it holds reads better apart, and one does when a part of the template adds routes of its own to the module, so the part owns their handlers, and the service functions and queries that only those handlers call, whole in files of their own (D119).
What else a part adds among the module's own carries its markers instead: a field, a variant, a conversion, a function that only the part's files call, with what that function alone calls, and a step of one of the module's own operations, as the registration of a job and a source of an export are. <!-- part: removal -->

`just new-module <module> [resource]` writes a module in this layout that holds one workspace resource, with its migration, permissions, spec and tests, from the templates in `crates/xtask/templates/new-module/`, registers it in the files every module shares, and leaves a codebase that passes `just checks` ([guide](../guides/adding-a-module.md)).
- Why: every module reads the same way, so a developer who knows one knows all of them, and a new one starts from code that already follows the rules (D22, D29).
- Check: `ci: check-layout` (a module's entries, and its templates' directory), `ci: check-deps` (each path into another module that a template of the module's directory names, an item that module's `mod.rs` re-exports), plus `ci: check-new-module` (two modules that `just new-module` writes on a copy of the repository, one after the other, then `just checks` there).

### ARCH-04 A module is used only through its `mod.rs`
Submodules are private (`mod repo;`, never `pub mod repo;`). Other modules use only the items `mod.rs` re-exports, and a type of the module that the signature of a function it re-exports names is one of them: `mod.rs` re-exports it with the function.
- Why: a module's internals can change without touching its callers.
- Check: `compiler` (visibility), `ci: check-deps` (a module's submodules are private, no code outside it names one, and `mod.rs` re-exports each type that a file of the module declares at its top level and that the signature of a function it re-exports names), plus `review` (a type of the module that other code reaches only through a field or a method of an item that `mod.rs` re-exports).

### ARCH-05 Module dependencies follow the allowed graph
A module references another module only along an edge in the graph in [architecture.md §5](../architecture.md#dependencies). The graph has no cycles.
- Why: a cycle couples modules permanently, and a fork can't remove one of them (D22, D29).
- Check: `ci: check-deps`.

### ARCH-06 A module reads and writes only its own tables
SQL in a module references only the tables that module owns: those its own migrations create, whose names hold the module's name (DATA-09). `platform` owns the tables of its own migrations, and code of `app` outside its modules runs no SQL. Data owned by another module comes through that module's API, using a batch read when loading a list.
- Why: ownership stays clear, and a module's schema can change without breaking others.
- Check: `ci: check-table-ownership` (each table a query of a `sqlx` macro names, against the module whose name the table's creating migration holds, and no query in `app` outside its modules), plus `review` (a list loads another module's data in a batch read).

<!-- part: removal -->
### ARCH-07 Optional parts are removable
These parts are optional:
- Each OAuth provider is one file in `auth/providers/` plus its lines in that directory's registry.
- The `client_telemetry` module, with each user's consent to the telemetry of their clients, the telemetry proxy, and the collector's pipelines of what the proxy forwards. <!-- part: client_telemetry -->
- The `files` module, with the avatars and workspace pictures, whose images it keeps. <!-- part: files -->
- Each part that gives a product its workspace shape ([architecture.md §5](../architecture.md#workspace-shapes)):
  - users creating workspaces through `POST /v1/workspaces` <!-- part: workspace_creation -->
  - the setup subcommand, `setup-workspace` <!-- part: workspace_setup -->
  - the `superadmins` module, with its subcommand, and a superadmin's routes <!-- part: superadmins -->
  - the `invitations` module <!-- part: invitations -->

One more part, `removal`, holds what makes the others removable: `crates/xtask/parts.yaml`, the tooling that reads it, its checks, and the lines that carry its marker. That file lists its words as it lists each part's, and step 4 below holds a line that names it to its marker as it does for each part. `just remove` never removes it.

An optional part consists of everything that exists only for it:
- its code and its registration
- the items of other modules, and of `platform`, that only it uses, such as a permission, a function or a method
- the dependencies that only it uses
- its emails, with their templates
- its tables, each created and changed only by migrations of its own
- its tests
- its section of the module spec, or for a module its whole spec and its row in the spec index
- its settings in `.env.example` and in the reference deployment's files
- the metrics it declares, with the dashboard panels and alerts that use them, its panels in a dashboard of its own, since a dashboard's JSON holds no comment for a marker (D119)
- its routes in `openapi.json`
- every sentence, list item or table row of a document that exists only for it

What several parts use is the core's, in the shape that serves each of them, as the one operation that creates a workspace is, which every part that creates workspaces calls, and as the event that tells a user they joined one. So is what a rule asks of any code that a fork adds, though one part alone uses it here, as the client for URLs that a third party gives is, which SEC-10 asks of every request to one. A marker deletes a line with its part, and none keeps a line while any one of several parts stays, so a fork that removes every part that uses something of the core's keeps it: one whose kept parts add no member through a request keeps that event in its catalog, which nothing publishes there. A part's tests go with it, so a clause of a behavior that stays without the part is asserted by a test that stays too, as the tests' stand-in provider, whose second client's tokens don't lapse, proves what the registry does with a deletion of a token that fails (AUTH-079), which a provider's own tests would take with them. `just remove <part>` deletes all of these, together with every link and ID that points into them, and leaves a codebase that passes `just ci`:
1. It deletes the files that the part owns whole, which `xtask` lists for each part. A module owns its directory, its templates' directory, its test files, its spec, and the migrations whose names hold its name (DATA-09).
2. In every other file, it deletes what the part's markers mark. A marker is a comment that reads `part: <part>`, in the file's own syntax: `// part: <part>` in Rust, `# part: <part>` wherever `#` starts a comment, as in `clippy.toml`, Compose's files, the justfile, shell scripts and the settings files, `<!-- part: <part> -->` in Markdown, and `%% part: <part>` in a Mermaid diagram. A line that holds a marker after other text, as a table row does in its last cell, is deleted. A marker alone on its line opens a block, which the next line that holds only `end part: <part>`, in the same syntax, closes; the block is deleted with both lines. Outside Rust, whose blank lines rustfmt sets when step 3 formats the code, each run of lines that it deletes that stood between two blank lines, or between a blank line and the file's end, takes one blank line with it. A blank line holds nothing but white space, and this step deletes no other, since a blank line can be part of what a file says, as in a block of text in a Compose file.

   What exists only for two parts together is a line or a block of one inside a block of the other, which removing either deletes, as a workspace shape that two parts give is, whose name is a word of each of them. A table's row, which no block holds, takes one marker, so what two parts state together is written outside a table, or so that each row needs one part alone.

   In a Rust file, every marker is Rust's `//` comment, alone on its line once rustfmt has formatted the file. rustfmt joins the short elements of an array or of a call's arguments onto one line, and moves a comment among them onto the element before it, so what a part adds to such a list goes outside it, such as in a statement of its own that extends the list. It also moves a comment that follows the block of a match's last arm onto the block's closing line, and leaves one that follows an expression, or the block of an arm that another follows. Removing a part can make another part's arm the last, so a part's arm that no arm of the core follows has an expression for its body, never a block.
3. It deletes each entry of the query cache (DATA-14) whose query no code it kept holds, updates `Cargo.lock` to the dependencies left, writes `openapi.json` and the metric catalog again, writes "None." in each section of a spec that it left empty, as the [spec README](../spec/README.md#sections) asks of a section with nothing in it, retires the number of each behavior that it deleted from a spec, in the spec's line of retired IDs, so the fork gives no later behavior that number (DOC-08), and formats the code.
4. It fails while any word of the part, which `xtask` lists with its files, appears in a file it kept or in the path of one, naming each line and path. A line's end may stand for a space of a word: it also finds a word whose text before one of its spaces ends a kept line, white space aside, and whose text after that space begins the next kept line, after the white space and the `///`, `//!`, `//`, `#`, `--` or `%%` that the line begins with.

A fork chooses its parts once, with `just keep <part>...`, which names every part that the fork keeps, one that creates workspaces among them, since every shape has one: it removes every other part, `removal` among them, as `just remove` does, then takes every marker out of the files it kept, so that the fork's code names the parts it kept as it names anything else. A line that holds only a marker goes, and a marker after other text goes from its line, which keeps the text on each side of it. Outside Rust, the lines that go take a blank line with them, as step 2 says of the lines that a removal deletes. Until then, `just remove` removes parts and keeps `removal`.

Step 4, and the Check's search for a part's words, leave out the migration workspace (DOC-13), which the migration's close deletes. Step 2 deletes there too what a part's markers mark, such as the decisions and the reviews that exist only for it, whose links and IDs would otherwise point into what the part removed.

So a line that names an optional part either exists only for it, and carries its marker, or is written so that it doesn't name it, with the part's words in a line of their own, such as a sentence of a paragraph on its own source line. A fork removes optional parts before its first release: its history starts fresh (D29), so no release of its own contains the migrations it deletes (DATA-09, DATA-10). Every other module is a core module. No Cargo feature flag selects modules or optional parts. A part adds what it needs to a core module in files that it owns whole and on marked lines of the module's other files, as its permissions in the role matrix and its steps in `seed` are: no core module offers a registration point so that a part marks fewer of its lines (D182).
- Why: a fork that doesn't need a feature removes it by deleting it, and the workspace parts it keeps give its product its shape (D29, D39). The template keeps every part, so the code of the parts most forks keep is compiled, tested and deployed as the template's own (D73). A fork that has chosen carries no marker, no tooling and no check of a removal that it will never make (D209).
- Check: `ci: check-parts`, in `just checks`, plus `ci: check-removable`, which runs on `main`, plus `review`, each for what its items below say.
  - `ci: check-parts` does in memory, in under a minute, the removal of each run as the run's task makes it, with `removal`'s for a run of `just keep`, and without it for the run of `just remove`: the parts that one workspace shape doesn't keep, one part alone, where no shape's run removes it alone, or every part that one fork can remove at once, so the lines that several parts mark go together too, or `removal` alone, as for a fork that keeps every part, and never every part that creates workspaces, since every shape keeps one.
  - It does the removal of each part alone too, as `just remove` does it, which keeps `removal`, where no run removes that part and no other, so a line that names a part inside the block of another, `removal`'s among them, with which every run removes it, fails; that removal reads names alone, so it is made of every part, the last one that creates workspaces among them.
  - It fails on a run that the `matrix.run` of CI's `removable` job doesn't hold, or holds twice, and on a name there that is no run's, on the repository and on what each removal that keeps `removal` leaves of the two files, so CI makes every run.
  - It fails on each of these:
    - a marker that names no part
    - a line that holds a marker and starts a second
    - a block that no end marker closes
    - a marker in a Rust file that isn't Rust's `//` comment alone on its line
    - a part's arm of a match whose body is a block and which no arm of the core follows
    - an attribute or a doc comment right above a Rust block that holds the item it belongs to
    - a marker that marks its line before more of the line's text, outside a table row, whose removal would take that text too
    - a test, in a file that a part owns whole, named by a behavior of a module other than the part's own, if it has one, whose row in its spec the part doesn't mark, which removing the part would take with it (TEST-04), unless a part that marks the row marks the line that declares the test too, so the test proves a clause that exists only with both parts, and goes with either
    - a Rust file left that doesn't parse
    - a name, in one left that parses, that a `use` of the file imported, no `use` left imports and the code left names, or that a `use` left without `pub` imports and only lines that the removal deletes named, as the doc comment of `crates/xtask/src/parts/imports.rs` reads names, with its exceptions
    - a line or path left that holds a word of a part removed, outside the files that a removal writes again
    - outside them too, a line left that cites a decision, `D` and its number with no ASCII letter or digit on either side, whose row in the tracker the removal deletes, or, outside the inventory and the tests of the checks, which DOC-12 exempts, cites by its ID or its test prefix, as `check-docs` reads them, a behavior whose row in a spec, or a rule whose heading in the standards, the removal deletes
    - a line left that links, as `check-docs` reads the links of a document and of a Rust file's doc comments, to a file, or a directory, that the removal deletes whole, or to a heading's anchor that a document gives and its kept lines don't
    - a line left, outside the inventory and the files that a removal writes again, that names a Rust file, by a path or a name that `check-docs` reads of a document's line or of a comment (DOC-01), while the removal deletes every file that the path or the name names
    - a line left, outside the migration workspace and the tests of the checks, that holds `<module>::<name>`, `::` between two whole words of ASCII letters, digits and `_`, for a name that a `pub` `use` of `crates/app/src/modules/<module>/mod.rs` gives, as a path's last name or the name after `as`, a glob giving none, only on lines that the removal deletes
  - `ci: check-removable` runs `just keep` on a copy of the repository for each run, with every part that the run doesn't remove, then `just checks` on the copy, as on a fork that has chosen its parts; for the one run whose task is `remove`, it runs `just remove` with the run's parts instead, every part that one fork can remove at once, so what that task leaves, the markers of the parts kept and the checks that read them among it, passes `just checks` too.
  - `review` covers what only the image and the end-to-end tests would show of a copy, and a line that exists only for a part without naming it.
<!-- end part: removal -->

## Layers

### ARCH-08 Handlers only extract, authorize, call and respond
A handler in `routes.rs` extracts its inputs, receives its authorization through its extractor types, calls one service function, and shapes the response. It contains no SQL and no business rules, and opens no unit of work: the service opens it, or `Authorize` on a workspace route (architecture §6).
On a superadmin's route, `AuthorizeSuperadmin` opens it. <!-- part: superadmins -->
A function of the routes that no route registers calls no service function, so the handler's one call is the only one.
- Why: business rules live in exactly one layer.
- Check: `ci: check-layers` (no `sqlx` in a module's routes, no handler calls `check` on the `RateLimits` it extracts, which the service counts, no handler makes a second of the calls that the check counts as a service function's, which are a method called on a name that a parameter binds as `State(name)` and a function at a path that starts with `service::`, no function that no `routes!` registers makes one, and the routes call no `begin`), plus `review` (the rest, a handler's call through a state that its parameter takes apart, or through a field of it, among it).

### ARCH-09 Services contain no HTTP types
`service.rs` doesn't use `axum`, `http`, or request and response types. A domain type that a handler reads from its request isn't one: a job or a test builds it without a request.
`ranges::Wanted` is such a type: it holds what a download asks of the bytes it serves.
- Why: business rules can be called from jobs and tests without HTTP.
- Check: `ci: check-layers`.

### ARCH-10 Repositories contain only SQL
`repo.rs` runs queries and maps rows to domain types. It makes no decisions beyond translating database outcomes (ERR-05).
- Why: a rule hidden in a query is invisible to readers of the service.
- Check: `review`.

### ARCH-11 One unit of work per operation, and the commit decides the outcome
An operation that spans modules passes one `&mut UnitOfWork` through every call. A service never opens a second transaction for the same operation, but for one read before the unit of work opens, in a short transaction that writes nothing: what decides which rate limit counts the request, as a sign-in reads its device token (AUTH-019), and as authentication, before the service runs, reads the session whose user's limit counts the request (PLATFORM-054), with whether its account must still prove its address (AUTH-025) or set up a second factor (AUTH-070). A unit of work that finds, once it holds the lock it waited for, that what this read decided holds no more, ends with nothing written, and its operation counts in the limit that applies after all and reads again in a new unit of work, as a sign-in through a provider registers once the account that it read as linked has gone (AUTH-099, D147).
Or, on an upload route, whether the caller may upload, which the handler checks through `Authorize::check` before it reads the body, and which the unit of work then authorizes again (PLATFORM-073). <!-- part: files -->
Or, on the telemetry proxy's routes, whether the caller consents to their clients' telemetry, which decides whether the budgets count the batch; no unit of work follows that read, since the service writes nothing but those counts (CLIENT_TELEMETRY-005). <!-- part: client_telemetry -->

The operator's end of every account's sessions is an operation for each account: it reads the users that hold a session in short transactions that write nothing, and ends each user's sessions in a unit of work of their own (AUTH-094).
A periodic job's run is an operation for each batch of rows that it works through: it opens a unit of work for the batch, so a run that ends partway leaves the rest to the next run, as the platform's sweeps do (PLATFORM-019).
The job that deletes the objects that no row names opens two for each batch, one on each side of the bucket's deletions (DATA-21). <!-- part: files -->
The job that copies a provider's picture opens two: before its outbound request, a short transaction that writes nothing, which reads whether the account has an avatar, and so whether the job fetches the picture, and then its unit of work, which records the picture once the job has fetched and stored it (ACCOUNTS-030, DATA-21). <!-- part: files -->
A download that streams bytes it stored, such as an export's documents, after its unit of work commits reads them in short transactions of their own that write nothing, since those bytes never change and the response reports what it committed already (ACCOUNTS-026).

Three records are written outside it, each so that what happens to the operation can't undo it:
- a rate limit's count, which commits before the unit of work opens, so a request refused afterwards still counts (PLATFORM-011)
- a refused authorization, which the `SecurityEventSink` records in a short transaction of its own, once the operation's has rolled back, so nothing of the refused operation commits while its refusal does (PLATFORM-010)
- a job's last attempt that failed, which its job records in a unit of work of its own once the attempt's has rolled back, as an export's job makes its export failed (ACCOUNTS-023)

Once the unit of work commits, the response reports what it recorded. A unit of work that recorded a security failure (PLATFORM-008) answers with that failure, such as 401 for a failed sign-in, and any other commit answers with success. Work after the commit happens only through jobs and events, so it can't turn a committed change into an error, but for two kinds: a download's reads of the bytes it stored, and the send of an email that holds a secret its job made, which the job sends once it has committed the secret's hash-based message authentication code (HMAC), so no email carries a secret that no row proves, and whose failure fails the attempt, whose retry makes a new secret, in place of the committed one or beside it, as the module's spec says (D128, AUTH-030, AUTH-036, AUTH-050).
- Why: the whole operation commits or none of it does, and the response always matches what was committed (D1, D36).
- Check: `review`, plus `test` (PLATFORM-008).

## Composition

### ARCH-12 One composition root
The composition root, `composition/`, builds every service once at startup, hands each one its dependencies, registers the `platform` traits that architecture §5 lists as registered there, and mounts every module's routes. Nothing else constructs services. The process's resources aren't services: `main.rs` opens them before the services are built, each that can fail as a step of startup with its own `startup.area` (OPS-03). It hands the composition root the database, the clock, readiness, the listener, the workers, the turns of CPU-heavy work, and the mail, the transport to the mail server that `Mail` holds, with `startup.area` `mail`, and, in `serve`, keeps for the server it runs the signal handlers and the socket that the server listens on, with `startup.area` `signals` and `listen`. Global mutable state is not allowed. The only exceptions are metric handles and other read-only values that are initialized once, and the state the test support keeps for the tests of one process, such as the template database's name, or for the thread of one test, such as the watch of its lines.
- Why: every dependency is visible in one file, and tests build the same graph.
- Check: `review`.

### ARCH-13 Dependency traits only for inversion or multiple implementations
A trait that abstracts a dependency exists for one of two reasons:
- It inverts a dependency the crate or module graph forbids: `SessionVerifier`, `AccessPolicy`, `SecurityEventSink`, the personal data contract, and `ExportStore`.
  `SuperadminPolicy` inverts one too. <!-- part: superadmins -->
  The operator's setup of a workspace that the composition root hands `seed` inverts one too, as a function, since `system` uses no other module. <!-- part: workspace_setup -->
  The operator's making of a superadmin that the composition root hands `seed` inverts one too, as a function, since `system` uses no other module. <!-- part: superadmins -->
- It covers an external system with more than one implementation: the clock, which the tests replace with one they move, and a provider of sign-in, with one implementation for each provider and the tests' stand-in.

No trait exists just so something can be mocked. Traits that define a behavior many types share, such as `ErrorCode`, `Permission` and job handlers, aren't dependency abstractions, and this rule doesn't limit them.
- Why: an interface with no second implementation and no inversion adds indirection without flexibility. Tests use the real database (D23).
- Check: `review`.

### ARCH-14 Application tasks are binary subcommands
Every task that runs application logic against the database, such as bootstrap, migrate and seed, is a subcommand of the app binary. A script reaches the database only through Postgres's own tools, for what Postgres itself does: pgBackRest, the reference deployment's backup tool, backs it up and restores it (D44, OPS-15), and a restore asks Postgres, through `pg_isready` and `psql`, whether it runs and how far its replay has come, and commits a transaction that changes nothing ([backups.md](../operations/backups.md#restoring)). No statement a script sends names a table of the product schema (DATA-01). The repository's own tools reach the database too: the CI checks that inspect the schema or the compiled queries, `just prepare`, which rewrites the query cache from the migrated database (DATA-14), and `just new-module`, which rewrites it from a database of its own that the migrations, its module's among them, build.
So is the task that runs a module's logic against the files bucket alone, `restore-files` (FILES-028). <!-- part: files -->
`just load-test streams` arranges the workspaces whose owners rename them in the database of the machine it runs on, as a test arranges its state (TEST-05), since no route that every fork keeps creates one.
- Why: application tasks use the same configuration, types and checks as the server.
- Check: `ci: check-shell` (each `psql` of a script under `deploy/` takes its statement with `-c`, with no expansion, and the statement names no table of the product schema, as the doc of `psql_problems` in [`scripts.rs`](../../crates/xtask/src/shell/scripts.rs) says it reads them), plus `review` (what a script asks of Postgres's tools).

### ARCH-15 The product name is renamable
The product's name is the value of `PRODUCT` in `crates/app/src/modules/mod.rs`: words of ASCII letters and digits, with one space between two, the first starting with a capital letter. A file holds it in one of two forms: the name itself, which the emails and an authenticator app show, or its slug, its words in lowercase with a hyphen between two, which names the binary, `<slug>-api`, and so the service in telemetry and the title of `openapi.json`. The app's code that shows the name, such as an email's subject and the issuer that an authenticator app shows, reads `PRODUCT`, and the code that names the service reads the binary's name from Cargo. A test that compares a form of the name that a format encodes, such as a link's or a header's, encodes the spaces that a name may hold. `just rename <name>` replaces each form with the same form of the new name, and changes nothing while a file that it reads would still hold the old name in another form, or a file already holds the new one, as `crates/xtask/src/rename.rs` says. The image takes the name of the repository that publishes it, so a fork's image has the fork's name.
- Why: a fork becomes its own product with one command (D29).
- Check: `ci: check-rename`, which runs on `main`, renames a copy of the repository with `just rename`, to a name of two words, the first of three that the product can take, so that a fork whose name or files hold one passes with the next, then runs `just checks` on the copy. `review` covers what only the image and the end-to-end tests would show of the copy.
