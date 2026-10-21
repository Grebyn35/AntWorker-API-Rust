# Inventory: Data layer (`DATA`)

Reference: `AntWorker-API` @ `0210895` (2026-10-20). This file records what the old API does. It does not prescribe what the new one should do.

## Scope

**Covered.** The PostgreSQL schema (every table, column, constraint, index and foreign-key action), the migration mechanism and the `migrate` command, database roles and grants, the row-level-security model and every policy, the paths that bypass RLS, the full stored-procedure catalog, triggers, the background tasks whose bodies are data maintenance (`auth_cleanup`, `rate_limit_cleanup`, `idempotency_cleanup`, `re_encryption`), the repository layer (traits, transaction helpers, keyset SQL, error classification, retries, connection pool, pool gauge), and field-level encryption at rest with key rotation.

**Boundaries.**
- The business meaning of each `sp_public_*` procedure (login, refresh, unlink, email change, invitation acceptance, ...) belongs to the domain whose endpoint calls it (AUTH, USER, WS, FILE). Here they appear only in the `## Stored procedure catalog` with their caller.
- RT owns the background-task runner, the distributed lock, task shutdown, the `sse_events` notify trigger (RT-022) and SSE retention (RT-025). This file only references them.
- FILE owns the object-deletion capture trigger's behavior (FILE-016) and the reaper (FILE-035 to FILE-038). AUDIT owns audit immutability, audit RLS and audit retention (AUDIT-018 to AUDIT-021). USER owns the account-deletion cascade list (USER-030). They are referenced, not repeated.
- HTTP owns the semantics of `rate_limits` and `idempotency_cache` and the pagination contract; DATA owns their tables and cleanup sweeps. OPS owns Compose, `deploy.sh` and the test harness; DATA records only the migration/bootstrap ordering.

**Files read in full:** `migrations/0001_extensions.sql` through `migrations/0008_grants.sql` (all of `0005_stored_procedures.sql`), `src/migrate.rs`, `src/cli.rs`, `src/repositories/mod.rs`, `src/repositories/postgresql/{mod,transaction,rls_context,keyset}.rs`, `src/encryption/{mod,key_manager,database_encryption,re_encryption}.rs`, `src/background.rs`, `src/utils/db_pool_metrics.rs`, `src/config/database.rs`, `scripts/db-bootstrap.sh`, `build.rs`, `documentation/DATABASE.md`, `tests/lint_definer_sp_scoping_coverage.rs`, `tests/repository_error_classification.rs`.

**Read in part:**
- `src/repositories/traits.rs` (trait and method list, `Repository<T>`, `TransactionalRepository<T>`, Combined traits); `src/utils/database_retry.rs:1-235`; `src/utils/retry.rs:76-217`; `src/utils/distributed_lock.rs`; `src/errors/repository_error.rs:224-246,831-960`; `src/errors/api_error.rs:688-800`.
- Every file in `src/repositories/postgresql/` for its SQL statements, transaction helper and SP calls; in detail `public_user_repository.rs:15-250`, `public_session_repository.rs:228-393`, `public_verification_code_repository.rs:180-200`, `user_repository.rs:36-664`, `workspace_repository.rs:182-280,985-1060`, `session_repository.rs:155-395`, `workspace_member_repository.rs:925-1200,1403-1470`, `user_audit_log_repository.rs:186-262,405-475`, `workspace_audit_log_repository.rs:236-300,443-530`, `object_deletion_queue_repository.rs:85-130`, `background_task_lock_repository.rs:95-116`, `user_preferences_repository.rs:195-215`, `sse_event_repository.rs:112-140`, `sse_notification_repository.rs:426-500`.
- `src/server.rs:745-800,1090-1195,1475-2240`; `src/config/environment.rs:340-590,650-735,900-950,1130-1165`; `src/config/server.rs:248-262`; `src/config/storage.rs:140-200`; `src/services/public_auth_service.rs:390-560,1540-1630,1800-1815,2285-2300`; `src/services/workspace_service.rs:250-300`; `src/services/object_deletion_reaper_service.rs:52-96`; `src/auth/verification_code_validator.rs:70-75`.
- `docker-compose.yml` (api, postgres, db-bootstrap, db-migrate services), `scripts/deploy.sh` (header and one-shot handling), `.env.example:186-230,1325-1360`, `documentation/DEPLOYMENT.md:212-225`.
- Tests: `tests/rls_isolation.rs` (helpers and every test), `tests/re_encryption_{all_tables,cas_guard,cycle}.rs`, `tests/repository_smoke.rs`, `tests/repository_column_scoping_live.rs`, `tests/migrate_command.rs`, `tests/object_deletion_trigger.rs` (headers and test lists); a grep of `tests/` for constraint names.
- Sibling inventories `02-user-account.md`, `04-files.md`, `05-audit.md`, `06-realtime.md` (to avoid duplicate cataloguing).

`.env` and `alertmanager-secrets/` exist and were not opened.

## Schema

Conventions that hold for every table unless noted:
- Primary keys are `UUID DEFAULT uuid_generate_v4()` from the `uuid-ossp` extension (`migrations/0001_extensions.sql:6`). There are no sequences.
- Timestamps are `TIMESTAMP WITH TIME ZONE`. `created_at` and `updated_at` default to `NOW()`. No trigger maintains `updated_at`; the application or a procedure sets it on each write.
- "ciphertext" means the column holds `base64(version || nonce || AES-256-GCM ciphertext || tag)` written by the application (DATA-042). "HMAC" means a 64-hex-char HMAC-SHA256 blind index (DATA-044). The database never sees plaintext or keys.
- No column has a length limit other than those shown. No CHECK constrains `method`, `auth_method`, `purpose`, `oauth_provider`, `event_type`, the shape of `permissions` JSONB, or email format.

### `users` — owner USER (identity anchor shared with AUTH) — RLS on

| Column | Type | Null | Default | Notes |
|---|---|---|---|---|
| `id` | UUID | no | `uuid_generate_v4()` | PK |
| `email` | TEXT | no | | ciphertext |
| `name` | TEXT | no | | ciphertext |
| `avatar_file_id` | UUID | yes | | FK `user_files(id)` ON DELETE SET NULL |
| `email_hash` | VARCHAR(64) | no | | HMAC of the normalized email |
| `email_verified` | BOOLEAN | no | FALSE | |
| `created_at`, `updated_at` | TIMESTAMPTZ | no | NOW() | |

Constraints: `users_email_hash_unique UNIQUE(email_hash)`. Indexes: `idx_users_avatar_file_id (avatar_file_id) WHERE avatar_file_id IS NOT NULL`. Source: `migrations/0002_tables.sql:16-37,672`, `migrations/0003_indexes.sql:118-120`.

### `user_identities` — owner AUTH — RLS on

| Column | Type | Null | Default | Notes |
|---|---|---|---|---|
| `id` | UUID | no | uuid | PK |
| `user_id` | UUID | no | | FK `users` ON DELETE CASCADE, DEFERRABLE INITIALLY DEFERRED |
| `method` | TEXT | no | | `credentials` or an OAuth provider id |
| `provider_subject` | TEXT | yes | | plaintext provider `sub` / numeric id |
| `password_hash` | TEXT | yes | | Argon2id hash (not app-encrypted) |
| `created_at`, `updated_at` | TIMESTAMPTZ | no | NOW() | |

Constraints: `user_identities_user_method_unique UNIQUE(user_id, method)`; `user_identities_provider_subject_unique UNIQUE(method, provider_subject)`; `user_identities_shape CHECK ((method='credentials' AND password_hash IS NOT NULL AND provider_subject IS NULL) OR (method<>'credentials' AND password_hash IS NULL AND provider_subject IS NOT NULL))`. Indexes: only the two UNIQUE backing indexes. Source: `migrations/0002_tables.sql:56-87,648`, `migrations/0003_indexes.sql:51-59`.

### `sessions` — owner AUTH — RLS on

| Column | Type | Null | Default | Notes |
|---|---|---|---|---|
| `id` | UUID | no | uuid | PK; replaced on re-login (DATA catalog `sp_public_upsert_session`) |
| `user_id` | UUID | no | | FK `users` ON DELETE CASCADE (not deferred) |
| `access_token_hash` | VARCHAR(64) | no | | HMAC, UNIQUE |
| `refresh_token_hash` | VARCHAR(64) | no | | HMAC, UNIQUE |
| `previous_refresh_token_hash` | VARCHAR(64) | yes | | HMAC of the last rotated-out refresh token, not unique |
| `auth_method` | TEXT | no | | |
| `device_fingerprint` | VARCHAR(64) | no | | client-supplied plaintext |
| `device_name` | TEXT | yes | | ciphertext |
| `created_at` | TIMESTAMPTZ | no | NOW() | |
| `expires_at` | TIMESTAMPTZ | no | | access-token expiry |
| `refresh_token_expires_at` | TIMESTAMPTZ | no | | session liveness boundary |

No `updated_at`. Constraints: `sessions_user_device_unique UNIQUE(user_id, device_fingerprint)`, `sessions_access_token_hash_unique`, `sessions_refresh_token_hash_unique`. Indexes: `idx_sessions_user (user_id)`, `idx_sessions_expires (expires_at)`, `idx_sessions_refresh_token_expires_at`, `idx_sessions_device_fingerprint`, `idx_sessions_previous_refresh_token_hash WHERE previous_refresh_token_hash IS NOT NULL`. Source: `migrations/0002_tables.sql:91-133,645`, `migrations/0003_indexes.sql:33-45`.

### `oauth_tokens` — owner AUTH — RLS on

| Column | Type | Null | Default | Notes |
|---|---|---|---|---|
| `id` | UUID | no | uuid | PK |
| `user_id` | UUID | no | | FK `users` ON DELETE CASCADE |
| `session_id` | UUID | no | | FK `sessions` ON DELETE CASCADE **ON UPDATE CASCADE** |
| `oauth_provider` | TEXT | no | | |
| `provider_access_token_encrypted` | TEXT | no | | ciphertext |
| `provider_token_expires_at` | TIMESTAMPTZ | yes | | NULL for never-expiring tokens (GitHub) |
| `created_at`, `updated_at` | TIMESTAMPTZ | no | NOW() | |

Constraints: `oauth_tokens_session_unique UNIQUE(session_id)`. Indexes: `idx_oauth_tokens_user`. The ON UPDATE CASCADE makes the token row follow a session whose `id` is replaced by a re-login upsert. Source: `migrations/0002_tables.sql:144-158,653-654`, `migrations/0003_indexes.sql:49`.

### `verification_codes` — owner AUTH (email verification, password reset), USER (email change) — RLS on

Columns: `id` UUID PK; `user_id` UUID NOT NULL FK `users` ON DELETE CASCADE; `purpose` VARCHAR(32) NOT NULL (`email_verification`, `password_reset`, `email_change` by convention, no CHECK); `code_hash` VARCHAR(64) NOT NULL (HMAC of the 6-digit code); `attempts_used` INTEGER NOT NULL DEFAULT 0; `max_attempts` INTEGER NOT NULL DEFAULT 5; `created_at` DEFAULT NOW(); `expires_at` NOT NULL. No `updated_at`. Constraints: `verification_codes_user_purpose_unique UNIQUE(user_id, purpose)`. Indexes: `idx_verification_codes_expires_at`. Source: `migrations/0002_tables.sql:166-187,655`, `migrations/0003_indexes.sql:24`.

### `pending_email_changes` — owner USER — RLS on

Columns: `id` UUID PK; `user_id` UUID NOT NULL FK `users` ON DELETE CASCADE; `new_email` TEXT NOT NULL (ciphertext); `new_email_hash` VARCHAR(64) NOT NULL (HMAC); `created_at` DEFAULT NOW(); `expires_at` NOT NULL. Constraints: `pending_email_changes_user_unique UNIQUE(user_id)`. Indexes: `idx_pending_email_changes_expires_at`. Source: `migrations/0002_tables.sql:201-216,656`, `migrations/0003_indexes.sql:30`.

### `workspaces` — owner WS — RLS on

Columns: `id` UUID PK; `name` TEXT NOT NULL (ciphertext); `owner_id` UUID NOT NULL FK `users` ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED; `picture_file_id` UUID NULL FK `workspace_files(id)` ON DELETE SET NULL; `created_at`, `updated_at`. Indexes: `idx_workspaces_owner`, `idx_workspaces_picture_file_id WHERE picture_file_id IS NOT NULL`. Source: `migrations/0002_tables.sql:222-235,644,681`, `migrations/0003_indexes.sql:67,126-128`.

### `workspace_memberships` — owner WS — RLS on

Columns: `id` UUID PK; `user_id` UUID NOT NULL FK `users` ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED; `workspace_id` UUID NOT NULL FK `workspaces` ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED; `permissions` JSONB NOT NULL (array of permission strings, shape unchecked); `created_at`, `updated_at`. Constraints: `workspace_memberships_user_workspace_unique UNIQUE(user_id, workspace_id)`. Indexes: `idx_workspace_memberships_user`, `idx_workspace_memberships_workspace`, `idx_workspace_memberships_workspace_created (workspace_id, created_at DESC)`. Source: `migrations/0002_tables.sql:238-246,649-650`, `migrations/0003_indexes.sql:70-73`.

### `workspace_invitations` — owner WS — RLS on

Columns: `id` UUID PK; `workspace_id` UUID NOT NULL FK `workspaces` ON DELETE CASCADE; `email` TEXT NOT NULL (ciphertext); `email_hash` VARCHAR(64) NOT NULL (HMAC); `permissions` JSONB NOT NULL; `invited_by` UUID NOT NULL FK `users` ON DELETE CASCADE; `invited_at` DEFAULT NOW(); `updated_at` DEFAULT NOW(); `expires_at` NOT NULL. There is no status column: a row's existence means "pending". Constraints: `workspace_invitations_workspace_email_unique UNIQUE(workspace_id, email_hash)`. Indexes: `idx_workspace_invitations_workspace`, `_email_hash`, `_expires`, `_workspace_invited (workspace_id, invited_at DESC)`. Source: `migrations/0002_tables.sql:253-266,651-652`, `migrations/0003_indexes.sql:76-80`.

### `user_preferences` — owner USER — RLS on

Columns: `user_id` UUID PK FK `users` ON DELETE CASCADE; `default_workspace_id` UUID NULL FK `workspaces` ON DELETE SET NULL; `telemetry_consent` BOOLEAN NOT NULL DEFAULT FALSE; `created_at`, `updated_at`. Indexes: `idx_user_preferences_default_workspace`. Source: `migrations/0002_tables.sql:269-278`, `migrations/0003_indexes.sql:65`.

### `user_files` — owner FILE — RLS on

Columns: `id` UUID PK; `user_id` UUID NOT NULL FK `users` ON DELETE CASCADE; `storage_key` TEXT NOT NULL; `content_type` TEXT NOT NULL; `file_name` TEXT NOT NULL (ciphertext); `size_bytes` BIGINT NOT NULL; `visibility` TEXT NOT NULL DEFAULT `'private'`; `created_at`, `updated_at`. Constraints: `user_files_storage_key_unique UNIQUE(storage_key)`, `user_files_visibility_check CHECK (visibility IN ('private','public'))`, `user_files_size_bytes_non_negative CHECK (size_bytes >= 0)`. Indexes: `idx_user_files_user_created (user_id, created_at DESC)`. Trigger: `user_files_enqueue_deletion`. Source: `migrations/0002_tables.sql:311-334,664`, `migrations/0003_indexes.sql:109`, `migrations/0007_triggers.sql:63-66`.

### `workspace_files` — owner FILE — RLS on

Same columns and constraints as `user_files` (constraint names prefixed `workspace_files_`), with `workspace_id` UUID NOT NULL FK `workspaces` ON DELETE CASCADE instead of `user_id`, plus `uploaded_by` UUID NULL FK `users` ON DELETE SET NULL. Indexes: `idx_workspace_files_workspace_created (workspace_id, created_at DESC)`, `idx_workspace_files_uploaded_by`. Trigger: `workspace_files_enqueue_deletion`. Source: `migrations/0002_tables.sql:336-355,665-666`, `migrations/0003_indexes.sql:110-112`, `migrations/0007_triggers.sql:68-71`.

### `workspace_audit_logs` — owner AUDIT — RLS on

Columns: `id` UUID PK; `workspace_id` UUID NOT NULL (no FK); `user_id` UUID NOT NULL (no FK); `action` VARCHAR(50) NOT NULL; `resource_type` VARCHAR(100) NOT NULL; `resource_id` UUID NULL; `old_value`, `new_value`, `metadata` TEXT NULL (ciphertext of JSON); `trace_id` VARCHAR(32) NOT NULL; `created_at` DEFAULT NOW(). Indexes: `(workspace_id, created_at DESC)`, `(user_id)`, `(resource_type, resource_id)`, `(action)`, `(created_at)`. Source: `migrations/0002_tables.sql:376-388,414-416`, `migrations/0003_indexes.sql:88-93`.

### `user_audit_logs` — owner AUDIT — RLS on

As `workspace_audit_logs` without `workspace_id`. Indexes: `(user_id, created_at DESC)`, `(resource_type, resource_id)`, `(action)`, `(created_at)`. Source: `migrations/0002_tables.sql:401-412`, `migrations/0003_indexes.sql:97-101`.

### `idempotency_cache` — owner HTTP — no RLS

Columns: `composite_key` TEXT PK (`{key}:{user_id}:{workspace_id|user-scoped}:{method}:{path}`); `status` TEXT NOT NULL DEFAULT `'completed'` CHECK IN (`processing`,`completed`); `original_trace_id` VARCHAR(32) NOT NULL; `status_code` INTEGER NULL CHECK (NULL or 200..599); `response_body` TEXT NULL (ciphertext); `request_body_hash` TEXT NOT NULL; `response_headers` JSONB NOT NULL DEFAULT `'{}'`; `created_at` DEFAULT NOW(). Storage parameter `autovacuum_enabled = true`. Indexes: `idx_idempotency_cache_created_at`. Created with `IF NOT EXISTS`. Source: `migrations/0002_tables.sql:444-501`, `migrations/0003_indexes.sql:143-144`.

### `sse_events` — owner RT — no RLS

Columns: `id` UUID PK; `event_type` TEXT NOT NULL; `scope_type` TEXT NOT NULL CHECK IN (`workspace`,`user`) (`sse_events_scope_type_check`); `scope_id` UUID NOT NULL (no FK); `payload` JSONB NOT NULL (plaintext); `trace_id` VARCHAR(32) NULL; `created_at` TIMESTAMPTZ NOT NULL DEFAULT NOW(). Indexes: `idx_sse_events_created_at`. Trigger: `sse_event_notify_trigger` (RT-022). Source: `migrations/0002_tables.sql:517-543`, `migrations/0003_indexes.sql:149`.

### `rate_limits` — owner HTTP — no RLS

Columns: `key` TEXT PK (`user:{id}`, `ip:{addr}`, `credential_login:{email_hash}`, ...); `count` INTEGER NOT NULL DEFAULT 1; `window_start` TIMESTAMPTZ NOT NULL DEFAULT NOW(); `window_secs` INTEGER NOT NULL. Indexes: `idx_rate_limits_window_start`. Created with `IF NOT EXISTS`. Source: `migrations/0002_tables.sql:561-574`, `migrations/0003_indexes.sql:152`.

### `background_task_locks` — owner RT — no RLS

Columns: `task_id` TEXT PK; `instance_id` TEXT NOT NULL; `locked_at` DEFAULT NOW(); `lock_expires_at` NOT NULL. Created with `IF NOT EXISTS`. Source: `migrations/0002_tables.sql:592-608`.

### `pending_object_deletions` — owner FILE — no RLS

Columns: `id` UUID PK; `storage_key` TEXT NOT NULL (not unique); `enqueued_at` DEFAULT NOW(); `attempts` INTEGER NOT NULL DEFAULT 0; `last_attempt_at` TIMESTAMPTZ NULL; `last_error` TEXT NULL. No FK, by design. Indexes: `idx_pending_object_deletions_claim (last_attempt_at NULLS FIRST, enqueued_at)`. Created with `IF NOT EXISTS`. Source: `migrations/0002_tables.sql:623-636`, `migrations/0003_indexes.sql:133-134`.

### `_sqlx_migrations` — owner DATA/OPS — no RLS

sqlx's bookkeeping table (version, description, installed_on, success, checksum, execution_time), created by the migrator in the target schema before the first migration runs. Source: `src/migrate.rs:21-22,119-127`.

### Foreign-key action summary

| Child column | Parent | On delete | Deferrable |
|---|---|---|---|
| `workspaces.owner_id` | `users` | CASCADE | initially deferred |
| `sessions.user_id` | `users` | CASCADE | no |
| `user_identities.user_id` | `users` | CASCADE | initially deferred |
| `workspace_memberships.user_id` | `users` | CASCADE | initially deferred |
| `workspace_memberships.workspace_id` | `workspaces` | CASCADE | initially deferred |
| `workspace_invitations.workspace_id` | `workspaces` | CASCADE | no |
| `workspace_invitations.invited_by` | `users` | CASCADE | no |
| `oauth_tokens.user_id` | `users` | CASCADE | no |
| `oauth_tokens.session_id` | `sessions` | CASCADE, ON UPDATE CASCADE | no |
| `verification_codes.user_id` | `users` | CASCADE | no |
| `pending_email_changes.user_id` | `users` | CASCADE | no |
| `user_preferences.user_id` | `users` | CASCADE | no |
| `user_preferences.default_workspace_id` | `workspaces` | SET NULL | no |
| `user_files.user_id` | `users` | CASCADE | no |
| `workspace_files.workspace_id` | `workspaces` | CASCADE | no |
| `workspace_files.uploaded_by` | `users` | SET NULL | no |
| `users.avatar_file_id` | `user_files` | SET NULL | no |
| `workspaces.picture_file_id` | `workspace_files` | SET NULL | no |

No FK: both audit tables, `sse_events.scope_id`, `pending_object_deletions`, `idempotency_cache`, `rate_limits`, `background_task_locks`. Source: `migrations/0002_tables.sql:269-271,638-681`.

## Behaviors

### DATA-001 `migrate` applies the embedded migrations

- **Behavior:** `app-api migrate` connects to `DATABASE_URL`, applies every migration compiled into the binary that the database has not recorded, in ascending version order, records each in `_sqlx_migrations`, logs one outcome line and exits 0. With nothing to apply it is a no-op and still exits 0.
- **Kind:** `capability` (operators run it; Compose runs it as `db-migrate`)
- **Trigger:** CLI `app-api migrate` (Compose service `db-migrate`, `command: ["migrate"]`).
- **Auth:** Whoever holds the admin DSN.
- **Input & validation:** No arguments; any extra argument is rejected by clap (`InvalidSubcommand` / `UnknownArgument`). Migrations are embedded with `sqlx::migrate!("./migrations")`; `build.rs` reruns the build when `migrations/` changes.
- **Effects:** DDL from `migrations/0001`..`0008`. Log `Database migrations applied` at info with `component=infrastructure`, `subsystem=migrations`, `applied_count`, `applied_versions` (only this run's), `embedded_count`. JSON console logs, filter `app_api=info`, no OTel.
- **Errors:** Exit code 1 with `Database migrations failed` at error for every failure in DATA-002, DATA-005, DATA-006.
- **Guarantees:** `applied_versions` is the set difference of recorded versions before and after the run, computed under the migration advisory lock (DATA-003).
- **Source:** `src/cli.rs:15-29`, `src/migrate.rs:83-164`, `build.rs:3-5`, `docker-compose.yml:165-178`
- **Tests:** `tests/migrate_command.rs::{empty_database_receives_every_embedded_migration, rerun_applies_nothing_and_changes_nothing, binary_migrates_with_only_database_url, binary_rejects_an_unknown_argument}`; `src/migrate.rs::tests::embedded_migrations_match_the_directory`; `src/cli.rs::tests::*`.

### DATA-002 Applied migrations are immutable

- **Behavior:** If an applied migration's checksum differs from the embedded file, the run aborts before applying anything newer, with the message `migration N differs from the copy already applied: applied migrations are immutable, so restore that file and put the change in a new migration`.
- **Kind:** `rule`
- **Trigger:** DATA-001 against a database whose recorded checksum for version N differs.
- **Errors:** `MigrateError::VersionMismatch(N)` → `MigrationError::Modified(N)` → exit 1.
- **Source:** `src/migrate.rs:56-73`
- **Tests:** `tests/migrate_command.rs::edited_applied_migration_is_refused`; `src/migrate.rs::tests::a_modified_migration_names_its_version`.

### DATA-003 Concurrent `migrate` runs apply each migration once

- **Behavior:** The command takes sqlx's session-level advisory lock before reading the applied set and holds it across the migrator run and the second read, so two concurrent runs serialize; the second applies nothing.
- **Kind:** `guarantee`
- **Trigger:** Two `migrate` processes against the same database.
- **Source:** `src/migrate.rs:104-112`
- **Tests:** `tests/migrate_command.rs::concurrent_runs_apply_each_migration_once`.

### DATA-004 Versions the binary does not embed are ignored

- **Behavior:** `set_ignore_missing(true)`: versions recorded in `_sqlx_migrations` but not embedded (applied by a newer release) do not fail the run, so an older binary can run `migrate` after a rollback.
- **Kind:** `rule`
- **Trigger:** DATA-001 with an older binary.
- **Source:** `src/migrate.rs:83-87`
- **Tests:** `tests/migrate_command.rs::migration_recorded_by_a_newer_release_is_ignored`.

### DATA-005 A failing migration rolls back and fails the command

- **Behavior:** A migration whose SQL errors is not recorded and leaves no partial DDL (sqlx runs each migration file in its own transaction by default; no file opts out). The command exits 1.
- **Kind:** `guarantee`
- **Trigger:** DATA-001 with a failing migration.
- **Errors:** `MigrationError::Migrate` → exit 1.
- **Source:** `src/migrate.rs:62-73,108`
- **Tests:** `tests/migrate_command.rs::{failing_migration_is_an_error_and_rolls_back, binary_reports_a_failed_migration_and_exits_nonzero}`.

### DATA-006 `migrate` reads only `DATABASE_URL`

- **Behavior:** The command reads no other configuration (not the app's `Config`, not encryption keys). An unset or empty value fails with `DATABASE_URL is not set: ...`; an unparsable one with `DATABASE_URL is not a valid PostgreSQL connection string`; a connect that does not finish within 30 s with `could not connect ... within 30s`; a refused connect with `could not connect to the database in DATABASE_URL: ...`. libpq options such as `sslmode` are honored as given.
- **Kind:** `rule`
- **Trigger:** DATA-001.
- **Errors:** `MissingDatabaseUrl`, `InvalidDatabaseUrl`, `ConnectTimeout`, `Connect` → exit 1.
- **Source:** `src/migrate.rs:18-54,89-102,137-140`
- **Tests:** `src/migrate.rs::tests::{unset_or_empty_database_url_is_missing, malformed_database_url_is_invalid, database_url_options_are_used_as_given}`; `tests/migrate_command.rs::{binary_without_database_url_exits_nonzero, binary_that_cannot_connect_exits_nonzero}`.

### DATA-007 Schema changes run only in the one-shot, before replicas start

- **Behavior:** The API server never applies migrations. In Compose the order is `db-bootstrap` (creates the role) → `db-migrate` (applies schema as the admin, `DATABASE_URL=${MIGRATE_DATABASE_URL}`) → `api` (`depends_on: db-migrate: service_completed_successfully`). `deploy.sh` force-recreates both one-shots after refreshing the image so they run the new image and `.env`; new replicas start after the migrations while old replicas keep serving.
- **Kind:** `guarantee`
- **Trigger:** `docker compose up`, `./scripts/deploy.sh`.
- **Effects:** The runtime container's environment blanks `POSTGRES_ADMIN_PASSWORD`, `MIGRATE_DATABASE_URL` and `TEST_DATABASE_URL`, so the API does not hold admin credentials.
- **Guarantees:** No replica sees a half-migrated schema at startup. Nothing checks that a migration is compatible with the replicas still running the previous release.
- **Source:** `docker-compose.yml:39-60,145-178`, `scripts/deploy.sh:383-395`, `src/server.rs:1100` (pool only, no migrator)
- **Tests:** None found in the files read (OPS owns `tests/deploy/`).

### DATA-008 Runtime role bootstrap

- **Behavior:** `db-bootstrap.sh` waits up to 60 × 2 s for `MIGRATE_DATABASE_URL` to accept connections, creates role `app_user` with `NOLOGIN` if it does not exist, then runs `ALTER ROLE app_user LOGIN PASSWORD :'app_password'` from `APP_DB_PASSWORD`. Re-running rotates the password.
- **Kind:** `capability`
- **Trigger:** Compose one-shot `db-bootstrap` (every `up` and every deploy).
- **Input & validation:** Fails with `FATAL:` when `MIGRATE_DATABASE_URL` or `APP_DB_PASSWORD` is empty; after 60 failed probes it prints the last psql error and exits 1.
- **Guarantees:** The password is passed through psql variable quoting. Attributes of a pre-existing `app_user` other than LOGIN and PASSWORD are left as they are.
- **Source:** `scripts/db-bootstrap.sh:13-65`, `docker-compose.yml:147-160`
- **Tests:** None found.

### DATA-009 Runtime role privileges

- **Behavior:** The API connects as `app_user`, which is not the table owner and is created without BYPASSRLS, so RLS applies to it. Migration `0008` grants it USAGE on schema `public`, SELECT/INSERT/UPDATE/DELETE on every table, USAGE on all sequences, the same as default privileges for future tables and sequences, and EXECUTE on every function by name. CREATE on `public` is revoked from `PUBLIC`.
- **Kind:** `guarantee`
- **Trigger:** Migration `0008_grants.sql`.
- **Effects:** No TRUNCATE, REFERENCES or TRIGGER grants; no DDL rights.
- **Guarantees:** Every write the app performs on an RLS table passes through a policy unless it goes through a SECURITY DEFINER function (DATA-018). FORCE ROW LEVEL SECURITY is not set, so the table owner (the migration role; `postgres` superuser in the bundled stack) bypasses RLS.
- **Source:** `migrations/0008_grants.sql:1-168`, `migrations/0004_row_level_security.sql:15-24`, `scripts/db-bootstrap.sh:52-62`
- **Tests:** `tests/rls_isolation.rs` (positive controls in every write/delete test prove the grants).

### DATA-010 SECURITY DEFINER functions pin `search_path`

- **Behavior:** Every SECURITY DEFINER function is declared with `SET search_path = pg_catalog, public`, so unqualified names in its body resolve only there, whatever the caller's `search_path`. Together with the CREATE revoke on `public` (DATA-009) this blocks shadow-object attacks on the elevated functions.
- **Kind:** `hardening`
- **Trigger:** Every call to a SECURITY DEFINER function.
- **Source:** `migrations/0004_row_level_security.sql:60-68`, `migrations/0005_stored_procedures.sql:24-32`, `migrations/0006_background_tasks.sql:18-24`, `migrations/0008_grants.sql:18-31`
- **Tests:** None found (`tests/lint_definer_sp_scoping_coverage.rs` lists DEFINER functions but does not check the attribute).

### DATA-011 Per-transaction RLS context

- **Behavior:** Every authenticated repository operation opens a transaction and first runs `SELECT set_user_request_context($user)` (sets `app.current_user_id`, sets `app.current_workspace_id` to `''`) or `SELECT set_request_context($user, $workspace)` (sets both). Both use `set_config(..., is_local = true)`, so the context ends at commit or rollback and does not leak to the next use of the pooled connection.
- **Kind:** `guarantee`
- **Trigger:** Every call through `with_user_context` / `with_workspace_context`, and every `_in_tx` repository method (DATA-028).
- **Input & validation:**
  - Rust rejects a nil `user_id` before opening a connection (`nil_user_id`) and, for workspace scope, a missing or nil `workspace_id` (`missing_workspace_context`).
  - The SQL functions independently raise `Invalid nil user UUID in security context` / `Invalid nil workspace UUID in security context`.
- **Errors:** Rust checks → `InvalidAuthContext` → 401 `AUTHENTICATION_FAILED`. A SQL raise would surface as `QueryFailed` → 500.
- **Guarantees:**
  - The context functions trust their arguments: they do not check that the user exists or is a member of the workspace. The HTTP auth middleware validates membership first (AUTH).
  - The context call's errors go through `RepositoryError::from`, so pool timeouts and transient codes stay retryable.
- **Source:** `migrations/0004_row_level_security.sql:72-117`, `src/repositories/postgresql/rls_context.rs:49-91`, `src/repositories/postgresql/transaction.rs:61-156,212-228`
- **Tests:** `tests/rls_isolation.rs::{workspace_context_repo_path_scopes_reads_to_the_authenticated_tenant, update_member_permissions_in_tx_guards_and_scopes_foreign_writes}`.

### DATA-012 Self-scoped tables

- **Behavior:** Under RLS, rows of `sessions`, `user_identities`, `user_preferences`, `oauth_tokens`, `verification_codes` and `pending_email_changes` are visible, updatable and deletable only when `user_id = app.current_user_id`, and inserts must carry that `user_id` (each policy is `FOR ALL USING (...)`; `user_identities` adds an identical `WITH CHECK`).
- **Kind:** `guarantee`
- **Trigger:** Any statement by `app_user` on these tables.
- **Guarantees:** A user cannot read or change another user's sessions, identities, preferences, provider tokens, codes or pending email change through the authenticated repositories, even if a query omits its own `user_id` filter. Most writes to these tables actually go through SECURITY DEFINER procedures (DATA-018), so the policies are a backstop for the direct-SQL paths (`session_repository.rs`, `identity_repository.rs`, `user_preferences_repository.rs`).
- **Source:** `migrations/0004_row_level_security.sql:218-230,238-240,257-273`
- **Tests:** `tests/rls_isolation.rs::{app_user_cannot_read_other_tenants_data, app_user_cannot_write_other_tenants_data}`.

### DATA-013 Users: self, plus co-members under a workspace context

- **Behavior:** `users_workspace_access` (`FOR ALL`) admits a row when `users.id = app.current_user_id` or `users.id` is in `get_current_workspace_user_ids()` (members of `app.current_workspace_id`). Under a user-scoped context the second branch is empty (the helper catches the `''` cast error and returns `[]`), so only the caller's own row is visible.
- **Kind:** `guarantee`
- **Trigger:** Any statement by `app_user` on `users`.
- **Guarantees:** The same predicate governs UPDATE and DELETE: under a workspace context, co-members' rows are writable at the RLS level. Repositories update and delete users only under a user-scoped context (`user_repository.rs:208,261`).
- **Source:** `migrations/0004_row_level_security.sql:163-175,198-204`
- **Tests:** `tests/rls_isolation.rs::{users_co_member_visible_under_workspace_context, app_user_cannot_read_other_tenants_data}`.

### DATA-014 Workspaces: owner, member or pending invitee

- **Behavior:** `workspaces_access_control` (`FOR ALL`, no `WITH CHECK`) admits a row when the caller owns it, when its id is in `get_current_user_workspace_ids()` (membership rows plus owned workspaces), or when its id is in `get_current_user_invited_workspace_ids()` (non-expired invitations whose `email_hash` equals the caller's stored `email_hash`).
- **Kind:** `guarantee`
- **Trigger:** Any statement by `app_user` on `workspaces`.
- **Guarantees:**
  - An invitee can read the workspace row (its name) before accepting.
  - Because the one predicate also serves as the INSERT/UPDATE check and the DELETE filter, any member and any pending invitee passes RLS for UPDATE and DELETE of the workspace; permission checks are application-only (WS).
  - An INSERT passes when `owner_id` is the caller.
- **Source:** `migrations/0004_row_level_security.sql:128-160,177-188,207-215`
- **Tests:** `tests/rls_isolation.rs::{app_user_cannot_read_other_tenants_data, rls_cross_tenant_visibility_is_intentional_where_designed}`.

### DATA-015 Memberships: any row of a workspace the caller belongs to

- **Behavior:** `workspace_memberships_access` (`FOR ALL`) admits a row when its `workspace_id` is in `get_current_user_workspace_ids()`. Reads, inserts, updates and deletes of any membership in such a workspace (including other members' rows and the caller's own permissions) pass RLS.
- **Kind:** `guarantee`
- **Trigger:** Any statement by `app_user` on `workspace_memberships`.
- **Guarantees:** Isolation is between workspaces, not between members. A user's first membership in a workspace cannot be created under RLS (chicken-and-egg), which is why owner-membership creation and invitation acceptance use SECURITY DEFINER procedures.
- **Source:** `migrations/0004_row_level_security.sql:233-235`, `migrations/0005_stored_procedures.sql:359-372,797-822`
- **Tests:** `tests/rls_isolation.rs::{app_user_cannot_write_other_tenants_data, update_member_permissions_is_scoped_to_own_workspace_under_rls, update_member_permissions_in_tx_guards_and_scopes_foreign_writes}`.

### DATA-016 Invitations: read in own workspaces or addressed to me; create only in own workspaces

- **Behavior:** `workspace_invitations_access` uses `USING (workspace_id ∈ my workspaces OR email_hash = my email_hash)` and `WITH CHECK (workspace_id ∈ my workspaces)`.
- **Kind:** `guarantee`
- **Trigger:** Any statement by `app_user` on `workspace_invitations`.
- **Guarantees:**
  - The invitee sees and can DELETE (decline) an invitation addressed to their email, including expired-but-not-yet-cleaned rows (the email branch has no expiry filter).
  - Nobody can INSERT, or UPDATE into, an invitation for a workspace they do not belong to, even one addressed to their own email.
  - Members can create, update and delete any invitation of their workspace at the RLS level.
- **Source:** `migrations/0004_row_level_security.sql:128-141,245-254`
- **Tests:** `tests/rls_isolation.rs::{workspace_invitations_write_check_rejects_own_email_in_foreign_workspace, rls_cross_tenant_visibility_is_intentional_where_designed}`.

### DATA-017 Files: read own or public, write owner or member

- **Behavior:** Per-command policies. `user_files`: SELECT when `user_id = me OR visibility = 'public'`; INSERT, UPDATE and DELETE only when `user_id = me`. `workspace_files`: SELECT when `workspace_id ∈ get_current_user_workspace_ids() OR visibility = 'public'`; INSERT, UPDATE and DELETE only when `workspace_id ∈ get_current_user_workspace_ids()`.
- **Kind:** `guarantee`
- **Trigger:** Any statement by `app_user` on the file tables.
- **Guarantees:** A public file never grants a stranger write or delete. The public branch needs no context at all. Workspace-file reads work under a user-scoped context because membership derives from `app.current_user_id`.
- **Source:** `migrations/0004_row_level_security.sql:297-347`
- **Tests:** `tests/rls_isolation.rs::{app_user_cannot_delete_other_tenants_data, rls_cross_tenant_visibility_is_intentional_where_designed, app_user_cannot_read_other_tenants_data}`.

### DATA-018 Paths that bypass RLS

- **Behavior:** The following reach RLS-protected rows without a policy check:
  - **SECURITY DEFINER functions** (run as the function owner, who owns the tables): the context setters and RLS helpers (0004), every `sp_public_*` and `sp_system_*` (0005), and every `sp_cleanup_*` except `sp_cleanup_expired_rate_limits` (0006). The `Public*` repositories and the re-encryption service reach the data only through these.
  - **Foreign-key actions** (CASCADE / SET NULL) run by the database, e.g. account and workspace deletion cascades.
  - **No-RLS infrastructure tables**: `idempotency_cache`, `sse_events`, `rate_limits`, `background_task_locks`, `pending_object_deletions`, `_sqlx_migrations`, gated only by table grants.
  - **Admin connections**: `db-bootstrap`, `db-migrate`.
- **Kind:** `guarantee`
- **Trigger:** Any call of these functions or tables.
- **Guarantees:** Each DEFINER function scopes rows by its own `WHERE` (its parameters), which is then the only tenant boundary. Reasons, per the SQL comments:
  - pre-authentication (no user identity yet: login lookups, session lookup/creation/refresh, registration, password reset);
  - chicken-and-egg membership creation (owner membership, invitation acceptance);
  - atomic multi-table writes without composing contexts (password change/reset, unlink, email change);
  - rows RLS would hide but a cascade will delete (`sp_public_find_authored_invitations_in_unowned_workspaces`);
  - cross-tenant system work (cleanup sweeps, re-encryption);
  - anonymous public-file download.
- **Source:** `migrations/0004_row_level_security.sql:15-24,47-51`, `migrations/0005_stored_procedures.sql:16-23,792-800,833-841,1837-1849`, `migrations/0006_background_tasks.sql:13-24`, `src/repositories/postgresql/transaction.rs:158-207`
- **Tests:** `tests/lint_definer_sp_scoping_coverage.rs` (classifies every DEFINER function; names a pinning test for each scoped one); the per-SP `tests/*_repository_live.rs` suites it names.

### DATA-019 One account per email

- **Behavior:** `users.email_hash` is UNIQUE. A second user with the same normalized email (hashed under the same key version) is rejected by the database with 23505.
- **Kind:** `rule`
- **Trigger:** Any INSERT or email UPDATE on `users` (registration, `sp_public_complete_email_change`).
- **Errors:** 23505 → `ConstraintViolation` → 409 `RESOURCE_CONFLICT` unless the service maps it (e.g. USER-026).
- **Guarantees:** The constraint compares hashes, so it is authoritative only within one key version (DATA-044).
- **Source:** `migrations/0002_tables.sql:33-36`, `migrations/0005_stored_procedures.sql:1482-1490`
- **Tests:** Seeded as a fixture constraint in `tests/oauth_registration_tx_live.rs` and others; no direct duplicate-email test found.

### DATA-020 Identity row shape and uniqueness

- **Behavior:** A user has at most one identity per method; a given `(method, provider_subject)` belongs to at most one user; a `credentials` row must have a password hash and no subject; any other method must have a subject and no password hash.
- **Kind:** `rule`
- **Trigger:** INSERT into `user_identities` (registration via `sp_public_insert_identity`, linking via `identity_repository.rs::insert`).
- **Errors:** UNIQUE → 23505 → `ConstraintViolation` (409 unless the service maps it); shape CHECK → 23514 → `QueryFailed` → 500.
- **Guarantees:** Several credentials rows with NULL subject do not collide on the subject constraint (NULLs are distinct); the `(user_id, method)` constraint prevents them.
- **Source:** `migrations/0002_tables.sql:70-86`
- **Tests:** `tests/oauth_registration_tx_live.rs`, `tests/credentials_identity_repository_live.rs` (reference the constraints).

### DATA-021 One session per user and device; token hashes unique

- **Behavior:** `(user_id, device_fingerprint)` is unique, so a new login on the same device replaces the session row via `ON CONFLICT ON CONSTRAINT sessions_user_device_unique DO UPDATE` (new id, new hashes, `previous_refresh_token_hash = NULL`, new expiries; `device_fingerprint` and `user_id` kept). Access and refresh token hashes are each unique across all sessions.
- **Kind:** `rule`
- **Trigger:** `sp_public_upsert_session` (AUTH login paths).
- **Effects:** The replaced session's `oauth_tokens` row follows the new id through ON UPDATE CASCADE.
- **Source:** `migrations/0002_tables.sql:121-133,654`, `migrations/0005_stored_procedures.sql:464-521`
- **Tests:** `tests/session_repository_live.rs` (not read in detail).

### DATA-022 Single-row-per-key tables

- **Behavior:** The database holds at most one verification code per `(user_id, purpose)`, one pending email change per user, one provider-token row per session, and one preferences row per user. Writers use upserts that replace the existing row: `sp_public_upsert_verification_code` and `sp_public_initiate_email_change` regenerate the id, reset `attempts_used` to 0 and reset the expiry; `sp_public_upsert_oauth_tokens` replaces the ciphertext and expiry.
- **Kind:** `rule`
- **Trigger:** The upsert procedures (AUTH, USER).
- **Source:** `migrations/0002_tables.sql:157,184-186,214-215,270`, `migrations/0005_stored_procedures.sql:876-900,1087-1122,1414-1446`
- **Tests:** `tests/verification_code_repository_live.rs::upsert_verification_code_conflict_resets_the_brute_force_lockout`.

### DATA-023 One membership per user and workspace; one invitation per workspace and email

- **Behavior:** `workspace_memberships(user_id, workspace_id)` and `workspace_invitations(workspace_id, email_hash)` are unique. Re-inviting the same email in a workspace updates the existing row (new ciphertext, permissions, inviter, `invited_at`, expiry; id kept). Accepting an invitation when already a member overwrites that membership's permissions with the invitation's (`ON CONFLICT ... DO UPDATE SET permissions`).
- **Kind:** `rule`
- **Trigger:** `create_invitation` (`workspace_member_repository.rs:1403`), `sp_public_accept_workspace_invitation`, `sp_public_create_owner_membership`.
- **Errors:** A duplicate owner membership raises 23505 → 409.
- **Source:** `migrations/0002_tables.sql:245,265`, `src/repositories/postgresql/workspace_member_repository.rs:1424-1435`, `migrations/0005_stored_procedures.sql:801-822`
- **Tests:** `tests/workspace_invitation_repository_live.rs`, `tests/public_workspace_member_repository_live.rs`.

### DATA-024 Column value checks

- **Behavior:** The database rejects: file `visibility` other than `private`/`public`; negative `size_bytes`; duplicate `storage_key` within a file table; idempotency `status` other than `processing`/`completed`; `status_code` outside 200..599; `sse_events.scope_type` other than `workspace`/`user`.
- **Kind:** `rule`
- **Trigger:** INSERT/UPDATE on those tables.
- **Errors:** CHECK → 23514 → `QueryFailed` → 500 `INTERNAL_SERVER_ERROR`; UNIQUE `storage_key` → 23505 → 409.
- **Source:** `migrations/0002_tables.sql:331-333,352-354,467,475,542`
- **Tests:** None found for the CHECKs.

### DATA-025 Deletion lifecycle

- **Behavior:** Deleting a `users` row cascades through every user-owned table and every workspace the user owns (with those workspaces' memberships, invitations and files); deleting a `workspaces` row cascades its memberships, invitations and files and nulls other users' `default_workspace_id`. File deletions null the avatar/picture pointer. Uploader deletion nulls `workspace_files.uploaded_by`. Audit rows, `sse_events`, `rate_limits`, `idempotency_cache` and `pending_object_deletions` have no FK and survive.
- **Kind:** `guarantee`
- **Trigger:** `DELETE FROM users` (`user_repository.rs:273`), `DELETE FROM workspaces` (`workspace_repository.rs:261,1083`), file deletes.
- **Effects:** Every cascaded file row fires the enqueue trigger (FILE-016). Cascades run as database actions, not under the caller's RLS.
- **Source:** `migrations/0002_tables.sql:638-681`
- **Tests:** `tests/object_deletion_trigger.rs::{user_deletion_cascade_enqueues_owned_file_key, workspace_deletion_cascade_enqueues_file_key, uploader_deletion_nulls_attribution_without_enqueue}`. Detail lists: USER-030, FILE-016, FILE-017, AUDIT-020.

### DATA-026 Deferred foreign keys for same-transaction creation

- **Behavior:** `workspaces.owner_id`, `user_identities.user_id` and both `workspace_memberships` FKs are checked at commit, so a transaction can insert the child before the parent; a dangling reference fails the COMMIT, not the INSERT.
- **Kind:** `guarantee`
- **Trigger:** Registration and workspace-creation transactions.
- **Errors:** 23503 at commit → `ConstraintViolation` → 409 (the commit error goes through `RepositoryError::from`).
- **Guarantees:** CASCADE actions on these FKs still run immediately (PostgreSQL does not defer referential actions).
- **Source:** `migrations/0002_tables.sql:641-650`, `src/repositories/postgresql/transaction.rs:93-99`
- **Tests:** `tests/repository_error_classification.rs::real_foreign_key_violation_classifies_as_constraint_violation` (non-deferred FK).

### DATA-027 Transaction helpers

- **Behavior:** `with_user_context`, `with_workspace_context` and `with_public_context` each begin a transaction on the pool, optionally activate RLS context (DATA-011), run a `FnOnce` closure with the transaction, and commit. An `Err` from the closure returns before commit and the transaction rolls back on drop. `with_public_context` activates no context; its closure must use DEFINER procedures or `_in_tx` methods that activate their own context.
- **Kind:** `guarantee`
- **Trigger:** Every authenticated repository method and every multi-repository service transaction (registration, login workspace resolution, session creation, password reset/change, unlink, email-change confirm, invitation accept, workspace create/transfer).
- **Errors:** Begin/commit failures are classified through `RepositoryError::from` with operation context (`begin_transaction` / `commit_transaction`, scope `user_scope` / `workspace_scope` / `public_scope`).
- **Guarantees:**
  - The helpers do not retry; serialization failures and deadlocks propagate to the service-layer retry (DATA-030).
  - No other file in `src/` calls `pool.begin()` or `.commit().await`.
- **Source:** `src/repositories/postgresql/transaction.rs:1-228`
- **Tests:** `src/repositories/postgresql/transaction.rs::structural::{no_direct_pool_begin_outside_transaction_module, no_direct_tx_commit_outside_transaction_module, no_stringified_sqlx_error_in_query_failed}`.

### DATA-028 `_in_tx` methods re-activate context inside the caller's transaction

- **Behavior:** Transactional repository methods (`create_in_tx` for both audit tables, workspaces `create_in_tx`/`update_in_tx`/`delete_in_tx`, preferences `update_in_tx`, member `update_member_permissions_in_tx`, `find_user_invitation_by_id_in_tx`) validate the context and call `activate_user_context` / `activate_workspace_context` on the caller's transaction before their statement. The new GUC values stay in force for the rest of that transaction.
- **Kind:** `guarantee`
- **Trigger:** Any `_in_tx` call, e.g. `log_event_in_tx` inside a `with_public_context` registration transaction (sets `app.current_user_id` to the new user) or a workspace audit inside a `with_user_context` transaction (switches to workspace scope).
- **Errors:** Nil user → `nil_user_id`; missing workspace → `missing_workspace_context` → 401.
- **Source:** `src/repositories/postgresql/workspace_audit_log_repository.rs:486-519`, `src/repositories/postgresql/user_audit_log_repository.rs:455-472`, `src/repositories/postgresql/workspace_repository.rs:922-939,989-1007,1060-1077`, `src/repositories/postgresql/user_preferences_repository.rs:442-460`, `src/repositories/postgresql/workspace_member_repository.rs:1223-1240,1626-1664`
- **Tests:** `tests/rls_isolation.rs::update_member_permissions_in_tx_guards_and_scopes_foreign_writes`.

### DATA-029 Database error classification and HTTP mapping

- **Behavior:** `RepositoryError::from(sqlx::Error)` maps:
  - `RowNotFound` → `NotFound` → 404 `RESOURCE_NOT_FOUND`;
  - 23505 unique, 23503 foreign key → `ConstraintViolation` → 409 `RESOURCE_CONFLICT` with the generic message "Resource already exists" (constraint names are not returned);
  - 40001 serialization, 40P01 deadlock → `TransientError` (retryable) → 500 if retries run out;
  - `PoolTimedOut`, `WorkerCrashed`, and `Io` errors of kind ConnectionReset/Aborted, BrokenPipe, TimedOut, NotConnected, Interrupted, UnexpectedEof → `ConnectionFailed` (retryable) → 503 `SERVICE_UNAVAILABLE`;
  - every other code (23514 check, 23502 not-null, 42501 RLS violation, 22P02 bad input, ...) and every other sqlx error → `QueryFailed` → 500 `INTERNAL_SERVER_ERROR`.
  Decryption failures are `DataIntegrityError` → 500 `DATA_INTEGRITY_ERROR` with a generic message.
- **Kind:** `guarantee` (it decides the status a client sees for data faults)
- **Trigger:** Every database error.
- **Source:** `src/errors/repository_error.rs:224-246,831-960`, `src/errors/api_error.rs:688-800`
- **Tests:** `tests/repository_error_classification.rs::{real_unique_violation_classifies_as_constraint_violation, real_foreign_key_violation_classifies_as_constraint_violation}`; unit matrix in `src/errors/repository_error.rs`.

### DATA-030 Service-layer database retry

- **Behavior:** `retry_database_operation(op, name)` runs the operation up to 3 times in total. After a failure it retries only if `is_transient()` (`TransientError` or `ConnectionFailed`), sleeping 2 s then 4 s, each ±500 ms jitter; any other error returns at once. The final error is converted to `ApiError`. `retry_database_operation_with_predicate` takes a custom predicate.
- **Kind:** `hardening`
- **Trigger:** Service methods that wrap repository calls (each attempt re-runs the whole repository transaction).
- **Effects:** Debug logs per failed attempt and per retry (`component=infrastructure`, `subsystem=database_retry`, `trace_id`, `attempt`, `delay_ms`); no success or final-error log (the HTTP layer logs).
- **Guarantees:** Callers must make operations repeatable; repository deletes are idempotent for this reason (DATA-032).
- **Source:** `src/utils/database_retry.rs:47-230`, `src/utils/retry.rs:204-217`
- **Tests:** `src/utils/database_retry.rs::tests` (retry-loop tests).

### DATA-031 Background-task retry

- **Behavior:** Every background task body runs inside `retry_with_backoff(op, name, 3)`: 3 attempts, same 2 s / 4 s ±500 ms backoff, permanent errors abort without sleeping, returns `RepositoryError`.
- **Kind:** `hardening`
- **Trigger:** Each background-task cycle (DATA-038 to DATA-041, RT-025, AUDIT-021, FILE-038).
- **Source:** `src/utils/retry.rs:76-200`, `src/server.rs:1546-1563,1599-1646,1683-1702,1740-1767,1874-1878,2038-2042`
- **Tests:** `src/utils/retry.rs::tests`.

### DATA-032 Repository deletes are idempotent

- **Behavior:** `DELETE` by id for users, workspaces, sessions (`revoke`), invitations (`delete_invitation`, `delete_user_invitation`) and the object-deletion queue (`complete`) return success when zero rows match; the service's prior lookup is the not-found gate.
- **Kind:** `hardening`
- **Trigger:** The delete repository methods, notably under DATA-030 retries after a lost commit acknowledgement.
- **Source:** `src/repositories/postgresql/user_repository.rs:258-292`, `src/repositories/postgresql/workspace_repository.rs:248-280`, `src/repositories/postgresql/session_repository.rs:155-190`, `src/repositories/postgresql/workspace_member_repository.rs:1160-1195,1573-1610`, `src/repositories/postgresql/object_deletion_queue_repository.rs:87-103`
- **Tests:** `tests/object_deletion_queue_repository_live.rs` (`complete_*`); none found for the others at DB level.

### DATA-033 Column-scoped updates

- **Behavior:** Workspace rename writes only `name` and `updated_at`; ownership transfer writes only `owner_id` and `updated_at`; user rename writes only `name` and `updated_at`. Values from the caller's in-memory model for other columns are never written back.
- **Kind:** `hardening`
- **Trigger:** `Repository<Workspace>::update`, `TransactionalRepository<Workspace>::update_in_tx`, `Repository<User>::update`.
- **Guarantees:** A concurrent rename, transfer, email change or avatar swap is not reverted by a stale snapshot (lost update).
- **Source:** `src/repositories/postgresql/workspace_repository.rs:182-232,989-1045`, `src/repositories/postgresql/user_repository.rs:197-242`
- **Tests:** `tests/repository_column_scoping_live.rs::{workspace_rename_does_not_clobber_a_completed_ownership_transfer, workspace_ownership_transfer_does_not_revert_a_concurrent_rename, user_rename_does_not_clobber_email_hash_or_avatar}`. See also USER-008.

### DATA-034 Connection pool

- **Behavior:** One sqlx `PgPool` per process with `max_connections = DATABASE_MAX_CONNECTIONS` (default 10), `min_connections = min(max, 2)`, `acquire_timeout = DATABASE_ACQUIRE_TIMEOUT_SECS` (default 10 s), `idle_timeout` 600 s, `max_lifetime` 1800 s, `test_before_acquire(true)`. It is created at startup before encryption and services; a connect failure aborts startup. It is closed after background tasks drain, before OTel shutdown.
- **Kind:** `guarantee`
- **Trigger:** Startup and shutdown.
- **Input & validation:** Both settings must be greater than 0 (`Config::validate`). `DATABASE_URL` must be non-empty.
- **Effects:** The SSE listener takes one more connection from this pool (RT-023). Pool exhaustion surfaces as `ConnectionFailed` → 503 (DATA-029).
- **Source:** `src/config/database.rs:6-25`, `src/config/environment.rs:346-350,631-664`, `src/server.rs:1100,2215-2232`
- **Tests:** `src/config/environment.rs::tests::config_rejects_empty_database_url` and the zero-floor tests.

### DATA-035 Pool gauge

- **Behavior:** When OTel metrics are active, an observable gauge `db_pool_connections` reports three series per collection: `state="total"` (`pool.size()`), `state="idle"` (`pool.num_idle()`) and `state="max"` (the configured maximum).
- **Kind:** `capability` (operators read it)
- **Trigger:** Each meter collection cycle; registered once at startup only if a `MeterProvider` exists.
- **Source:** `src/utils/db_pool_metrics.rs:53-69`, `src/server.rs:1174-1186`
- **Tests:** Alert rules reference it (`tests/prometheus/alert-rules.test.yml`); no Rust test found.

### DATA-036 Repository access split

- **Behavior:** Repository traits come in three families:
  - **Public** (`PublicUser`, `PublicSession`, `PublicWorkspace`, `PublicUserPreferences`, `PublicOAuthToken`, `PublicVerificationCode`, `PublicWorkspaceMember`, `PublicUserFile`, `PublicWorkspaceFile`): no `AuthContext`; every statement calls a SECURITY DEFINER procedure, either directly on the pool (single statement, autocommit) or on a caller's transaction (`*_in_tx`).
  - **Authenticated** (`AuthenticatedUser`, `AuthenticatedWorkspace`, `AuthenticatedSession`, `AuthenticatedIdentity`, `AuthenticatedUserPreferences`, `AuthenticatedWorkspaceMember`, both audit-log traits, both file traits): take `&AuthContext` and run raw SQL under DATA-011 context.
  - **Transactional** (`TransactionalAuthenticatedWorkspace`, `...UserPreferences`, `...WorkspaceMember`, both audit-log traits; generic `TransactionalRepository<T>`): `*_in_tx` variants (DATA-028).
  Infrastructure repositories (rate limit, idempotency, SSE event/notification, task lock, object-deletion queue) use the pool directly on no-RLS tables or through INVOKER procedures. `Combined*` marker traits bundle each family for dependency injection. The re-encryption service bypasses repositories and uses the pool directly.
- **Kind:** `guarantee`
- **Trigger:** Service construction and every data access.
- **Guarantees:** Every ciphertext column is encrypted before it reaches SQL and decrypted after it returns, inside the repository; services see plaintext models.
- **Source:** `src/repositories/traits.rs:24-686,687-1404,1411-1615,1619-1735,1739-1808,1824-2026`, `src/repositories/mod.rs:39-86`, `src/repositories/postgresql/mod.rs:1-104`
- **Tests:** `tests/repository_smoke.rs` (one query per security-critical repository against an empty schema).

### DATA-037 Keyset pagination SQL

- **Behavior:** For list queries the repositories append `AND (sort_col, id_col) <op> ($ts, $id)` (no predicate on the first page; `<`/`>` chosen from the sort direction and page direction) and `ORDER BY sort_col <ord>, id_col <ord> LIMIT n+1`, reversing the order for "before" pages. Only the cursor values are bound; column names come from per-repository allowlists.
- **Kind:** `guarantee`
- **Trigger:** List methods for user/workspace files, sessions, user/workspace audit logs, workspace members.
- **Source:** `src/repositories/postgresql/keyset.rs:39-94`
- **Tests:** `src/repositories/postgresql/keyset.rs::tests::*`. Page contract: HTTP domain; AUDIT-007.

### DATA-038 `auth_cleanup` task

- **Behavior:** Every `CLEANUP_INTERVAL_HOURS` (default 6, first run at startup) one instance, holding the `auth_cleanup` lock, runs four sweeps in order, each in its own 3-attempt retry:
  1. Sessions: captures `cutoff = now()`, fetches provider tokens of sessions with `refresh_token_expires_at < cutoff` whose provider token is unexpired or never expires, revokes them at the provider (fail-open: a fetch error logs WARN `oauth_token_fetch_failed` and the delete proceeds), then deletes sessions with `refresh_token_expires_at < cutoff` (cascading their `oauth_tokens`).
  2. Invitations with `expires_at < NOW()`.
  3. Verification codes (all purposes) with `expires_at < NOW()`.
  4. Pending email changes with `expires_at < NOW()`.
- **Kind:** `rule`
- **Trigger:** Background job `auth_cleanup`, spawned when `ENABLE_BACKGROUND_CLEANUP=true`.
- **Effects:** `log_cleanup_result` per sweep (`sessions`, `invitations`, `verification_codes`, `pending_email_changes`): INFO with `removed_count` when > 0, DEBUG when 0, ERROR after all retries. No audit entries and no SSE events for removed rows.
- **Guarantees:** The revocation set equals the delete set (same cutoff). A retry recomputes the cutoff and refetches. The SPs are SECURITY DEFINER.
- **Source:** `src/server.rs:1567-1648`, `src/services/public_auth_service.rs:1563-1630,1804-1808,2290-2294`, `migrations/0006_background_tasks.sql:50-151`, `migrations/0005_stored_procedures.sql:1057-1080`
- **Tests:** `src/services/public_auth_service.rs::tests::{cleanup_expired_sessions_swallows_revocable_fetch_failure, cleanup_expired_sessions_revokes_fetched_provider_tokens}`; `tests/repository_smoke.rs::{public_session_cleanup_expired_sessions_executes_against_schema, public_verification_cleanup_expired_codes_executes_against_schema, public_verification_cleanup_expired_pending_email_changes_executes_against_schema, public_workspace_member_cleanup_expired_invitations_executes_against_schema}`; `tests/oauth_token_repository_live.rs::find_revocable_for_expired_sessions_selects_expired_session_with_live_provider_token`.

### DATA-039 `rate_limit_cleanup` task

- **Behavior:** Every `RATE_LIMIT_CLEANUP_INTERVAL_HOURS` (default 1) one instance deletes `rate_limits` rows whose `window_start + window_secs < NOW()` via `sp_cleanup_expired_rate_limits()` (SECURITY INVOKER).
- **Kind:** `rule`
- **Trigger:** Background job `rate_limit_cleanup`, spawned when `ENABLE_BACKGROUND_CLEANUP=true`.
- **Effects:** `log_cleanup_result("rate_limit", ...)`.
- **Source:** `src/server.rs:1515-1565`, `src/services/rate_limit_service.rs:326`, `src/repositories/postgresql/rate_limit_repository.rs:141-166`, `migrations/0006_background_tasks.sql:279-292`
- **Tests:** `tests/rate_limit_repository_live.rs`.

### DATA-040 `idempotency_cleanup` task

- **Behavior:** Every `IDEMPOTENCY_CLEANUP_INTERVAL_HOURS` (default 1) one instance deletes `idempotency_cache` rows that are `completed` and older than 24 h, or `processing` and older than 5 min (stale-lock recovery after a crash). Both thresholds are fixed in SQL.
- **Kind:** `rule`
- **Trigger:** Background job `idempotency_cleanup`, spawned when `ENABLE_BACKGROUND_CLEANUP=true`.
- **Effects:** `log_cleanup_result("idempotency", ...)`.
- **Source:** `src/server.rs:1650-1705`, `src/services/idempotency_service.rs:222`, `src/repositories/postgresql/idempotency_repository.rs:384-411`, `migrations/0006_background_tasks.sql:167-189`
- **Tests:** `tests/idempotency_repository_live.rs`.

Other tasks: `audit_log_cleanup` (AUDIT-021), `sse_event_cleanup` (RT-025), `object_deletion_reaper` (FILE-035 to FILE-038). Scheduling and locking: RT-028 to RT-033.

### DATA-041 `re_encryption` task

- **Behavior:** Spawned only when `ENABLE_BACKGROUND_CLEANUP=true` and the key ring holds more than one version. Every `RE_ENCRYPTION_INTERVAL_HOURS` (default 6, first run at startup) one instance runs a full re-encryption cycle (DATA-047) inside a 3-attempt retry.
- **Kind:** `rule`
- **Trigger:** Background job `re_encryption`.
- **Effects:** Logs, all `component=background_cleanup`, `cleanup_type=re_encryption`:
  - WARN "Re-encryption cycle completed with permanent per-row failures" with `total_processed`, `total_failed` and per-table processed/failed counts, when any row failed;
  - INFO "Re-encryption cycle completed" with per-table processed counts, when rows were processed and none failed;
  - DEBUG "...all data uses active key version" when nothing was processed;
  - ERROR after all retries fail.
  At startup an INFO line reports the active version and key count, or why the task is disabled (`single_key_version` or the master switch).
- **Source:** `src/server.rs:1479-1484,1841-1988`, `src/server.rs:786-797`
- **Tests:** `src/server.rs::tests::{conditional_tasks_require_their_backing, conditional_tasks_are_independent}`.

### DATA-042 Field encryption format

- **Behavior:** Encrypted columns hold `base64(STANDARD)` of `version_byte || 12-byte random nonce || AES-256-GCM ciphertext || 16-byte tag`. Encryption always uses the active version. Each master key yields an AES key and an HMAC key through HKDF-SHA256 (empty salt, info `app-aes-256-gcm` / `app-hmac-sha256`, 32 bytes). No associated data is bound.
- **Kind:** `guarantee`
- **Trigger:** Every write of a ciphertext column (DATA-043).
- **Guarantees:** Two encryptions of the same plaintext differ. The format and derivation are pinned by a known-answer test so existing data stays decryptable.
- **Source:** `src/encryption/database_encryption.rs:14-21,55-158,395-402`
- **Tests:** `src/encryption/database_encryption.rs::tests::{known_answer_locks_hkdf_and_ciphertext_format, encrypt_decrypt_round_trip, encrypt_produces_different_ciphertexts_for_same_input, version_byte_reflects_active_version, hkdf_derives_different_aes_and_hmac_keys, ...}`.

### DATA-043 What is encrypted at rest

- **Behavior:** Application-encrypted columns: `users.email`, `users.name`, `sessions.device_name`, `oauth_tokens.provider_access_token_encrypted`, `pending_email_changes.new_email`, `workspaces.name`, `workspace_invitations.email`, `workspace_audit_logs.{old_value,new_value,metadata}`, `user_audit_logs.{old_value,new_value,metadata}`, `user_files.file_name`, `workspace_files.file_name`, `idempotency_cache.response_body`. HMAC-only columns: `users.email_hash`, `pending_email_changes.new_email_hash`, `workspace_invitations.email_hash`, `sessions.{access,refresh,previous_refresh}_token_hash`, `verification_codes.code_hash`, and email hashes embedded in `rate_limits.key`. Plaintext: everything else, including `user_identities.provider_subject`, `sessions.device_fingerprint`, `sse_events.payload`, membership/invitation `permissions`, file `content_type`/`size_bytes`/`storage_key`.
- **Kind:** `guarantee`
- **Trigger:** Repository writes.
- **Source:** `migrations/0002_tables.sql` column comments; encrypt calls in `src/repositories/postgresql/*` (e.g. `public_user_repository.rs:20-29`, `workspace_member_repository.rs:1408-1409`, `idempotency_repository.rs:297`); `src/encryption/re_encryption.rs:171-221`
- **Tests:** `tests/re_encryption_all_tables.rs::re_encryption_cycle_rotates_every_encrypted_table` (enumerates the ciphertext columns).

### DATA-044 Blind-index hashing and multi-version lookup

- **Behavior:** `hmac_for_search` computes hex HMAC-SHA256 under the active version's HMAC key for writes. `hmac_for_search_all_versions` returns the deduplicated hashes under every key in the ring; lookups pass that array to `= ANY($1)` (email lookups, session lookups by access/refresh/previous token, invitation member-email checks) or compare against it in Rust (verification codes). Emails must be normalized by the caller first. After an email-hash lookup the repository decrypts the email and fails with `DataIntegrityError` if it differs from the input.
- **Kind:** `guarantee`
- **Trigger:** Every write and lookup of a hashed column.
- **Guarantees:** During a rotation, rows hashed under either version are found. Equality comparisons between two stored hashes (RLS `email_hash = get_current_user_email_hash()`, `get_user_pending_invitations`, the UNIQUE constraints) do not get this treatment.
- **Source:** `src/encryption/database_encryption.rs:268-307`, `src/repositories/postgresql/public_user_repository.rs:128-171,188-230`, `src/repositories/postgresql/public_session_repository.rs:101,152,192`, `src/repositories/postgresql/workspace_member_repository.rs:220,643`, `src/auth/verification_code_validator.rs:73-74`
- **Tests:** `src/encryption/database_encryption.rs::tests::{hmac_all_versions_returns_one_per_key, hmac_all_versions_includes_active_version_hash, hmac_single_key_all_versions_matches_single, ...}`.

### DATA-045 Key ring loading

- **Behavior:** At startup the server reads `APP_DB_ENCRYPTION_KEY_1` .. `_255` (each must be 64 hex chars = 32 bytes) and `APP_DB_ACTIVE_KEY_VERSION` (1-255; default: the lowest version present). It panics ("Failed to load encryption key ring") if no key is set, a key is not hex or not 32 bytes, the active version is non-numeric, 0, or absent from the ring. Hex strings and intermediate buffers are zeroized; master and derived keys are zeroized on drop and redacted in Debug output.
- **Kind:** `rule`
- **Trigger:** Startup (`EncryptionKeyManager::load_key_ring`, `DatabaseEncryption::new`).
- **Effects:** INFO "Database encryption initialized with versioned key ring" with `active_key_version` and `key_version_count`.
- **Source:** `src/encryption/key_manager.rs:76-251`, `src/encryption/database_encryption.rs:358-393`, `src/server.rs:1108-1131`
- **Tests:** `src/encryption/key_manager.rs::tests::*`.

### DATA-046 Decryption failure semantics

- **Behavior:** Decrypt fails with `DataIntegrityError` on invalid base64, data shorter than 29 bytes, version byte 0, a version not in the ring ("Cannot decrypt data encrypted with a removed key"), a GCM tag mismatch, or non-UTF-8 plaintext. Error contexts never contain the plaintext or ciphertext.
- **Kind:** `guarantee`
- **Trigger:** Any read of a ciphertext column.
- **Errors:** → 500 `DATA_INTEGRITY_ERROR` (DATA-029). A list read fails as a whole if one row fails.
- **Source:** `src/encryption/database_encryption.rs:165-251`
- **Tests:** `src/encryption/database_encryption.rs::tests::{decrypt_fails_when_key_version_removed, decrypt_rejects_version_zero, decrypt_rejects_truncated_data, decrypt_rejects_invalid_base64, decrypt_rejects_tampered_ciphertext}`.

### DATA-047 Re-encryption cycle

- **Behavior:** One cycle walks, in order, `users`, `workspace_invitations`, `pending_email_changes`, `oauth_tokens`, `workspaces`, `workspace_audit_logs`, `user_audit_logs`, `sessions`, `user_files`, `workspace_files`, `idempotency_cache`. Each table is read in keyset batches of 100 (`id > cursor ORDER BY id`; `composite_key` for `idempotency_cache`, `status = 'completed'` only) through `sp_system_re_encrypt_fetch_*` (raw SQL for `idempotency_cache`). A row is rewritten only if at least one of its non-NULL ciphertext columns has a version byte other than the active one; all its ciphertext columns are then decrypted and re-encrypted, and for `users`, `workspace_invitations` and `pending_email_changes` the email blind index is recomputed under the active key. Each row update is its own autocommit statement.
- **Kind:** `guarantee`
- **Trigger:** DATA-041.
- **Guarantees:** Re-running is idempotent: already-current rows are skipped. NULL columns stay NULL. `users` is gated on both `email` and `name`, so a lagging `name` is not stranded.
- **Source:** `src/encryption/re_encryption.rs:17,171-221,245-1044`, `migrations/0005_stored_procedures.sql:1834-2146`
- **Tests:** `tests/re_encryption_all_tables.rs::re_encryption_cycle_rotates_every_encrypted_table`; `tests/re_encryption_cycle.rs::re_encryption_cycle_rotates_lagging_columns_and_skips_corrupt_rows`.

### DATA-048 Re-encryption per-row resilience

- **Behavior:** A row that fails permanently (corrupt ciphertext, missing key version, constraint violation on update) is logged at WARN ("Skipping row due to permanent error during re-encryption", `entity_type`, `entity_id`, `error_code`) and counted in that table's `failed`; the cycle continues. A transient error aborts the table method and bubbles to the outer retry, which restarts the cycle from the first table. A failing batch read aborts the cycle.
- **Kind:** `hardening`
- **Trigger:** DATA-047.
- **Source:** `src/encryption/re_encryption.rs:19-37,224-241,1046-1091`
- **Tests:** `src/encryption/re_encryption.rs::tests::{tally_row_*}`; `tests/re_encryption_cycle.rs`.

### DATA-049 Re-encryption compare-and-swap guard

- **Behavior:** The update procedures for `users` (email and name), `workspaces` (name), `oauth_tokens`, `pending_email_changes` and `sessions` (device_name) rewrite a row only if its ciphertext column(s) still equal, `IS NOT DISTINCT FROM`, what the batch read; otherwise they change nothing. The `workspace_invitations`, both audit-log, both file-table and `idempotency_cache` updates are unconditional.
- **Kind:** `hardening`
- **Trigger:** DATA-047.
- **Guarantees:** A concurrent email change, rename, token refresh, email-change re-request or re-login is not overwritten with a stale re-wrapped value. A CAS no-op is still counted as `processed`; the row is picked up again on a later cycle if it still needs it.
- **Source:** `migrations/0005_stored_procedures.sql:1868-1883,1901-1912,1930-1938,1956-1969,1992-2002,2022-2030,2049-2057,2087-2097,2118-2123,2141-2146`, `src/encryption/re_encryption.rs:299-318,366-379,429-442,569-585,802-815,988-1001`
- **Tests:** `tests/re_encryption_cas_guard.rs::{re_encrypt_update_applies_on_match_and_noops_on_concurrent_write, re_encrypt_workspace_name_cas_applies_on_match_and_noops_on_concurrent_rename, re_encrypt_oauth_token_cas_applies_on_match_and_noops_on_concurrent_refresh, re_encrypt_pending_email_cas_applies_on_match_and_noops_on_concurrent_rerequest, re_encrypt_session_device_name_cas_applies_on_match_and_noops_on_concurrent_relogin}`.

### DATA-050 Hash-only secrets are not rotated

- **Behavior:** Session access/refresh token hashes and verification-code hashes are HMACs of values the server never stores, so re-encryption cannot recompute them. They stay under the key version that was active when written: a session moves to the new key only on its next refresh (new token pair). Once an old key is removed from the ring, sessions not refreshed since the switch and codes issued before it no longer match any lookup hash, so those users are logged out and the codes are dead.
- **Kind:** `rule`
- **Trigger:** Removing `APP_DB_ENCRYPTION_KEY_<n>` from the environment and restarting.
- **Source:** `src/services/public_auth_service.rs:771-774,1223-1224,1667,1906`, `src/encryption/re_encryption.rs:171-221` (no session-hash or code-hash pass), `documentation/DEPLOYMENT.md:212-225`, `.env.example:1346-1348`
- **Tests:** None found.

## Stored procedure catalog

DEF = SECURITY DEFINER with `search_path` pinned; INV = SECURITY INVOKER (runs as `app_user`). All are `plpgsql`. Every function has an explicit `GRANT EXECUTE ... TO app_user` in `0008_grants.sql:51-168`. File references are to `migrations/`; repository paths are under `src/repositories/postgresql/`.

### RLS context and helpers (`0004`)

| Function | Line | Mode | Purpose | Caller |
|---|---|---|---|---|
| `set_request_context(user, workspace)` | 0004:72 | DEF | Reject nil UUIDs; set both GUCs, transaction-local | `rls_context.rs:73` (`activate_workspace_context`) |
| `clear_request_context()` | 0004:90 | DEF | Set both GUCs to `''` | No caller |
| `set_user_request_context(user)` | 0004:104 | DEF | Reject nil; set user GUC; set workspace GUC to `''` | `rls_context.rs:49` (`activate_user_context`) |
| `get_current_user_email_hash()` | 0004:128 | DEF, STABLE | Caller's stored `users.email_hash`; NULL on bad GUC | Invitation policy; `workspace_member_repository.rs:941,1039,1138,1201,1243` (raw SQL) |
| `get_current_user_workspace_ids()` | 0004:144 | DEF, STABLE | Membership workspace ids ∪ owned workspace ids; `[]` on bad GUC | Policies for workspaces, memberships, invitations, workspace audit select, workspace files |
| `get_current_workspace_user_ids()` | 0004:163 | DEF, STABLE | User ids with a membership in `app.current_workspace_id`; `[]` on bad GUC | `users` policy |
| `get_current_user_invited_workspace_ids()` | 0004:178 | DEF, STABLE | Workspace ids of non-expired invitations to the caller's email hash | `workspaces` policy |

### `sp_public_*` (`0005`) — all DEF

| Function | Line | Purpose | Repository method → service |
|---|---|---|---|
| `sp_public_find_user_by_email_hash(hashes[])` | 54 | User row by any of the hashes | `public_user_repository.rs:128 find_by_email_for_auth` → public_auth, identity, user, workspace_member services |
| `sp_public_find_user_by_email_with_credentials_hash(hashes[])` | 88 | User row plus credentials `password_hash` (LEFT JOIN, NULL if none) | `:188` → public_auth (credentials login) |
| `sp_public_find_user_by_oauth_identity(method, subject)` | 124 | User by `(method, provider_subject)` | `:255` → public_auth, identity |
| `sp_public_find_credentials_identity_by_user_id(user)` | 161 | Credentials identity row | `:310` → identity, public_auth, user |
| `sp_public_insert_identity(...)` | 194 | Insert identity row (registration) | `:449 insert_identity_in_tx` → public_auth |
| `sp_public_create_user(...)` | 217 | Insert user, return row | `:397 create_user_in_tx` → public_auth |
| `sp_public_ingest_user_avatar(...)` | 255 | Insert public `user_files` row and point `users.avatar_file_id` at it | `user_file_repository.rs:389 ingest_avatar` → public_auth |
| `sp_public_find_public_user_file(id)` | 282 | File row only if `visibility='public'` | `user_file_repository.rs:347 find_public` → public_file |
| `sp_public_find_public_workspace_file(id)` | 303 | Same for workspace files | `workspace_file_repository.rs:398` → public_file |
| `sp_public_create_workspace(...)` | 329 | Insert workspace, return row | `public_workspace_repository.rs:53 create_default_workspace_in_tx` → **no production caller** |
| `sp_public_create_owner_membership(...)` | 359 | Insert membership | `public_workspace_member_repository.rs:58` → workspace_service::create_workspace |
| `sp_public_create_user_preferences(...)` | 377 | Insert preferences (consent defaults FALSE) | `user_preferences_repository.rs:66 create_in_tx` → public_auth |
| `sp_public_update_user_preferences(user, ws, at)` | 393 | Set `default_workspace_id` | `:156 update_default_workspace_in_tx` → public_auth |
| `sp_public_clear_user_default_workspace(user, at)` | 410 | Null `default_workspace_id`, return row count | `:197` → public_auth |
| `sp_public_find_session_by_token_hash(hashes[])` | 432 | Session by access-token hash | `public_session_repository.rs:97 find_by_access_token` → public_auth |
| `sp_public_upsert_session(...)` | 464 | Insert or replace session per `(user, device)` | `:264 create_session_in_tx` → public_auth::create_session |
| `sp_public_update_session_tokens(id, old_refresh, ...)` | 538 | CAS rotation on current refresh hash; stores old hash in previous slot; returns row count | `:321` (0 → NotFound) → public_auth refresh |
| `sp_public_find_session_by_refresh_token_hash(hashes[])` | 570 | Session by refresh hash | `:148` → public_auth |
| `sp_public_find_session_by_previous_refresh_token_hash(hashes[])` | 613 | Session by previous refresh hash (reuse detection) | `:188` → public_auth |
| `sp_public_terminate_session_for_reuse(id)` | 653 | Delete session, return row count | `:230` (count ignored) → public_auth |
| `sp_public_get_user_by_id(id)` | 674 | User row (no credentials) | `public_user_repository.rs:351 find_by_id_for_auth` → public_auth |
| `sp_public_get_user_preferences(user)` | 697 | Preferences row | `user_preferences_repository.rs:116` → public_auth |
| `sp_public_validate_workspace_access(user, ws)` | 716 | TRUE if a membership row exists (ownership alone does not count) | `public_workspace_repository.rs:108` → auth middleware, handlers/auth, public_auth |
| `sp_public_get_workspace_permissions(user, ws)` | 733 | Membership `permissions`, or `[]` for non-members | `:204` → auth middleware, handlers/auth, public_auth, workspace_service |
| `sp_public_get_workspace_by_id(ws)` | 750 | Workspace row | `:149` → public_auth |
| `sp_public_get_user_accessible_workspaces(user)` | 770 | Workspaces joined through membership, `created_at ASC` | `:258` → public_auth (login, SSE connect) |
| `sp_public_accept_workspace_invitation(...)` | 801 | Upsert membership (overwrites permissions on conflict), delete invitation by id | `public_workspace_member_repository.rs:123` → user_invitation_service::accept_invitation |
| `sp_public_find_authored_invitations_in_unowned_workspaces(user)` | 850 | Invitations the user sent in workspaces they do not own (STABLE) | `:212` → user_service::delete_user |
| `sp_public_upsert_oauth_tokens(...)` | 876 | Insert or replace provider token per session | `public_oauth_token_repository.rs:75` → public_auth |
| `sp_public_find_oauth_tokens_by_session(session)` | 911 | Token of one session | `:114` → auth/revocation.rs |
| `sp_public_find_oauth_tokens_by_user_excluding_session(user, session)` | 958 | Tokens of the user's other live sessions | `:155` → auth/revocation.rs, identity_service |
| `sp_public_find_oauth_tokens_by_user(user)` | 1006 | Every token of the user, live or not | `:201 find_all_by_user` → auth/revocation.rs (account deletion) |
| `sp_public_find_oauth_tokens_for_expired_sessions(cutoff)` | 1057 | Live-at-provider tokens of sessions expiring before cutoff | `:243` → public_auth::cleanup_expired_sessions |
| `sp_public_upsert_verification_code(...)` | 1087 | Insert or replace code per `(user, purpose)`, resetting attempts | `public_verification_code_repository.rs:64` → public_auth |
| `sp_public_find_verification_code(user, purpose)` | 1125 | Code row | `:104` → auth/verification_code_validator.rs |
| `sp_public_increment_verification_attempts(user, purpose, max)` | 1152 | Atomic `attempts_used+1` only while `< max`; NULL at the ceiling | `:144` → verification_code_validator |
| `sp_public_complete_email_verification(user, at)` | 1175 | Set `email_verified` (if FALSE), delete the `email_verification` code; returns BOOLEAN | `:183` (BOOLEAN ignored) → public_auth |
| `sp_public_complete_password_reset(user, hash, at)` | 1225 | Update credentials hash; delete reset code, pending email change and `email_change` code; delete live sessions; return their ids, or no row if no credentials identity | `:224 complete_password_reset_in_tx` → public_auth::reset_password |
| `sp_public_change_password(user, hash, current_session, at)` | 1329 | As above but keeps the current session and leaves the reset code | `public_user_repository.rs:488` → user_service::change_password |
| `sp_public_initiate_email_change(...)` | 1414 | Upsert pending row then `email_change` code, both ids regenerated | `public_verification_code_repository.rs:274` → user_service |
| `sp_public_complete_email_change(user, expected_code_id, at)` | 1495 | Lock pending row; abort if the code id changed; delete pending and code; promote address, `email_verified=TRUE`; return new ciphertext | `:332` → user_service::confirm_email_change |
| `sp_public_unlink_credentials(user, current_session)` | 1611 | Lock identities; no row if no credentials; refuse if last identity; else delete it and other live sessions | `public_user_repository.rs:532` → identity_service::unlink_credentials |
| `sp_public_unlink_oauth_identity(user, method, current_session)` | 1753 | Same for an OAuth method; deletes only other live sessions of that method | `:578` → identity_service::unlink_oauth_identity |

### `sp_system_*` key-rotation procedures (`0005`) — all DEF

| Function pair | Lines | CAS guard | Caller |
|---|---|---|---|
| `sp_system_re_encrypt_fetch_users` / `_update_user(id, email, name, email_hash, old_email, old_name)` | 1856, 1868 | email and name | `re_encryption.rs:250,299` |
| `..._fetch_oauth_tokens` / `_update_oauth_token(id, new, old)` | 1889, 1901 | yes | `re_encryption.rs:335,366` |
| `..._fetch_workspaces` / `_update_workspace(id, name, old_name)` | 1918, 1930 | yes | `re_encryption.rs:398,429` |
| `..._fetch_workspace_invitations` / `_update_workspace_invitation(id, email, email_hash)` | 1944, 1956 | no (justified in comment) | `re_encryption.rs:461,493` |
| `..._fetch_pending_email_changes` / `_update_pending_email_change(id, email, hash, old)` | 1980, 1992 | yes | `re_encryption.rs:537,569` |
| `..._fetch_workspace_audit_logs` / `_update_workspace_audit_log(id, old, new, meta)` | 2010, 2022 | no | `re_encryption.rs:609,647` |
| `..._fetch_user_audit_logs` / `_update_user_audit_log(...)` | 2037, 2049 | no | `re_encryption.rs:686,724` |
| `..._fetch_sessions` / `_update_session(id, name, old)` | 2075, 2087 | yes | `re_encryption.rs:766,802` |
| `..._fetch_user_files` / `_update_user_file(id, name)` | 2106, 2118 | no | `re_encryption.rs:837,868` |
| `..._fetch_workspace_files` / `_update_workspace_file(id, name)` | 2129, 2141 | no | `re_encryption.rs:899,930` |

Every fetch takes `(p_cursor UUID, p_limit INT)` and returns `id > p_cursor ORDER BY id LIMIT p_limit`. The limit is bound as `i32` because PostgreSQL will not narrow `bigint` to `int` during function resolution (`re_encryption.rs:10-17`). `idempotency_cache` is re-encrypted with raw SQL (`re_encryption.rs:963-989`).

### Background-task procedures (`0006`)

| Function | Line | Mode | Purpose | Repository method → caller |
|---|---|---|---|---|
| `sp_cleanup_expired_sessions(cutoff)` | 50 | DEF | Delete sessions with `refresh_token_expires_at < cutoff` | `public_session_repository.rs:370` → DATA-038 |
| `sp_cleanup_expired_invitations()` | 76 | DEF | Delete invitations with `expires_at < NOW()` | `public_workspace_member_repository.rs:178` → DATA-038 |
| `sp_cleanup_expired_verification_codes()` | 104 | DEF | Delete codes with `expires_at < NOW()` | `public_verification_code_repository.rs:405` → DATA-038 |
| `sp_cleanup_expired_pending_email_changes()` | 133 | DEF | Delete pending changes with `expires_at < NOW()` | `public_verification_code_repository.rs:372` → DATA-038 |
| `sp_cleanup_expired_idempotency_entries()` | 167 | DEF | Delete completed > 24 h, processing > 5 min | `idempotency_repository.rs:384` → DATA-040 |
| `sp_cleanup_expired_workspace_audit_logs(days)` | 198 | DEF | Delete rows older than `days` | `workspace_audit_log_repository.rs:443` → AUDIT-021 |
| `sp_cleanup_expired_user_audit_logs(days)` | 225 | DEF | Same | `user_audit_log_repository.rs:413` → AUDIT-021 |
| `sp_cleanup_expired_sse_events(hours DEFAULT 1)` | 252 | DEF | Delete events older than `hours` | `sse_event_repository.rs:119` → RT-025 |
| `sp_cleanup_expired_rate_limits()` | 279 | INV | Delete windows that ended | `rate_limit_repository.rs:141` → DATA-039 |
| `sp_ratelimit_increment(key, window_secs, amount)` | 314 | INV | Fixed-window upsert; resets when the window ended; returns count, window start, window secs | `rate_limit_repository.rs:63` → rate_limit_service (HTTP) |
| `sp_task_acquire_lock(task, instance, secs)` | 378 | INV | Take or re-take a task lock | `background_task_lock_repository.rs:61` → RT-029 |
| `sp_task_release_lock(task, instance)` | 414 | INV | Release own lock | `background_task_lock_repository.rs:96` → RT-030 |
| `sp_object_deletion_claim_batch(limit, backoff, max)` | 452 | INV | Claim eligible queue rows | `object_deletion_queue_repository.rs:45` → FILE-036 |
| `sp_object_deletion_complete(id)` | 483 | INV | Delete queue row | `:87` → FILE-035 |
| `sp_object_deletion_record_error(id, error)` | 503 | INV | Set `last_error` | `:117` → FILE-035 |
| `sp_object_deletion_count_exhausted(max)` | 528 | INV | Count rows at the ceiling | `:145` → FILE-037 |

`rate_limit_repository.rs:116 reset` deletes a `rate_limits` key with raw SQL; it is not a procedure.

### Trigger functions (`0007`) — both INV

| Function | Line | Bound by | Purpose |
|---|---|---|---|
| `notify_sse_event_insert()` | 18 | `sse_event_notify_trigger AFTER INSERT ON sse_events FOR EACH ROW` (0007:37) | `pg_notify('sse_event_created', {id, event_type, scope_type, scope_id, created_at})` (RT-022) |
| `enqueue_object_deletion()` | 54 | `user_files_enqueue_deletion`, `workspace_files_enqueue_deletion AFTER DELETE ... FOR EACH ROW` (0007:63,68) | Insert `OLD.storage_key` into `pending_object_deletions` (FILE-016) |

## Data owned

- **Schema as a whole:** every table in `## Schema`, its constraints, indexes and FK actions, and the `uuid-ossp` extension. Business owners per table are named in each table heading.
- **Migrations:** `migrations/0001`..`0008`, the `migrate` command, `_sqlx_migrations`.
- **Roles and grants:** `app_user` (created by `scripts/db-bootstrap.sh`), all of `0008_grants.sql`.
- **RLS:** enabled on 14 tables (`0004:33-46`); 24 policies:

| Table | Policy | Command | USING | WITH CHECK |
|---|---|---|---|---|
| users | `users_workspace_access` | ALL | `id = me OR id ∈ workspace_user_ids()` | (same) |
| workspaces | `workspaces_access_control` | ALL | `owner_id = me OR id ∈ my_workspace_ids() OR id ∈ invited_workspace_ids()` | (same) |
| sessions | `sessions_user_access` | ALL | `user_id = me` | (same) |
| user_identities | `user_identities_user_access` | ALL | `user_id = me` | `user_id = me` |
| workspace_memberships | `workspace_memberships_access` | ALL | `workspace_id ∈ my_workspace_ids()` | (same) |
| user_preferences | `user_preferences_access` | ALL | `user_id = me` | (same) |
| workspace_invitations | `workspace_invitations_access` | ALL | `workspace_id ∈ my_workspace_ids() OR email_hash = my_email_hash()` | `workspace_id ∈ my_workspace_ids()` |
| oauth_tokens | `oauth_tokens_user_access` | ALL | `user_id = me` | (same) |
| verification_codes | `verification_codes_user_policy` | ALL | `user_id = me` | (same) |
| pending_email_changes | `pending_email_changes_user_policy` | ALL | `user_id = me` | (same) |
| workspace_audit_logs | `workspace_audit_logs_insert` | INSERT | | `workspace_id = app.current_workspace_id` |
| workspace_audit_logs | `workspace_audit_logs_select` | SELECT | `workspace_id ∈ my_workspace_ids()` | |
| user_audit_logs | `user_audit_logs_insert` | INSERT | | `user_id = me` |
| user_audit_logs | `user_audit_logs_select` | SELECT | `user_id = me` | |
| user_files | `user_files_select` | SELECT | `user_id = me OR visibility='public'` | |
| user_files | `user_files_insert` | INSERT | | `user_id = me` |
| user_files | `user_files_update` | UPDATE | `user_id = me` | `user_id = me` |
| user_files | `user_files_delete` | DELETE | `user_id = me` | |
| workspace_files | `workspace_files_select` | SELECT | `workspace_id ∈ my_workspace_ids() OR visibility='public'` | |
| workspace_files | `workspace_files_insert` | INSERT | | `workspace_id ∈ my_workspace_ids()` |
| workspace_files | `workspace_files_update` | UPDATE | `workspace_id ∈ my_workspace_ids()` | (same) |
| workspace_files | `workspace_files_delete` | DELETE | `workspace_id ∈ my_workspace_ids()` | |

  `me` = `current_setting('app.current_user_id', true)::UUID`. Source: `migrations/0004_row_level_security.sql:197-347`. Audit policies are detailed in AUDIT-018/AUDIT-019.
- **Stored procedures:** the context setters and helpers, all `sp_system_*`, all `sp_cleanup_*` (the sweeps they run belong to the tasks above), and the catalog entries as SQL objects. Business semantics of `sp_public_*`, `sp_ratelimit_*`, `sp_task_*`, `sp_object_deletion_*` belong to their callers' domains.
- **Triggers:** `sse_event_notify_trigger` (behavior RT-022), `user_files_enqueue_deletion` and `workspace_files_enqueue_deletion` (behavior FILE-016).
- **Background tasks (bodies):** `auth_cleanup` (DATA-038), `rate_limit_cleanup` (DATA-039), `idempotency_cleanup` (DATA-040), `re_encryption` (DATA-041, DATA-047). Elsewhere: `audit_log_cleanup` (AUDIT-021), `sse_event_cleanup` (RT-025), `object_deletion_reaper` (FILE-035). All run in-app from `src/server.rs` through `background::spawn_background_task`; there is no `pg_cron` or other database scheduler.
- **Code:** `src/migrate.rs`, `src/repositories/postgresql/{transaction,rls_context,keyset}.rs`, `src/repositories/traits.rs`, `src/encryption/*`, `src/utils/{database_retry,db_pool_metrics}.rs`, `src/config/database.rs`.

## Configuration

| Variable | Read by | Default | Validation |
|---|---|---|---|
| `DATABASE_URL` | server (runtime, `app_user`); `migrate` (admin DSN) | none; Compose builds `postgresql://app_user:${APP_DB_PASSWORD}@postgres:5432/${COMPOSE_PROJECT_NAME:-app}` | server: must be non-empty; migrate: non-empty and parsable, 30 s connect timeout |
| `MIGRATE_DATABASE_URL` | `db-bootstrap`; passed to `db-migrate` as `DATABASE_URL` | Compose: `postgresql://postgres:${POSTGRES_ADMIN_PASSWORD:-postgres-dev-only-change-in-prod}@postgres:5432/...` | non-empty (bootstrap) |
| `APP_DB_PASSWORD` | `db-bootstrap` | none | required (`:?` in Compose, FATAL in script) |
| `POSTGRES_ADMIN_PASSWORD` | Compose `postgres`, admin DSNs | `postgres-dev-only-change-in-prod` | none |
| `DATABASE_MAX_CONNECTIONS` | pool | 10 | > 0 |
| `DATABASE_ACQUIRE_TIMEOUT_SECS` | pool | 10 | > 0 |
| `APP_DB_ENCRYPTION_KEY_1`..`_255` | key ring | none | at least one; each 64 hex chars; panic otherwise |
| `APP_DB_ACTIVE_KEY_VERSION` | key ring | lowest version present | 1-255 and present in the ring; panic otherwise |
| `ENABLE_BACKGROUND_CLEANUP` | task plan | true | boolean |
| `CLEANUP_INTERVAL_HOURS` | `auth_cleanup` | 6 | > 0; warning above 168 |
| `RATE_LIMIT_CLEANUP_INTERVAL_HOURS` | `rate_limit_cleanup` | 1 | > 0 |
| `IDEMPOTENCY_CLEANUP_INTERVAL_HOURS` | `idempotency_cleanup` | 1 | > 0; warning above 24 |
| `RE_ENCRYPTION_INTERVAL_HOURS` | `re_encryption` | 6 | > 0 |
| `BACKGROUND_TASK_LOCK_DURATION_SECS` | all tasks (RT) | 300 | > 0 |
| `AUDIT_LOG_RETENTION_DAYS`, `AUDIT_LOG_CLEANUP_INTERVAL_HOURS` | AUDIT-021 | 365, 24 | > 0; warnings below 30 / above 3650 days, above 168 hours |
| `SSE_EVENT_RETENTION_HOURS`, `SSE_EVENT_CLEANUP_INTERVAL_HOURS` | RT-025 | 1, 1.0 | see RT-026 |
| `STORAGE_DELETION_REAPER_INTERVAL_SECS`, `_BATCH_SIZE`, `_RETRY_BACKOFF_SECS`, `_MAX_ATTEMPTS` | FILE-038 | 300, 100, 600, 10 | interval > 0, batch > 0, attempts > 0, backoff ≥ 0 (only when storage is enabled) |
| `REFRESH_TOKEN_LIFETIME_SECS` | session expiry columns (AUTH) | 2 592 000 | > 0, greater than the access lifetime |

Fixed in code, not configurable: re-encryption batch size 100 (`re_encryption.rs:17`); pool `idle_timeout` 600 s and `max_lifetime` 1800 s (`config/database.rs:20-21`); idempotency TTLs 24 h / 5 min (SQL); retry attempts 3 and backoff 2 s, 4 s (`database_retry.rs:153`, `retry.rs:204-217`); shutdown wait 10 s per task (`server.rs:2201`). Source: `src/config/environment.rs:346-350,403-427,490-499,570-586,631-735,900-946,1140-1162,1440-1480`.

## Cross-domain dependencies

- **Calls out:** OBS (tracing spans and `component`/`subsystem` log fields on every repository method; the meter provider for DATA-035), RT (task runner, `run_with_distributed_lock`, `instance_id`), AUTH (`auth/revocation.rs` and the OAuth provider factory used by `auth_cleanup` to revoke provider tokens), HTTP (`ApiError` envelope for DATA-029; pagination types used by keyset SQL), OPS (Compose one-shots, `deploy.sh`, `.env`).
- **Called by:** every service through the repository traits (AUTH: sessions, identities, codes, provider tokens; USER: users, preferences, email change, account deletion; WS: workspaces, memberships, invitations; FILE: file metadata, public-file reads, avatar ingest, deletion queue; AUDIT: audit inserts, reads, retention; RT: `sse_events`, task locks; HTTP: `rate_limits`, `idempotency_cache`). The auth middleware calls `sp_public_validate_workspace_access` and `sp_public_get_workspace_permissions` on every workspace-scoped request.

## Observations

1. **RLS does not constrain a compromised or misused app role.** `app_user` may call `set_user_request_context` / `set_request_context` with any UUID (no membership or existence check, `0004:72-117`), and may call DEFINER procedures that return every tenant's rows, e.g. `sp_system_re_encrypt_fetch_users` (email and name ciphertext of all users, `0005:1856`) or `sp_public_find_oauth_tokens_for_expired_sessions`. RLS therefore guards against query bugs in correctly-contexted code, not against SQL injection or a stolen `app_user` password. The `0004` header describes it as defense in depth, which matches.
2. **EXECUTE is not revoked from `PUBLIC`.** PostgreSQL grants EXECUTE on new functions to `PUBLIC` by default and no migration revokes it, so every role in the cluster can call the DEFINER functions; the per-function grants to `app_user` (`0008:51-168`) are redundant.
3. **RLS enforces membership, not permissions.** Any member passes RLS for UPDATE/DELETE of the workspace, for INSERT/UPDATE/DELETE of any membership (including raising their own permissions), and for any invitation write in the workspace (`0004:207-254`). Permission checks exist only in services.
4. **A pending invitee can update or delete the workspace at the RLS level.** `workspaces_access_control` is `FOR ALL` with no `WITH CHECK`, and its USING includes `get_current_user_invited_workspace_ids()` (`0004:207-215`). The visibility was intended (an invitee sees the name); the write side looks unintended.
5. **Co-members' `users` rows are writable under a workspace context** (`0004:198-204`, FOR ALL). Repositories only write users under user scope, so this is latent.
6. **Two definitions of "has access".** The RLS helper counts owned workspaces even without a membership row (`0004:144-160`); `sp_public_validate_workspace_access`, `sp_public_get_user_accessible_workspaces` and `sp_public_get_workspace_permissions` count membership only (`0005:716-789`). They agree only while every owner also has a membership.
7. **Workspace audit inserts check only the GUC.** `workspace_audit_logs_insert` requires `workspace_id = app.current_workspace_id` with no membership check and no check of `user_id` (`0004:277-279`); post-cascade audit writes rely on this (AUDIT-020).
8. **Empty-string GUCs.** `set_user_request_context` stores `''` in `app.current_workspace_id`, and per PostgreSQL semantics a transaction-local custom setting reads back as `''` (not NULL) on that connection after the transaction ends. The helper functions catch the resulting cast error, but the inline `current_setting(...)::UUID` casts in most policies do not, so a statement on an RLS table without context on a previously used connection would raise 22P02 (→ 500) rather than return zero rows. `with_public_context` documents this case as "the default RLS deny path" (`transaction.rs:168-174`). Not exercised by any code path found; it fails closed either way.
9. **Comment says FORCE RLS would break functions because of "ambiguous column references"** (`0004:21-24`). The actual dependency is that DEFINER functions run as the table owner, who bypasses RLS only without FORCE. In the bundled stack the owner is the `postgres` superuser, which bypasses RLS regardless.
10. **Header claims that do not match the SQL.** `0008:9-11` says the `0007` trigger functions are SECURITY DEFINER; both are INVOKER (`0007:32,61`). `0006:18-24` says every Section 1 cleanup function is DEFINER with a pinned `search_path`; `sp_cleanup_expired_rate_limits` is INVOKER with none (`0006:279-292`). `0005:6-10` describes `sp_public_*` as pre-authentication; about a third are called only from authenticated flows (change password, unlink, email change, invitation acceptance, owner membership in `POST` workspace creation, authored invitations); `0005:1309-1312` acknowledges this for some.
11. **Dead SQL and code.**
    - `sp_public_create_workspace` and `PublicWorkspaceRepository::create_default_workspace_in_tx` have no production caller (`0005:329-353`, `public_workspace_repository.rs:53`); registration creates no workspace.
    - `clear_request_context()` has no caller (`0004:90-96`).
    - `AuthenticatedUserRepository::create` / `Repository<User>::create`, and `Repository<Workspace>::create` / `AuthenticatedWorkspaceRepository::create`, have no production caller (workspace creation uses `create_in_tx`).
    - `user_files_update` and `workspace_files_update` policies exist, but no application UPDATE of file rows exists; only the re-encryption DEFINER procedures update them.
    - `GRANT USAGE ON ALL SEQUENCES` and its default-privilege twin grant on no objects; there are no sequences (`0008:40,44`).
    - Columns never read by Rust: `background_task_locks.locked_at`, `pending_object_deletions.enqueued_at`, `last_attempt_at`, `last_error` (read only inside SQL or by operators). `users.email_hash` is read into `#[allow(dead_code)]` fields (`user_repository.rs:42`, `public_user_repository.rs:53,70`).
    - No SQL filters on `sessions.expires_at` or `sessions.device_fingerprint` alone, so `idx_sessions_expires` and `idx_sessions_device_fingerprint` (`0003:34,39`) appear unused. `idx_workspace_memberships_user`, `idx_workspace_memberships_workspace` and `idx_workspace_invitations_workspace` duplicate prefixes of UNIQUE or composite indexes (`0003:70-71,76`).
12. **Procedure results ignored.** `sp_public_complete_email_verification` returns BOOLEAN but `complete_email_verification` returns `()` (`public_verification_code_repository.rs:183-197`). `sp_public_terminate_session_for_reuse` returns a row count "so the Rust layer can detect a race" (`0005:651-652`), and the repository discards it (`public_session_repository.rs:230-245`).
13. **Duplicated rules, SQL and Rust.**
    - Email verification has two writers: `sp_public_complete_email_verification` (also deletes the code) and `AuthenticatedUserRepository::mark_email_verified` (raw SQL, `user_repository.rs:625-664`, used by `identity_service.rs:666`).
    - The "session alive" predicate `refresh_token_expires_at > NOW()` is repeated in `sp_public_find_oauth_tokens_by_user_excluding_session`, `sp_public_complete_password_reset`, `sp_public_change_password`, both unlink SPs, and three queries in `session_repository.rs` (`find_by_user`, `find_user_session_ids`, `revoke_others`); the service checks `<= now()` and cleanup uses `< cutoff`.
    - Nil-UUID rejection exists in Rust (`transaction.rs:73,127`) and SQL (`0004:76-82,108-110`); each `_in_tx` method re-implements the Rust checks instead of using `require_workspace_id` (e.g. `workspace_audit_log_repository.rs:492-516`).
    - Repositories add explicit `user_id` / `workspace_id` filters that duplicate the policies (e.g. `user_file_repository.rs:311`, `workspace_file_repository.rs:362`, `session_repository.rs:226`).
14. **`_in_tx` methods switch the RLS context mid-transaction** (DATA-028). Any statement after an audit `log_event_in_tx` in a `with_public_context` or `with_user_context` transaction runs under the audit method's context.
15. **Re-encryption reporting.** `processed` counts a row even when the CAS guard made the update a no-op (`re_encryption.rs:299-318`; the SPs return `void`). `DEPLOYMENT.md:222` tells operators to retire a key after seeing "Re-encryption cycle completed", but that INFO line is logged only for cycles that processed rows; a cycle that finds nothing to do logs at DEBUG only (`server.rs:1926-1952`).
16. **Unguarded re-encryption updates.** `idempotency_cache` is updated with raw SQL and no CAS (`re_encryption.rs:988-993`): if an entry is deleted and recreated with the same key between the batch read and the update, the stale body is written over the new one. The file-table and audit-log updates are unguarded too, but nothing else updates those columns.
17. **Blind indexes diverge during a rotation.** Stored-hash-to-stored-hash comparisons use a single hash: the RLS email branch (`get_current_user_email_hash()`), `get_user_pending_invitations` (`workspace_member_repository.rs:941`), `users_email_hash_unique` and `workspace_invitations_workspace_email_unique`. While a user row and an invitation are hashed under different versions, the invitee does not see the invitation, and a re-invite inserts a second row for the same email instead of updating. When re-encryption then recomputes the older row's hash, its update hits 23505, which is a permanent error, so that row is counted `failed` on every cycle (DATA-048).
18. **Comment overstates atomicity.** `re_encryption.rs:174-175` says users and invitations "share email_hash — process atomically per user"; they are separate sequential table passes with one autocommit statement per row.
19. **No associated data in AES-GCM** (`database_encryption.rs:141,238`): a ciphertext copied to another row or column decrypts without error.
20. **Retention values are narrowed with `as i32`.** `AUDIT_LOG_RETENTION_DAYS` (u64) is bound `retention_days as i32` (`user_audit_log_repository.rs:422`, `workspace_audit_log_repository.rs:452`). A value whose low 32 bits read as negative wraps to a negative number of days, making the cutoff lie in the future, so every audit row would be deleted. Validation only warns above 3650. `SSE_EVENT_RETENTION_HOURS` is narrowed the same way (`sse_event_repository.rs:123`).
21. **Retry documentation mismatch.** `database_retry.rs:14-17` says "Max retries: 3 ... (2s, 4s, 8s)"; the loop makes 3 attempts in total with two sleeps of 2 s and 4 s (`database_retry.rs:153-197`).
22. **Most constraint and policy violations surface as 500.** Only 23505 and 23503 become 409; CHECK violations (e.g. `user_identities_shape`, file visibility), NOT NULL, and RLS `WITH CHECK` violations (42501) become `QueryFailed` → 500 (`repository_error.rs:879-887`).
23. **No `updated_at` maintenance in the database.** No trigger sets it, and several writes leave it unchanged: `sp_public_ingest_user_avatar` changes `users.avatar_file_id` without touching `users.updated_at` (`0005:268`), and the re-encryption procedures rewrite ciphertext without touching it (`0005:1868-2146`). `sessions` and `verification_codes` have no `updated_at` at all. `user_files.updated_at` and `workspace_files.updated_at` never change after insert.
24. **Avatar and picture pointers are not owner-checked by the database.** `users.avatar_file_id` may reference any user's `user_files` row and `workspaces.picture_file_id` any workspace's file, public or private (`0002:672,681`); the lint test comment at `tests/lint_definer_sp_scoping_coverage.rs:116-120` says the same. Only the repositories' swap methods keep them owner-consistent.
25. **`app_user` likely holds DML on `_sqlx_migrations`.** The migrator creates `_sqlx_migrations` in `public` before applying `0008` (`migrate.rs:107,120`), and `0008:37` grants DML on all tables in `public`, so the runtime role can probably edit migration bookkeeping. Inferred from the order of operations; not verified against a database.
26. **`db-bootstrap` does not normalize a pre-existing role.** If `app_user` already exists with BYPASSRLS or SUPERUSER (e.g. created by hand on a managed cluster), the script keeps those attributes and RLS is silently off (`db-bootstrap.sh:52-62`).
27. **Key-ring failure panics.** Startup uses `.expect("Failed to load encryption key ring")` (`server.rs:1114-1118`) rather than returning a configuration error like the other startup checks.
28. **Inconsistent DDL style.** Four infrastructure tables use `CREATE TABLE IF NOT EXISTS` and one index uses `CREATE INDEX IF NOT EXISTS`; every other object uses plain `CREATE` (`0002:444,561,592,623`, `0003:143`). `0007:34-36` says AFTER INSERT "ensures event is committed before notification"; NOTIFY delivery is deferred to commit for BEFORE triggers too.
29. **Documentation check (`documentation/DATABASE.md`).** Its claims about the three-stage chain, immutability, the admin/runtime split and one-shot migrations match the code. "Migrations run as the superuser" (`DATABASE.md:244`) holds for the bundled stack only; managed mode uses the provider's admin role. The example `migrations/0012_add_user_phone.sql` skips 0009-0011 (only 0001-0008 exist), which is harmless as an example.
30. **Re-encryption runs only when two or more keys are present.** With a single key, data written under a now-removed version cannot be detected or reported; the task is not even spawned (`server.rs:794`).

## Open questions

1. Should RLS be a security boundary against the app role itself (Observations 1, 2), or is defense against query bugs the intended scope?
2. Should RLS enforce workspace permissions, or at least stop pending invitees from updating or deleting a workspace and members from rewriting memberships (Observations 3, 4)?
3. Does ownership without a membership row count as access? RLS says yes; the auth procedures say no (Observation 6).
4. Is logging out every session that has not refreshed since a key switch an accepted cost of key retirement (DATA-050)? Is the mid-rotation blind-index divergence for invitations and uniqueness acceptable (Observation 17)?
5. Should the re-encryption task report CAS no-ops separately and emit a distinct "rotation complete" signal that operators can rely on before retiring a key (Observation 15)?
6. Should an out-of-range `AUDIT_LOG_RETENTION_DAYS` be rejected instead of wrapping (Observation 20)?
7. Is zero-workspace registration final, so `sp_public_create_workspace` can go (Observation 11)?
8. Must migrations stay compatible with the previous release's running replicas during a rolling deploy (DATA-007)?
9. Should CHECK, NOT NULL and RLS `WITH CHECK` violations map to 4xx rather than 500 (Observation 22)?
10. Should the database enforce that avatar and picture pointers reference the owner's own file (Observation 24)?
11. Is the authenticated `mark_email_verified` path or the procedure the intended single writer of `email_verified` (Observation 13)?
12. Should accepting an invitation while already a member overwrite that member's permissions (DATA-023)?
