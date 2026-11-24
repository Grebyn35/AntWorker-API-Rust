# Data rules (`DATA`)

Schema, migrations, database roles, tenancy, queries and transactions.

## Schema

### DATA-01 Naming
These names apply to the product schema: the tables, views, columns and constraints that the files in `migrations/` create.
- Tables and views are plural `snake_case` (`workspace_memberships`).
- A foreign-key column is named `<singular>_id`, after the table it references (`workspace_id`), or `<role>_id`, after the role of the row it references, with that singular where the role alone doesn't tell the table (`publisher_id`, `actor_user_id`). Timestamps are `<event>_at`.
- Constraints and indexes take these names, which a migration gives them, but for a primary key and a `NOT NULL` that a column declares, whose names it leaves to Postgres:
  - `<table>_pkey` for the primary key
  - `<table>_<columns>_key` for unique constraints, and for the unique indexes that back no constraint, such as a partial one
  - `<table>_<columns>_fkey` for foreign keys
  - `<table>_<columns>_check` for checks, whose `<columns>` are some of the columns that the check reads
  - `<table>_<column>_not_null` for a `NOT NULL` that a migration adds to a column of a table that an earlier one created (DATA-10)
  - `<table>_<columns>_idx` for the other indexes
- Why: ERR-05 maps constraint violations by name, and a predictable name needs no lookup.
- Check: `ci: check-schema`, which reads of a foreign-key column's name only that it ends in `_id`, of a check's name only that what stands between its table's name and `_check` is some of the first eight columns that the check reads, in the catalog's order, joined by `_`, and of the name of an index on an expression only that it starts with its table's and ends in `_key` or `_idx`, and reads no `NOT NULL`'s name, plus `review` (what those names are taken from, and a `NOT NULL`'s name).

### DATA-02 Standard columns
- Every table of the product schema (DATA-01) has `id uuid PRIMARY KEY DEFAULT uuidv7()` and `created_at timestamptz NOT NULL DEFAULT now()`.
- A table whose rows are edited, which is one that `app` may update, also has `updated_at` and `version`. One shared trigger maintains both. A foreign key's `SET NULL`, which clears a row's reference to a user as their account goes (DATA-05), is no edit. A table of `platform`'s bookkeeping, whose rows only its own mechanism changes and no client edits, such as the rate-limit buckets, has neither.
- Pure join tables use a composite primary key instead of `id`.
- These columns are a table's: a view, which stores no rows, needs none of its own.

The migrator's own table, `_sqlx_migrations`, follows the layout sqlx gives it, and tables that tests create are throwaway, so neither is part of the product schema.
- Why: IDs are time-ordered and index-friendly, and every editable row supports `ETag` (D24, API-13).
- Check: `ci: check-schema`.

### DATA-03 The database clock is the only clock for stored time
Stored timestamps come from the database's clock in SQL, never from the application: `now()`, the time the transaction started, or for an event, which records when its unit of work committed, `clock_timestamp()` as it commits (PLATFORM-028). A time that the database's clock gave may be bound back into a later statement: a list's cursor compares one with its rows' times, and a timestamp stored from one is computed in SQL, as a periodic job's next run is, one interval after the `now()` of its claim (PLATFORM-015). Expiry checks run in SQL against `now()`.
- Why: one clock, at full precision, so ordering and expiry agree across replicas (D24).
- Check: `review`.

### DATA-04 The database enforces integrity, domain types enforce values
The schema declares keys, uniqueness, foreign keys and `NOT NULL`. Closed sets are Postgres enum types mapped to Rust enums: a set is closed when the template's own migrations fix its members, as they fix a membership's roles. A name from a set that a part, a fork or `just new-module` changes, which a permission's name, an event's, an audit action's, a job's kind and a sign-in provider's key are, is stored as `text`, written from the declaration that names it. The rules for a value's length, format or content live only in its domain type (D25), not in `CHECK` constraints: a `CHECK` relates columns of a row, such as a job's lease to its claim.
- Why: each rule is defined once.
- Check: `ci: check-schema` (no check constraint reads a single column), plus `review`.

### DATA-05 Foreign keys are declared and indexed
Every reference is a declared foreign key with an explicit `ON DELETE`: `CASCADE` for children the parent owns, `SET NULL` for a reference to a user from a row that outlives them, which deleting their account anonymizes (DATA-23), and `RESTRICT` otherwise. Three kinds of reference are exceptions, and outlive the rows they name: an audit entry's `resource_id`, which may name a row of any table (DATA-22), a job's `user_id` and `workspace_id`, which record whom it runs for (DATA-23), and a sign-in challenge's `device_token_hash`, the hash of the device token that its sign-in brought, which its completion deletes if it is still stored (AUTH-019). Every foreign-key column has an index.
- Why: deletes behave as the schema says, and cascades stay fast.
- Check: `ci: check-schema`.

### DATA-06 Deletes are hard deletes
Rows are deleted, not flagged. No table or view has a soft-delete column.
- Why: deleted personal data is actually gone (D24, D31).
- Check: `ci: check-schema` (no `deleted_at` or `is_deleted` columns).

### DATA-07 No business logic in SQL
Migrations define no stored procedures and no `SECURITY DEFINER` functions. The only function allowed is the trigger function that maintains `updated_at` and `version` (DATA-02).
- Why: business rules are read, tested and changed in Rust (D4).
- Check: `ci: check-schema` (the schema's one function is the trigger function that the `BEFORE UPDATE` row triggers of the tables outside `platform`'s bookkeeping run, and it isn't `SECURITY DEFINER`).

### DATA-08 The database generates IDs
Inserts take their rows' IDs from the column default, using `RETURNING`, but for a row whose secret is encrypted bound to its ID, which exists before the secret is written (SEC-02): its ID comes from the database's `uuidv7()`, the default's own function, in a statement just before the insert. Application code creates no row's ID; a value that names no row, such as an email's `Message-ID`, isn't one. A test's arrangement may give a row that it inserts an ID of its own, by which the test names the row without reading it back.
- Why: one source of IDs (D24).
- Check: `clippy` (`Uuid::new_v4`, the one function of the `uuid` crate's enabled features that generates an ID, is in `disallowed_methods`), `ci: check-table-ownership` (no `INSERT INTO` in a `sqlx` macro of `platform` or `app` names `id` among its columns, or has columns that the check can't read, but for the tables of the rows that SEC-02 binds to their IDs), plus `review` (SQL outside those macros, as the tests' and `xtask`'s).

## Migrations

### DATA-09 Forward-only migrations in one directory
Migrations live in `migrations/` and run through the sqlx migrator. Each file is named `<timestamp>_<module>_<description>.sql`, where the module is the one that owns what it creates (ARCH-06). A migration in a release, final or prerelease, is never edited or deleted. A migration that no release contains yet may be edited, and every database that applied it is then recreated, because `migrate` and `serve` refuse a database whose applied migration differs from its file (PLATFORM-006, PLATFORM-030). Migration files that tests use to exercise the migrator are test fixtures, not part of the product schema.
- Why: the schema history is linear and verifiable by checksum (D15).
- Check: `ci: check-migrations` (each name holds a module, and no migration that the newest release holds is changed or deleted).

### DATA-10 Expand, then contract
A migration keeps the previous release working. A column or table is removed or renamed only after a release in which no code uses it. An index of a table that another migration created is built `CONCURRENTLY`, in a migration of its own that runs outside a transaction, so that the build doesn't keep the table's writes waiting while the previous release's replicas serve (PLATFORM-005, D210). A foreign key, a check or a `NOT NULL` of such a table is added as a constraint that is `NOT VALID`, which holds for each row written from then on, and a later migration validates it: added in full, it reads every row of the table under a lock that keeps the table's writes waiting, where a validation's lock lets them through. A unique or primary-key constraint of such a table takes an index that an earlier migration built, with `USING INDEX`.

The jobs a release enqueues are data the next one reads: a job's kind is renamed or retired only after a release that still runs it and enqueues no more of it, and a payload keeps the fields that the previous release writes, since a waiting job whose kind no replica runs never runs and never dead-letters ([platform spec](../spec/platform.md#the-job-queue)). A keyed operation's stored result is data the next release reads too, for as long as its record lasts: its type keeps the fields that the previous release writes, and adds only fields that may be absent, so a retry that a deploy lets the other release answer still answers the first result (API-12, [platform spec](../spec/platform.md#idempotency)).

The access-change signals that one release's replicas send, the other's hubs read while a deploy runs both: a signal keeps the fields that the previous release sends, and adds only fields that the previous release's hub ignores and the new one does without, so that every hub acts on every signal ([platform spec](../spec/platform.md#events-and-the-real-time-hub)). An event that one release stores, the other's hubs deliver, while a deploy runs both and in the replays of the day that events are kept: a hub passes on the stored name and payload as they are, so they change only as API-18 lets what a client reads change. The permission that an event or an audit entry stores is a name, which the release that reads the row compares with the names its roles hold: a permission keeps its name while a stored row that a read compares holds it.
- Why: rolling deploys run old and new code against one schema, one queue and one channel of signals (D15).
- Check: `ci: check-migrations` (a migration that builds an index of a table that another migration creates is marked `-- no-transaction`, as the check's own doc says how it reads one, and an `ALTER TABLE` of such a table adds and validates a constraint in the forms that the doc of [its `constraints.rs`](../../crates/xtask/src/migrations/constraints.rs) names), `ci: check-schema` (the schema that every migration built holds no constraint that isn't valid), `review`, plus `test` (a membership signal without its transaction, and a keyed creation's result without its permissions, each as a release that doesn't write the field gives it).

### DATA-11 Migrations are tested both ways
A test applies every migration to an empty database. To a second, it applies the migrations that the newest release holds (DATA-09), then the migrations as they are, which adds the new ones. The two databases then hold one schema, by the lines that the test reads of each one's catalog, a table's columns in their order among them. In a repository without a release, such as a fork before its first, the second database takes every migration as the first does.
- Why: fresh installs and upgrades both work (D213).
- Check: `test`.

## Roles

### DATA-12 Two database roles
| Role | Used by | Attributes |
|---|---|---|
| `owner` | the `migrate` subcommand only | owns the tables it creates, and has `CREATE` on the schema `public`, which the database's owner keeps |
| `app` | every request and every job, `seed`, which sends the API's routes requests in process ([system spec](../spec/system.md#seed)), and each subcommand that an operator runs against the accounts, such as `remove-second-factor` | no superuser, owns nothing, data manipulation language (DML) statements only (DATA-19) |

The `bootstrap` subcommand creates both roles with these attributes, grants their privileges on the database and sets the role-level timeouts (DATA-19). The privileges on the tables come from the first migration. It runs with an administrator connection: the bundled Postgres superuser, or a managed provider's admin role. Running it again keeps the roles, their attributes and the grants, and sets each role's password again from its URL. Roles belong to the whole Postgres server, so a deployment's server holds its database alone (D75), and in production `bootstrap` refuses a server that holds another database of the roles before it changes one.
- Why: the running API can change data but never the schema, and holds no privilege beyond that (D4).
- Check: `test` (PLATFORM-001, PLATFORM-002, PLATFORM-003), plus `ci: check-schema` (grants).

### DATA-13 Startup refuses an unsafe role
The server refuses to start unless it connects as the role `app`, and that role isn't a superuser, can't create databases or roles, can't start replication, doesn't bypass row-level security, owns nothing in any database of the server, and belongs to no other role.
- Why: a role created by hand with any of those attributes would give the running API far more power than it needs.
- Check: `test` (PLATFORM-001).

### DATA-19 Least privilege in grants
- `EXECUTE` is revoked from `PUBLIC` on every function in the application schema, including through default privileges.
- `app` has `SELECT`, `INSERT`, `UPDATE` and `DELETE` on the application schema only, with no data definition language (DDL) statements and no `TRUNCATE`. On `audit_entries` and `audit_entry_clients` it has only `INSERT` and `SELECT`, and on `deletable_audit_entries`, the view through which retention deletes, which shows only the entries recorded more than 90 days ago, only `SELECT` and `DELETE` (AUDIT-005), and on each table whose rows are never edited, and which so has no `updated_at` or `version` (DATA-02), no `UPDATE`, such as `device_tokens` (AUTH-019) and `verification_codes` (AUTH-030).
  `export_entries` and `export_chunks` are such tables too (the accounts spec's Owns).
  So are `workspace_files`, `user_files`, `avatars`, `workspace_pictures`, `exported_files` and `object_deletions` (the files spec's Owns). <!-- part: files -->
  So is `workspace_setups` (the workspaces spec's Owns). <!-- part: workspace_setup -->
  So is `superadmins` (the superadmins spec's Owns). <!-- part: superadmins -->
  On a sequence that a mechanism of `platform` draws from, such as `events_position_seq`, it has `USAGE` alone, which takes the next value, and can't set one.
- `app` has `statement_timeout` and `idle_in_transaction_session_timeout` set at the role level.
- Why: a role can do only what its job needs, and a stuck transaction can't hold locks indefinitely.
- Check: `ci: check-schema`.

## Queries

### DATA-14 SQL is checked at compile time
All SQL that the Rust of `platform` and `app`, and of their tests, sends through sqlx goes through its `query!`, `query_as!` and `query_scalar!` macros. The `.sqlx/` offline cache is committed, and CI builds with `SQLX_OFFLINE=true`, from a cache that `just prepare` wrote after the last change to a query or a migration. Optional filters are nullable parameters in a static query (`$1::text IS NULL OR action = $1`). SQL that can't be a macro query is allowed only in these places, each through an `#[expect]` that states its reason: the statements of `platform::db` that take identifiers, such as role statements; the test support (TEST-05); and a dynamic query that a module spec requires, built with `QueryBuilder`, which never concatenates input. SQL that goes through `psql` takes no part of sqlx: what a script or a task of `xtask` sends is as ARCH-14 says, and an end-to-end test sends its statements to a deployment's database so, through the server that runs it.
- Why: a query that doesn't match the schema doesn't compile (D24).
- Check: `clippy` (`disallowed_methods` and `disallowed_macros` ban every function, method and macro of sqlx that takes SQL text outside the checked macros, `QueryBuilder`'s among them), `ci: check-sql-bans` (every function, method and macro that the locked sqlx exposes and that takes SQL text outside the checked macros is one that `clippy.toml` bans, so an update of sqlx can't add one unbanned), `ci: sqlx-check` (the cache is what sqlx-cli writes from a database that the migrations build), plus `review` (where an `#[expect]` allows string SQL, and where code names `sqlx::sqlx_macros`, whose `expand_query!` the checked macros expand to, so clippy can't ban it). `disallowed_macros` bans the checked macros that read a query from a file too, `query_file!`, `query_file_as!` and `query_file_scalar!`, since `check-tenancy` and `check-table-ownership` read a query's text in its macro.

### DATA-15 Repositories take a unit of work
A repository function takes `&mut UnitOfWork`, never a pool or connection. Pools, connections and transactions stay inside `platform::db`, and the test support is the one other place that names them (TEST-05).
- Why: every query runs inside the operation's transaction (ARCH-11).
- Check: `clippy` (`disallowed_types` bans sqlx's pools and pool options, its pooled and plain connections, its transactions and its listener, with their Postgres aliases; `disallowed_methods` bans opening a connection, and opening a transaction from one or from a pool; `platform::db` and the test support allow them through `#[expect]`), `ci: check-sql-bans` (each ban of `disallowed_types` that names a crate of sqlx's reaches something in it, since clippy only warns of one that reaches nothing), plus `review` (where an `#[expect]` allows them).

### DATA-16 Every query filters by tenant itself
Tenancy is enforced by the application, and every query on user or workspace data carries its own filter (D3, D4). Data a module adds for its product lives in a workspace, in a table with a `workspace_id` (D39); a table without one holds account data, keyed by a user, such as `sessions`, or is `workspaces` itself, or is a module's bookkeeping of what it keeps outside the database, which holds no tenant's data. `platform`'s bookkeeping, such as `jobs` and `events`, is platform data whatever its columns: a job's `workspace_id` and `user_id` record whom it runs for, which its handler filters by, while the queue selects jobs by their state; an event's record whom it reaches, which the replay of a stream filters by, while the hub loads events by their position. `check-tenancy` lists the tables of each kind.
- Workspace data is filtered by the workspace ID from the proof the operation runs under: `Authorized<P>`, or the `JobContext` of a job enqueued for a workspace. A repository function takes that ID as a parameter.
- User data is filtered by the acting user's ID. The hub acts for each open stream's caller, whose memberships and sessions it reads again by their IDs (SEC-06).
- A lookup by a presented token's hash, or of a user by an email address, goes through a unique index:
  - before sign-in, by the token or the sign-in email
  - in a password reset, by the address it names (AUTH-034, AUTH-037)
  - when asking to change one's address, by the address asked for, which no account may hold (AUTH-048)
  - when inviting, by the invitee's email, and when changing, sending again or cancelling an invitation, by its address <!-- part: invitations -->
  - in the setup subcommand, by its owner's email (WORKSPACES-023) <!-- part: workspace_setup -->
  - in the operator's removal of a second factor, by the address it is given (AUTH-072)
  - in the operator's end of an account's sessions, by the address it is given (AUTH-094)
  - in the operator's making of a superadmin, or ending of one, by the address it is given (SUPERADMINS-001) <!-- part: superadmins -->
  - in a sign-in through a provider, of the user whose provider identity holds the provider's subject, by the provider and the subject (AUTH-073)
  - for a signed URL, of its session by the ID that its signature proves the API gave (PLATFORM-080) <!-- part: files -->
- Rows addressed to a user by an email address are filtered by the caller's email.

A row ID alone never addresses tenant data. These reads are exempt:
- batch reads by IDs that came from a query filtered by tenant in the calling module: of display data, such as names, for example a workspace's member list, or of which of those IDs match a search, as `accounts::matching` finds them (API-20)
- the deletion of an account, which reads whether the workspaces the user owns have other members, by the IDs of the user's own memberships, and deletes those workspaces by them (ACCESS-008, ACCOUNTS-013)
- the export of a user's data, which reads the workspaces whose only member is the user by the IDs of the user's own memberships, and those workspaces' content by them (ACCOUNTS-024)
- accepting or declining an invitation, which reads its workspace's ID from the invitation addressed to the caller, by the caller's email, and by that ID locks the owner's membership, and, accepting, adds the caller's (INVITATIONS-005, INVITATIONS-017) <!-- part: invitations -->
- a user's read of their own log, which reads the entries of every workspace's log whose target is them by their own ID alone, since what was done to a user is theirs to see (AUDIT-008, D6)
- the position of the event that a stream's `Last-Event-ID` names, read by the event's ID alone, so a client resumes after the last event it received even from a workspace it has since left; the replay after that position filters by tenant, and the lookup tells a caller only whether an event ID it holds names an event, which a UUIDv7's random bits keep anyone from guessing (REALTIME-004)
- scheduled jobs that span tenants, which select rows by their own criteria, such as expiry
- the operator's end of every account's sessions, which reads the users that hold a session, whoever they are, and ends each user's sessions by the IDs that the read gave (AUTH-094)
- a superadmin's reads, under the proof `Superadmin`, which names no workspace, and which the first two take: of every workspace's name and creation time; of each one's owner and member count, by the IDs that the first read gave; and of those owners' names and addresses, by the IDs that the second gave (WORKSPACES-029, D62) <!-- part: superadmins -->
- a superadmin's deletion of a workspace, under the same proof, which locks and ends the memberships of the workspace that its path names, and deletes that workspace, by the path's ID (WORKSPACES-030, ACCESS-011) <!-- part: superadmins -->
- the operator's list of the deployment's superadmins, which reads every superadmin's row, and those users' addresses by the IDs that the read gave (SUPERADMINS-008) <!-- part: superadmins -->
- the startup check of the versions of the key ring under which secrets are stored, which reads the versions alone, of every user's rows (AUTH-066)
- a public file's download, which reads its row by its ID and its visibility alone, and an avatar's and a workspace's picture's, which each read their row by its ID alone, since whoever holds a public file's ID, an avatar's or a picture's, may read it ([files spec](../spec/files.md#visibility)) <!-- part: files -->
- Why: there is no row-level security, so the application's filters are the only guard between tenants, and they are checked by machine and by test (D4).
- Check: `ci: check-tenancy` (each item below), `test` (TEST-08 sends each workspace route that addresses a row by ID the ID of a row in another workspace the caller belongs to), plus `review` (the exemptions).
  - every query on a table with a `workspace_id` column, platform data aside, filters on it, on its `user_id` where it has one, or on the address its rows are addressed to, and every query of a view that reads such a table filters on a tenant column that the view shows, unless a listed function holds the query: a function of a cross-tenant job, or of the reads that this rule exempts
  - every other table is listed
  - the migrations create each table and view in a form that the check reads, which the doc of `crates/xtask/src/tenancy/mod.rs` names

### DATA-17 Lists use keyset pagination on an index
A list query filters by the cursor's sort key, with the ID as tiebreaker, and never uses `OFFSET`. An index matches its tenant filter and its order. A `search` (API-20) narrows the rows that index reaches, without an index of its own until a load test asks for one.
- Why: fast at any depth, and stable under inserts (API-06).
- Check: `review`, plus `test` (the tests that API-06 names, which page each list across rows sharing a timestamp, such as PLATFORM-046's and WORKSPACES-004's).

### DATA-18 Optimistic updates check the version
An update made with `If-Match` keeps its `WHERE` to the versions the header names, such as `version = ANY($2)`. Zero rows updated becomes `PreconditionFailed`.
- Why: lost updates are detected in the same statement that would cause them (API-13).
- Check: `test` (PLATFORM-071, and each update that takes `If-Match` has a test of a stale version's 412, such as ACCOUNTS-007's and WORKSPACES-013's), plus `review` (that the update's own `WHERE` names the versions, where a read before it would pass the same tests).

## Transactions

### DATA-20 Read committed, with row locks where the spec says so
Transactions use `READ COMMITTED`, but for the job that builds an export's archive, which reads under `REPEATABLE READ`, through `Database::for_snapshot_job`, so that the archive holds every module's data as of one moment, and which locks its export's row before any other statement but the one that sets that level, so that its writes, all of rows it inserts or has locked, meet no serialization failure (ACCOUNTS-023). That lock itself meets one when it waited for a transaction that changed or deleted the export's row, as the export's expiry and a deletion of the account do: the attempt fails, and its retry, where the job is still there, finds no pending export to build ([accounts spec](../spec/accounts.md#exporting)). An operation whose correctness depends on rows that another request may change at the same time does one of two things, as its module spec states: it locks them with `SELECT ... FOR UPDATE`, or a weaker mode of Postgres's row locks, such as `FOR NO KEY UPDATE`, which lets an insert that names the row go on (DATA-23), taking locks in a consistent order, or it changes them with an update that checks what it read (DATA-18). Nothing inside the API retries a transaction. A unit of work may run a locking read again: under `READ COMMITTED`, a read that waited for a change that left its row no match finds none, as a read of a workspace's owner does once a transfer made the member it waited at an admin, and the same read, run again, locks the row that matches. A deadlock is a defect, and surfaces as an internal error. A failed job is retried by the job system (PLATFORM-014).
- Why: a unit of work carries the effects of every module the operation called, so no one layer holds what it would need to re-run it. Under `READ COMMITTED` there are no serialization failures, so a deadlock means two operations took locks in different orders: a bug to fix, not to retry.
- Check: `review`.

### DATA-21 No external calls inside a transaction
A transaction never waits on HTTP or the Simple Mail Transfer Protocol (SMTP): external effects that follow a change are jobs (D1). A request that must call another system to learn what it changes calls it before its unit of work opens, as a sign-in through a provider exchanges its code, and deletes a token that doesn't lapse, before it reads an account (AUTH-077, AUTH-079, AUTH-100).
Nor does it wait on object storage: uploads write the object before the transaction. <!-- part: files -->
- Why: a slow dependency can't hold database locks.
- Check: `review`.

### DATA-22 Side effects join the unit of work
Audit entries (`audit::record`), events (`uow.publish_to_user` and `uow.publish_to_workspace`) and jobs (`uow.enqueue`) are written in the same unit of work as the change that causes them.

What is audited:
- Every change that a request, an enqueued job or an operator's subcommand makes to data a module owns.
- Each security failure: those D6 lists, a failed sign-in with a password to an account that holds the email (AUTH-005), a wrong current password, a reused refresh token and a refused authorization, a wrong verification code, a guess at what proves an account's address (AUTH-032), resets its password (AUTH-038) or changes its address (AUTH-052), a wrong code of a second factor (AUTH-056, AUTH-060, AUTH-065, AUTH-098), and a re-authentication through a provider whose account isn't the caller's (AUTH-101). A failed sign-in with a password to such an account commits its audit entry and then answers 401 (ARCH-11, PLATFORM-008), unless a deletion of the account commits before the record, which leaves no log to hold it, as it leaves none for a guess at what resets the account's password (AUTH-038). Refusals are reported to the `SecurityEventSink` (PLATFORM-010).
- Reads that a module spec names as audited, such as the download of an export (D31).

Platform bookkeeping (rate-limit buckets, idempotency records, job state) and scheduled maintenance (retention, sweeps) aren't audited. Metrics count them instead. Deleting an account records only the entries that the logs of the workspaces the user shared keep, since their own log goes with the account (ACCOUNTS-014, D31). Deleting a workspace records its one entry in the own log of whoever deletes it, since the workspace's log goes with the workspace (WORKSPACES-017).
- Why: a change and its side effects commit together or not at all (D1, D6).
- Check: `review`, plus `test` (each mutating behavior asserts its audit entry and events, such as WORKSPACES-014's and ACCOUNTS-007's).

## Personal data

### DATA-23 Personal data is covered by a contract
Every column that references a user or holds personal data (glossary) is covered by a registered personal data contract (D31). A column that holds personal data carries the column comment `personal_data`, set with `COMMENT ON COLUMN` in its migration. A column that names a user without a foreign key to `users` is covered by the contract that deletes its rows with the user, or else it keeps only an ID that no row holds once the user is gone, and no contract's columns name it (D237). `platform`'s contract covers `jobs.user_id` so, since it deletes the user's jobs, and `idempotency_records.result`, which names no user but the one whose record it is, and goes with them through the record's foreign key. An audit entry's `resource_id` and `events.payload` keep an ID that names no one once the user is gone: the first the ID of a user that the entry names as its resource, while the entry's actor and target go to null through their foreign keys, and the second the IDs of the members that a workspace's events name, until the sweep deletes the events (PLATFORM-019).

`platform` registers a contract for its own tables. Erasing a user deletes their idempotency records and the events scoped to them, and leaves the events that their requests published to others without a publisher, through the column's foreign key. Jobs hold no foreign keys to user data: erasing a user, or deleting a workspace, deletes the jobs enqueued for them, so no job runs for a subject that is gone. An operation that enqueues a job for a user therefore takes a turn with a deletion of their account: it locks the user's row, or a row of theirs that the deletion locks before it deletes their jobs, such as a membership, or it holds the user's row `FOR KEY SHARE`, as the foreign key of a row that it inserts and that names them does, and the deletion deletes the user's jobs again once it holds their row against every such operation. A succeeded job keeps no row, and a dead letter keeps its payload only for its retention (PLATFORM-014). Rate-limit buckets hold only keyed hashes and expire on their own. Export leaves these tables out, because the modules that own the data export it.
- Why: export and erasure can't miss a table, including the platform's own.
- Check: `test` (every foreign key to `users`, and every column commented `personal_data`, in the migrated schema, against the columns of the contracts that the composition root registers), plus `review` (which contract covers a column that names a user without a foreign key to `users`, or that the column keeps only an ID that names no one once the user is gone, and that no contract's columns name it).
