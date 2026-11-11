# Data rules (`DATA`)

Schema, migrations, database roles, tenancy, queries and transactions.

## Schema

### DATA-01 Naming
These names apply to the product schema: the tables, columns and constraints that the files in `migrations/` create.
- Tables are plural `snake_case` (`workspace_memberships`).
- Foreign-key columns are `<singular>_id`, and timestamps are `<event>_at`.
- Constraints and indexes are named explicitly:
  - `<table>_<columns>_key` for unique constraints
  - `<table>_<columns>_fkey` for foreign keys
  - `<table>_<columns>_check` for checks
  - `<table>_<columns>_idx` for indexes
- Why: ERR-05 maps constraint violations by name, and a predictable name needs no lookup.
- Check: `ci: check-schema`.

### DATA-02 Standard columns
- Every table of the product schema (DATA-01) has `id uuid PRIMARY KEY DEFAULT uuidv7()` and `created_at timestamptz NOT NULL DEFAULT now()`.
- A table whose rows are edited also has `updated_at` and `version`. One shared trigger maintains both. A table of `platform`'s bookkeeping, whose rows only its own mechanism changes and no client edits, such as the rate-limit buckets, has neither.
- Pure join tables use a composite primary key instead of `id`.

The migrator's own table, `_sqlx_migrations`, follows the layout sqlx gives it, and tables that tests create are throwaway, so neither is part of the product schema.
- Why: IDs are time-ordered and index-friendly, and every editable row supports `ETag` (D24, API-13).
- Check: `ci: check-schema`.

### DATA-03 The database clock is the only clock for stored time
Stored timestamps come from the database's clock in SQL, never from the application: `now()`, the time the transaction started, or for an event, which records when its unit of work committed, `clock_timestamp()` as it commits (PLATFORM-028). Expiry checks run in SQL against `now()`.
- Why: one clock, at full precision, so ordering and expiry agree across replicas (D24).
- Check: `review`.

### DATA-04 The database enforces integrity, domain types enforce values
The schema declares keys, uniqueness, foreign keys and `NOT NULL`. Closed sets are Postgres enum types mapped to Rust enums. The rules for a value's length, format or content live only in its domain type (D25), not in `CHECK` constraints: a `CHECK` relates columns of a row, such as a job's lease to its claim.
- Why: each rule is defined once.
- Check: `ci: check-schema` (no check constraint reads a single column), plus `review`.

### DATA-05 Foreign keys are declared and indexed
Every reference is a declared foreign key with an explicit `ON DELETE`: `CASCADE` for children the parent owns, `SET NULL` for a reference to a user from a row that outlives them, which deleting their account anonymizes (DATA-23), and `RESTRICT` otherwise. Two kinds of reference are exceptions, and outlive the rows they name: an audit entry's `resource_id`, which may name a row of any table (DATA-22), and a job's `user_id` and `workspace_id`, which record whom it runs for (DATA-23). Every foreign-key column has an index.
- Why: deletes behave as the schema says, and cascades stay fast.
- Check: `ci: check-schema`.

### DATA-06 Deletes are hard deletes
Rows are deleted, not flagged. No table has a soft-delete column.
- Why: deleted personal data is actually gone (D24, D31).
- Check: `ci: check-schema` (no `deleted_at` or `is_deleted` columns).

### DATA-07 No business logic in SQL
Migrations define no stored procedures and no `SECURITY DEFINER` functions. The only function allowed is the trigger function that maintains `updated_at` and `version` (DATA-02).
- Why: business rules are read, tested and changed in Rust (D4).
- Check: `ci: check-schema` (the schema's one function is the trigger function that the `BEFORE UPDATE` row triggers of the tables outside `platform`'s bookkeeping run, and it isn't `SECURITY DEFINER`).

### DATA-08 The database generates IDs
Inserts take their rows' IDs from the column default, using `RETURNING`. Application code creates no row's ID; a value that names no row, such as an email's `Message-ID`, isn't one.
- Why: one source of IDs (D24).
- Check: `clippy` (`Uuid::new_v4` and `Uuid::now_v7` are in `disallowed_methods`).

## Migrations

### DATA-09 Forward-only migrations in one directory
Migrations live in `migrations/` and run through the sqlx migrator. Each file is named `<timestamp>_<module>_<description>.sql`, where the module is the one that owns what it creates (ARCH-06). A migration in a release, final or prerelease, is never edited or deleted. A migration that no release contains yet may be edited, and every database that applied it is then recreated, because `migrate` and `serve` refuse a database whose applied migration differs from its file (PLATFORM-006, PLATFORM-030). Migration files that tests use to exercise the migrator are test fixtures, not part of the product schema.
- Why: the schema history is linear and verifiable by checksum (D15).
- Check: `ci: check-migrations` (each name holds a module, and no migration that the newest release holds is changed or deleted).

### DATA-10 Expand, then contract
A migration keeps the previous release working. A column or table is removed or renamed only after a release in which no code uses it. The jobs a release enqueues are data the next one reads: a job's kind is renamed or retired only after a release that still runs it and enqueues no more of it, and a payload keeps the fields that the previous release writes, since a waiting job whose kind no replica runs never runs and never dead-letters ([platform spec](../spec/platform.md#the-job-queue)). A keyed operation's stored result is data the next release reads too, for as long as its record lasts: its type keeps the fields that the previous release writes, and adds only fields that may be absent, so a retry that a deploy lets the other release answer still answers the first result (API-12, [platform spec](../spec/platform.md#idempotency)). The access-change signals that one release's replicas send, the other's hubs read while a deploy runs both: a signal keeps the fields that the previous release sends, and adds only fields that the previous release's hub ignores and the new one does without, so that every hub acts on every signal ([platform spec](../spec/platform.md#events-and-the-real-time-hub)).
- Why: rolling deploys run old and new code against one schema, one queue and one channel of signals (D15).
- Check: `review`, plus `test` (a membership signal in the previous release's form).

### DATA-11 Migrations are tested both ways
CI applies every migration to an empty database, and applies the new migrations on top of the previous release's schema.
- Why: fresh installs and upgrades both work.
- Check: `ci: test-migrations`.

## Roles

### DATA-12 Two database roles
| Role | Used by | Attributes |
|---|---|---|
| `owner` | the `migrate` subcommand only | owns the tables it creates, and has `CREATE` on the schema `public`, which the database's owner keeps |
| `app` | every request and every job, and `seed`, which sends the API's routes requests in process ([system spec](../spec/system.md#seed)) | no superuser, owns nothing, data manipulation statements (DML) only (DATA-19) |

The `bootstrap` subcommand creates both roles with these attributes, grants their privileges on the database and sets the role-level timeouts (DATA-19). The privileges on the tables come from the first migration. It runs with an administrator connection: the bundled Postgres superuser, or a managed provider's admin role. Running it again keeps the roles, their attributes and the grants, and sets each role's password again from its URL. Roles belong to the whole Postgres server, so a deployment's server holds its database alone (D75), and in production `bootstrap` refuses a server that holds another database of the roles before it changes one.
- Why: the running API can change data but never the schema, and holds no privilege beyond that (D4).
- Check: `test` (PLATFORM-001, PLATFORM-002, PLATFORM-003), plus `ci: check-schema` (grants).

### DATA-13 Startup refuses an unsafe role
The server refuses to start unless it connects as the role `app`, and that role isn't a superuser, can't create databases or roles, can't start replication, doesn't bypass row-level security, owns nothing in any database of the server, and belongs to no other role.
- Why: a role created by hand with any of those attributes would give the running API far more power than it needs.
- Check: `test` (PLATFORM-001).

### DATA-19 Least privilege in grants
- `EXECUTE` is revoked from `PUBLIC` on every function in the application schema, including through default privileges.
- `app` has `SELECT`, `INSERT`, `UPDATE` and `DELETE` on the application schema only, with no data definition statements (DDL) and no `TRUNCATE`. On `audit_entries` and `audit_entry_clients` it has only `INSERT` and `SELECT` (AUDIT-005), and on each table whose rows are never edited, and which so has no `updated_at` or `version` (DATA-02), no `UPDATE`, such as `device_tokens` (AUTH-019) and `verification_codes` (AUTH-030).
  `export_entries` and `export_chunks` are such tables too (the accounts spec's Owns).
  So are `workspace_files`, `user_files`, `avatars`, `exported_files` and `object_deletions` (the files spec's Owns). <!-- part: files -->
  On a sequence that a mechanism of `platform` draws from, such as `events_position_seq`, it has `USAGE` alone, which takes the next value, and can't set one.
- `app` has `statement_timeout` and `idle_in_transaction_session_timeout` set at the role level.
- Why: a role can do only what its job needs, and a stuck transaction can't hold locks indefinitely.
- Check: `ci: check-schema`.

## Queries

### DATA-14 SQL is checked at compile time
All SQL goes through the sqlx `query!`, `query_as!` and `query_scalar!` macros. The `.sqlx/` offline cache is committed, and CI builds with `SQLX_OFFLINE=true`, from a cache that `just prepare` wrote after the last change to a query or a migration. Optional filters are nullable parameters in a static query (`$1::text IS NULL OR action = $1`). SQL that can't be a macro query is allowed only in these places, each through an `#[expect]` that states its reason: the statements of `platform::db` that take identifiers, such as role statements; the test support (TEST-05); and a dynamic query that a module spec requires, built with `QueryBuilder`, which never concatenates input.
- Why: a query that doesn't match the schema doesn't compile (D24).
- Check: `clippy` (`disallowed_methods` and `disallowed_macros` ban every function, method and macro of sqlx that takes SQL text outside the checked macros, `QueryBuilder`'s among them), `ci: check-sql-bans` (every function, method and macro that the locked sqlx exposes and that takes SQL text outside the checked macros is one that `clippy.toml` bans, so an update of sqlx can't add one unbanned), `ci: sqlx-check` (the cache is what sqlx-cli writes from a database that the migrations build), plus `review` (where an `#[expect]` allows string SQL, and where code names `sqlx::sqlx_macros`, whose `expand_query!` the checked macros expand to, so clippy can't ban it).

### DATA-15 Repositories take a unit of work
A repository function takes `&mut UnitOfWork`, never a pool or connection. Pools, connections and transactions stay inside `platform::db`, and the test support is the one other place that names them (TEST-05).
- Why: every query runs inside the operation's transaction (ARCH-11).
- Check: `clippy` (`disallowed_types` bans sqlx's pools and pool options, its pooled and plain connections, its transactions and its listener, with their Postgres aliases; `disallowed_methods` bans opening a connection, and opening a transaction from one; `platform::db` and the test support allow them through `#[expect]`), plus `review` (where an `#[expect]` allows them).

### DATA-16 Every query filters by tenant itself
Tenancy is enforced by the application, and every query on user or workspace data carries its own filter (D3, D4). Data a module adds for its product lives in a workspace, in a table with a `workspace_id` (D39); a table without one holds account data, keyed by a user, such as `sessions`, or is `workspaces` itself, or is a module's bookkeeping of what it keeps outside the database, which holds no tenant's data. `platform`'s bookkeeping, such as `jobs` and `events`, is platform data whatever its columns: a job's `workspace_id` and `user_id` record whom it runs for, which its handler filters by, while the queue selects jobs by their state; an event's record whom it reaches, which the replay of a stream filters by, while the hub loads events by their position. `check-tenancy` lists the tables of each kind.
- Workspace data is filtered by the workspace ID from the proof the operation runs under: `Authorized<P>`, or the `JobContext` of a job enqueued for a workspace. A repository function takes that ID as a parameter.
- User data is filtered by the acting user's ID. The hub acts for each open stream's caller, whose memberships and sessions it reads again by their IDs (SEC-06).
- A lookup by a presented token's hash, or of a user by an email address, goes through a unique index:
  - before sign-in, by the token or the sign-in email
  - in a password reset, by the address it names (AUTH-034, AUTH-037)
  - when asking to change one's address, by the address asked for, which no account may hold (AUTH-048)
  - when inviting, by the invitee's email <!-- part: invitations -->
  - in the setup subcommand, by its owner's email
  - for a signed URL, of its session by the ID that its signature proves the API gave (PLATFORM-080) <!-- part: files -->
- Rows addressed to a user by an email address are filtered by the caller's email.

A row ID alone never addresses tenant data. These reads are exempt:
- batch reads by IDs that came from a query filtered by tenant in the calling module: of display data, such as names, for example a workspace's member list, or of which of those IDs match a search, as `accounts::matching` and `workspaces::matching` find them (API-20)
- the deletion of an account, which reads whether the workspaces the user owns have other members, by the IDs of the user's own memberships, and deletes those workspaces by them (ACCESS-008, ACCOUNTS-013)
- the export of a user's data, which reads the workspaces whose only member is the user by the IDs of the user's own memberships, and those workspaces' content by them (ACCOUNTS-024)
- the position of the event that a stream's `Last-Event-ID` names, read by the event's ID alone, so a client resumes after the last event it received even from a workspace it has since left; the replay after that position filters by tenant, and the lookup tells a caller only whether an event ID it holds names an event, which a UUIDv7's random bits keep anyone from guessing (REALTIME-004)
- scheduled jobs that span tenants, which select rows by their own criteria, such as expiry
- a public file's download, which reads its row by its ID and its visibility alone, and an avatar's, which reads its row by its ID alone, since whoever holds a public file's ID, or an avatar's, may read it ([files spec](../spec/files.md#visibility)) <!-- part: files -->
- Why: there is no row-level security, so the application's filters are the only guard between tenants, and they are checked by machine and by test (D4).
- Check: `ci: check-tenancy` (every query on a table with a `workspace_id` column, platform data aside, filters on it, on its `user_id` where it has one, or on the address its rows are addressed to, unless it belongs to a listed cross-tenant job or to a listed file of the reads that this rule exempts, and every other table is listed), `test` (TEST-08 sends, for every workspace route, an ID from another workspace the caller belongs to), plus `review` (the exemptions).

### DATA-17 Lists use keyset pagination on an index
A list query filters by the cursor's sort key, with the ID as tiebreaker, and never uses `OFFSET`. An index matches its tenant filter and its order. A `search` (API-20) narrows the rows that index reaches, without an index of its own until a load test asks for one.
- Why: fast at any depth, and stable under inserts (API-06).
- Check: `review`, plus `test` (API-06).

### DATA-18 Optimistic updates check the version
An update made with `If-Match` keeps its `WHERE` to the versions the header names, such as `version = ANY($2)`. Zero rows updated becomes `PreconditionFailed`.
- Why: lost updates are detected in the same statement that would cause them (API-13).
- Check: `test`.

## Transactions

### DATA-20 Read committed, with row locks where the spec says so
Transactions use `READ COMMITTED`, but for the job that builds an export's archive, which reads under `REPEATABLE READ`, through `Database::for_snapshot_job`, so that the archive holds every module's data as of one moment, and which locks its export's row before any other statement, so that its writes, all of rows it inserts or has locked, meet no serialization failure (ACCOUNTS-023). An operation whose correctness depends on rows that another request may change at the same time does one of two things, as its module spec states: it locks them with `SELECT ... FOR UPDATE`, taking locks in a consistent order, or it changes them with an update that checks what it read (DATA-18). Nothing inside the API retries a transaction. A deadlock is a defect, and surfaces as an internal error. A failed job is retried by the job system (PLATFORM-014).
- Why: a unit of work carries the effects of every module the operation called, so no one layer holds what it would need to re-run it. Under `READ COMMITTED` there are no serialization failures, so a deadlock means two operations took locks in different orders: a bug to fix, not to retry.
- Check: `review`.

### DATA-21 No external calls inside a transaction
A transaction never waits on HTTP or the Simple Mail Transfer Protocol (SMTP): external effects are jobs (D1).
Nor does it wait on object storage: uploads write the object before the transaction. <!-- part: files -->
- Why: a slow dependency can't hold database locks.
- Check: `review`.

### DATA-22 Side effects join the unit of work
Audit entries (`audit::record`), events (`uow.publish_to_user` and `uow.publish_to_workspace`) and jobs (`uow.enqueue`) are written in the same unit of work as the change that causes them.

What is audited:
- Every change that a request or an enqueued job makes to data a module owns.
- Each security failure: those D6 lists, a failed sign-in, a wrong current password, a reused refresh token and a refused authorization, and a wrong verification code, a guess at what proves an account's address (AUTH-032) or resets its password (AUTH-038). A failed sign-in commits its audit entry and then answers 401 (ARCH-11, PLATFORM-008). Refusals are reported to the `SecurityEventSink` (PLATFORM-010).
- Reads that a module spec names as audited, such as the download of an export (D31).

Platform bookkeeping (rate-limit buckets, idempotency records, job state) and scheduled maintenance (retention, sweeps) aren't audited. Metrics count them instead. Deleting an account records only the entries that the logs of the workspaces the user shared keep, since their own log goes with the account (ACCOUNTS-014, D31).
- Why: a change and its side effects commit together or not at all (D1, D6).
- Check: `review`, plus `test` (each mutating behavior asserts its audit entry and events).

## Personal data

### DATA-23 Personal data is covered by a contract
Every column that references a user or holds personal data (glossary) is covered by a registered personal data contract (D31). A column that holds personal data carries the column comment `personal_data`, set with `COMMENT ON COLUMN` in its migration.

`platform` registers a contract for its own tables. Erasing a user deletes their idempotency records and the events scoped to them. Jobs hold no foreign keys to user data: erasing a user, or deleting a workspace, deletes the jobs enqueued for them, so no job runs for a subject that is gone. A succeeded job keeps no row, and a dead letter keeps its payload only for its retention (PLATFORM-014). Rate-limit buckets hold only keyed hashes and expire on their own. Export leaves these tables out, because the modules that own the data export it.
- Why: export and erasure can't miss a table, including the platform's own.
- Check: `test` (every foreign key to `users`, and every column commented `personal_data`, in the migrated schema, against the columns of the contracts that the composition root registers).
