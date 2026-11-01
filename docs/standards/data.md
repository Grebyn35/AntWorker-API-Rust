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
Stored timestamps come from `now()` in SQL, never from the application. Expiry checks run in SQL against `now()`.
- Why: one clock, at full precision, so ordering and expiry agree across replicas (D24).
- Check: `review`.

### DATA-04 The database enforces integrity, domain types enforce values
The schema declares keys, uniqueness, foreign keys and `NOT NULL`. Closed sets are Postgres enum types mapped to Rust enums. The rules for a value's length, format or content live only in its domain type (D25), not in `CHECK` constraints.
- Why: each rule is defined once.
- Check: `review`.

### DATA-05 Foreign keys are declared and indexed
Every reference is a declared foreign key with an explicit `ON DELETE`: `CASCADE` for children the parent owns, `RESTRICT` otherwise. The one exception is an audit entry's `resource_id`, which may name a row of any table, and outlives it (DATA-27). Every foreign-key column has an index.
- Why: deletes behave as the schema says, and cascades stay fast.
- Check: `ci: check-schema`.

### DATA-06 Deletes are hard deletes
Rows are deleted, not flagged. No table has a soft-delete column.
- Why: deleted personal data is actually gone (D24, D31).
- Check: `ci: check-schema` (no `deleted_at` or `is_deleted` columns).

### DATA-07 No business logic in SQL
Migrations define no stored procedures and no `SECURITY DEFINER` functions. The only function allowed is the trigger function that maintains `updated_at` and `version` (DATA-02).
- Why: business rules are read, tested and changed in Rust (D4).
- Check: `ci: check-migrations` (`CREATE FUNCTION` only for allowlisted names).

### DATA-08 The database generates IDs
Inserts take their IDs from the column default, using `RETURNING`. Application code creates no IDs.
- Why: one source of IDs (D24).
- Check: `clippy` (`Uuid::new_v4` and `Uuid::now_v7` are in `disallowed_methods`).

## Migrations

### DATA-09 Forward-only migrations in one directory
Migrations live in `migrations/` and run through the sqlx migrator. Each file is named `<timestamp>_<module>_<description>.sql`. A migration in a release is never edited. A migration that no release contains yet may be edited, and every database that applied it is then recreated, because `migrate` and `serve` refuse a database whose applied migration differs from its file (PLATFORM-029, PLATFORM-030). Migration files that tests use to exercise the migrator are test fixtures, not part of the product schema.
- Why: the schema history is linear and verifiable by checksum (D15).
- Check: `ci: check-migrations` (no applied file changed relative to the last final release).

### DATA-10 Expand, then contract
A migration keeps the previous release working. A column or table is removed or renamed only after a release in which no code uses it.
- Why: rolling deploys run old and new code against one schema (D15).
- Check: `ci: check-migrations` (destructive statements are flagged), plus `review`.

### DATA-11 Migrations are tested both ways
CI applies every migration to an empty database, and applies the new migrations on top of the previous release's schema.
- Why: fresh installs and upgrades both work.
- Check: `ci: test-migrations`.

## Roles

### DATA-12 Two database roles
| Role | Used by | Attributes |
|---|---|---|
| `owner` | the `migrate` subcommand only | owns the tables it creates, and has `CREATE` on the schema `public`, which the database's owner keeps |
| `app` | every request and every job | no superuser, owns nothing, data manipulation statements (DML) only (DATA-19) |

The `bootstrap` subcommand creates both roles with these attributes, grants their privileges on the database and sets the role-level timeouts (DATA-19). The privileges on the tables come from the first migration. It runs with an administrator connection: the bundled Postgres superuser, or a managed provider's admin role. Running it again keeps the roles, their attributes and the grants, and sets each role's password again from its URL.
- Why: the running API can change data but never the schema, and holds no privilege beyond that (D4).
- Check: `test` (PLATFORM-009, PLATFORM-010), plus `ci: check-schema` (grants).

### DATA-13 Startup refuses an unsafe role
The server refuses to start unless it connects as the role `app`, and that role isn't a superuser, can't create databases or roles, can't start replication, doesn't bypass row-level security, owns nothing in any database of the server, and belongs to no other role.
- Why: a role created by hand with any of those attributes would give the running API far more power than it needs.
- Check: `test` (PLATFORM-009).

### DATA-19 Least privilege in grants
- `EXECUTE` is revoked from `PUBLIC` on every function in the application schema, including through default privileges.
- `app` has `SELECT`, `INSERT`, `UPDATE` and `DELETE` on the application schema only, with no data definition statements (DDL) and no `TRUNCATE`. On `audit_entries` it has only `INSERT` and `SELECT` (AUDIT-005).
- `app` has `statement_timeout` and `idle_in_transaction_session_timeout` set at the role level.
- Why: a role can do only what its job needs, and a stuck transaction can't hold locks indefinitely.
- Check: `ci: check-schema`.

## Queries

### DATA-20 SQL is checked at compile time
All SQL goes through the sqlx `query!`, `query_as!` and `query_scalar!` macros. The `.sqlx/` offline cache is committed, and CI builds with `SQLX_OFFLINE=true`. Optional filters are nullable parameters in a static query (`$1::text IS NULL OR action = $1`). SQL that can't be a macro query is allowed only in these places, each through an `#[expect]` that states its reason: the statements of `platform::db` that take identifiers, such as role statements; the test support (TEST-06); and a dynamic query that a module spec requires, built with `QueryBuilder`, which never concatenates input.
- Why: a query that doesn't match the schema doesn't compile (D24).
- Check: `clippy` (`disallowed_methods` and `disallowed_macros` ban every function, method and macro of sqlx that takes SQL text outside the checked macros, `QueryBuilder`'s among them), `ci: check-sql-bans` (every function, method and macro that the locked sqlx exposes and that takes SQL text outside the checked macros is one that `clippy.toml` bans, so an update of sqlx can't add one unbanned), `ci: sqlx-check`, plus `review` (where an `#[expect]` allows string SQL, and where code names `sqlx::sqlx_macros`, whose `expand_query!` the checked macros expand to, so clippy can't ban it).

### DATA-21 Repositories take a unit of work
A repository function takes `&mut UnitOfWork`, never a pool or connection. Pools, connections and transactions stay inside `platform::db`, and the test support is the one other place that names them (TEST-06).
- Why: every query runs inside the operation's transaction (ARCH-11).
- Check: `clippy` (`disallowed_types` bans sqlx's pools and pool options, its pooled and plain connections, its transactions and its listener, with their Postgres aliases; `disallowed_methods` bans opening a connection, and opening a transaction from one; `platform::db` and the test support allow them through `#[expect]`), plus `review` (where an `#[expect]` allows them).

### DATA-22 Every query filters by tenant itself
Tenancy is enforced by the application, and every query on user or workspace data carries its own filter (D3, D4). Data a module adds for its product lives in a workspace, in a table with a `workspace_id` (D39); a table without one holds account data, keyed by a user, such as `sessions`, or platform data, such as `jobs`, and `check-tenancy` lists which.
- Workspace data is filtered by the workspace ID from the proof the operation runs under: `Authorized<P>`, or the `JobContext` of a job enqueued for a workspace. A repository function takes that ID as a parameter.
- User data is filtered by the acting user's ID.
- A lookup by a presented token's hash, or of a user by an email address, goes through a unique index: before sign-in, by the token or the sign-in email; when inviting, by the invitee's email; and in the setup subcommand, by its owner's email.
- An invitee's own invitations are filtered by the caller's email.

A row ID alone never addresses tenant data. Two kinds of read are exempt:
- batch reads of display data, such as names, by IDs that came from a query filtered by tenant in the calling module, for example a workspace's member list
- scheduled jobs that span tenants, which select rows by their own criteria, such as expiry
- Why: there is no row-level security, so the application's filters are the only guard between tenants, and they are checked by machine and by test (D4).
- Check: `ci: check-tenancy` (every query on a table with a `workspace_id` column filters on it, unless it belongs to a listed cross-tenant job, and every table without one is listed as account or platform data), `test` (TEST-09 sends, for every workspace route, an ID from another workspace the caller belongs to), plus `review` (the exemptions).

### DATA-23 Lists use keyset pagination on an index
A list query filters by the cursor's sort key, with the ID as tiebreaker, and never uses `OFFSET`. An index matches its tenant filter and its order. A `search` (API-20) narrows the rows that index reaches, without an index of its own until a load test asks for one.
- Why: fast at any depth, and stable under inserts (API-06).
- Check: `review`, plus `test` (API-06).

### DATA-24 Optimistic updates check the version
An update made with `If-Match` includes `WHERE version = $expected`. Zero rows updated becomes `PreconditionFailed`.
- Why: lost updates are detected in the same statement that would cause them (API-13).
- Check: `test`.

## Transactions

### DATA-25 Read committed, with row locks where the spec says so
Transactions use `READ COMMITTED`. An operation whose correctness depends on rows that another request may change at the same time does one of two things, as its module spec states: it locks them with `SELECT ... FOR UPDATE`, taking locks in a consistent order, or it changes them with an update that checks what it read (DATA-24). Nothing inside the API retries a transaction. A deadlock is a defect, and surfaces as an internal error. A failed job is retried by the job system (OPS-10).
- Why: a unit of work carries the effects of every module the operation called, so no one layer holds what it would need to re-run it. Under `READ COMMITTED` there are no serialization failures, so a deadlock means two operations took locks in different orders: a bug to fix, not to retry.
- Check: `review`.

### DATA-26 No external calls inside a transaction
A transaction never waits on HTTP, the Simple Mail Transfer Protocol (SMTP) or object storage. Uploads write the object before the transaction, and external effects are jobs (D1).
- Why: a slow dependency can't hold database locks.
- Check: `review`.

### DATA-27 Side effects join the unit of work
Audit entries (`audit::record`), events (`uow.publish`) and jobs (`uow.enqueue`) are written in the same unit of work as the change that causes them.

What is audited:
- Every change that a request or an enqueued job makes to data a module owns.
- Each security failure D6 lists: a failed sign-in, a wrong current password, a reused refresh token, and a refused authorization. A failed sign-in commits its audit entry and then answers 401 (ARCH-11, PLATFORM-006). Refusals are reported to the `SecurityEventSink` (PLATFORM-025).
- Reads that a module spec names as audited, such as the download of a data export (D31).

Platform bookkeeping (rate-limit buckets, idempotency records, job state) and scheduled maintenance (retention, sweeps) aren't audited. Metrics count them instead.
- Why: a change and its side effects commit together or not at all (D1, D6).
- Check: `review`, plus `test` (each mutating behavior asserts its audit entry and events).

## Personal data

### DATA-28 Personal data is covered by a contract
Every column that references a user or holds personal data (glossary) is covered by a registered personal data contract (D31). A column that holds personal data carries the column comment `personal_data`, set with `COMMENT ON COLUMN` in its migration.

`platform` registers a contract for its own tables. Erasing a user deletes their idempotency records and the events scoped to them. Jobs hold no foreign keys to user data: a pending job still runs, and its handler does nothing when its subject has been deleted. A finished job keeps no payload (OPS-10). Rate-limit buckets hold only keyed hashes and expire on their own. Export leaves these tables out, because the modules that own the data export it.
- Why: export and erasure can't miss a table, including the platform's own.
- Check: `ci: check-personal-data` (every foreign key to `users`, and every column commented `personal_data`, against the registered contracts).
