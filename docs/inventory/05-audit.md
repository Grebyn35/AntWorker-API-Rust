# Inventory: Audit logs (`AUDIT`)

Reference: `AntWorker-API` @ `0210895` (2026-10-20). This file records what the old API does. It does not prescribe what the new one should do.

## Scope

The two audit trails (`user_audit_logs` and `workspace_audit_logs`): the action vocabulary, how an entry is built and written (in the caller's transaction or after commit), what each entry holds (actor, target, old/new values, metadata, client IP, user agent, trace id), field-level encryption, the two list endpoints with their filters and pagination, the public action-discovery endpoint, retention cleanup, immutability, RLS on the two tables, and PII content.

Boundary. The routes that write audit entries belong to the domains that own those routes (AUTH, USER, WS). This file lists each emission once, in the **Audit action catalog**, and does not describe the business flows themselves. The machinery that audit reuses is owned elsewhere: the generic keyset pagination and the error envelope (HTTP), the request-context middleware and client-IP extraction (HTTP), the RLS context helpers, the background-task runner with its distributed lock, key rotation and re-encryption (DATA), and trace-id extraction (OBS). This file describes how audit uses each of them.

Files read in full: `src/handlers/{audit_actions,user_audit_logs,workspace_audit_logs,mod}.rs`, `src/services/{user_audit_log_service,workspace_audit_log_service}.rs`, `src/models/{audit,audit_actions}.rs`, `src/repositories/postgresql/{user_audit_log_repository,workspace_audit_log_repository,rls_context,keyset}.rs`, `src/middleware/request_context.rs`, `tests/audit_insert_no_returning_live.rs`. Read in part: the audit SQL in `migrations/0002`–`0008`; every emitter in `src/services/{public_auth_service,session_service,identity_service,user_service,user_invitation_service,workspace_service,workspace_member_service}.rs`; `src/repositories/postgresql/transaction.rs`; `src/utils/{request,trace,pagination}.rs`; `src/validation/mod.rs`; `src/server.rs` (router layering, the audit cleanup task, the background plan); `src/background.rs`; `src/config/environment.rs`; `src/models/permissions.rs`; `src/errors/{api_error,response_envelope}.rs`; `src/middleware/rate_limit.rs`; `src/handlers/auth.rs`; `.env.example`; `documentation/*.md`; `README.md`; the audit-related tests in `tests/`. `.env` and `alertmanager-secrets/` exist and were not opened.

## Behaviors

### AUDIT-001 List own user audit log

- **Behavior:** Returns a page of the caller's own `user_audit_logs` entries, the actions the caller performed, newest first by default. Entries about the caller written by someone else never appear, because no such entries exist (see AUDIT-010).
- **Kind:** `capability`
- **Trigger:** `GET /v1/users/me/audit-logs`
- **Auth:** Session (bearer), through `user_scoped_auth_middleware`. No workspace header is needed and no permission is checked. When `REQUIRE_EMAIL_VERIFICATION` is on, `enforce_email_verification_middleware` blocks unverified credentials users. The route is under the per-user rate-limit tier and the idempotency middleware.
- **Input & validation:** Pagination as in AUDIT-007. Filters as in AUDIT-004, AUDIT-005 and AUDIT-006: `action`, `resource_type`, `resource_id`, `from`, `to`. Unknown query keys, `user_id` among them, are ignored without error.
- **Effects:** Read only. The SQL is `WHERE user_id = <ctx.user_id>`, and the `user_audit_logs_select` RLS policy enforces the same scope.
- **Errors:** `AUTHENTICATION_FAILED` → 401 → missing or invalid bearer token. `PERMISSION_DENIED` (`email_verification_required`) → 403 → an unverified credentials user while verification is enforced. `VALIDATION_ERROR` → 400 → a bad pagination, filter or time-range value. `DATA_INTEGRITY_ERROR` → 500 → a row fails to decrypt (AUDIT-017).
- **Guarantees:** User isolation is enforced twice, by the explicit WHERE clause and by RLS. Responses follow the `PaginatedApiResponse` envelope.
- **Source:** `src/handlers/mod.rs:214-217`; `src/handlers/user_audit_logs.rs:36-51`; `src/services/user_audit_log_service.rs:180-195`; `src/repositories/postgresql/user_audit_log_repository.rs:322-400`; `src/server.rs:628-645` (middleware stack); `src/auth/middleware.rs:85`.
- **Tests:** `tests/repository_smoke.rs::user_audit_log_find_by_user_executes_against_schema` (repository only, on an empty DB). No HTTP-level test was found.

### AUDIT-002 List workspace audit log

- **Behavior:** Returns a page of `workspace_audit_logs` entries for the workspace named in `X-Workspace-Id`. The page covers every member's actions, including those from before the caller joined.
- **Kind:** `capability`
- **Trigger:** `GET /v1/workspace/audit-logs`
- **Auth:** Workspace-scoped. `workspace_scoped_auth_middleware` validates `X-Workspace-Id` and membership. Email verification is enforced. The caller needs permission `read:workspace_audit_log` (AUDIT-003).
- **Input & validation:** Pagination as in AUDIT-007. Filters as in AUDIT-004, AUDIT-005 and AUDIT-006: `action`, `user_id` (the actor), `resource_type`, `resource_id`, `from`, `to`.
- **Effects:** Read only. The SQL is `WHERE workspace_id = <ctx.workspace_id>`. The RLS policy `workspace_audit_logs_select` also requires the workspace to be one the caller owns or belongs to.
- **Errors:** The same as AUDIT-001, plus `PERMISSION_DENIED` → 403 → the permission is missing (AUDIT-003). Header and membership failures come from the workspace middleware (HTTP/WS).
- **Guarantees:** Workspace isolation by SQL scope and by RLS.
- **Source:** `src/handlers/mod.rs:389-392`; `src/handlers/workspace_audit_logs.rs:49-69`; `src/services/workspace_audit_log_service.rs:205-219`; `src/repositories/postgresql/workspace_audit_log_repository.rs:347-430`; `src/server.rs:651-668`.
- **Tests:** `tests/repository_smoke.rs::workspace_audit_log_find_by_workspace_executes_against_schema` (exercises every filter bind on an empty DB). No HTTP-level test was found.

### AUDIT-003 Workspace audit read requires `read:workspace_audit_log`

- **Behavior:** A workspace member without `read:workspace_audit_log` gets 403 before any database read. The check lives only in the service layer, because RLS checks membership and not this permission. Both role presets carry the permission: Admin (all permissions) and the read-only Member preset (`read:member` plus `read:workspace_audit_log`).
- **Kind:** `rule`
- **Trigger:** `GET /v1/workspace/audit-logs`
- **Auth:** Workspace-scoped, plus `read:workspace_audit_log`.
- **Errors:** `PERMISSION_DENIED` → 403 → the permission is missing. The message is `Missing required permission: read:workspace_audit_log`.
- **Source:** `src/services/workspace_audit_log_service.rs:212`; `src/models/auth.rs:80-94`; `src/models/permissions.rs:91-92,144-151,207-209,385-390` (roles are client-side presets built from this metadata; the server enforces permissions only).
- **Tests:** `src/services/workspace_audit_log_service.rs::get_audit_logs_rejects_caller_without_read_workspace_audit_log`.

### AUDIT-004 Filter semantics

- **Behavior:** Every filter key may repeat (`?action=a&action=b`). Values within one key are ORed (`col = ANY($n)`), and different keys are ANDed. `from` is inclusive (`created_at >= from`) and `to` is exclusive (`created_at < to`). An empty list adds no clause. On the workspace log, `user_id` filters by actor. The target user of an action cannot be filtered, because it exists only inside the encrypted `old_value` and `new_value` JSON.
- **Kind:** `capability`
- **Trigger:** `GET /v1/users/me/audit-logs`, `GET /v1/workspace/audit-logs`
- **Input & validation:** The query string is parsed with `axum_extra::extract::Query`, which supports repeated keys. `resource_id` and `user_id` must be UUIDs, and `from` and `to` must be RFC 3339 timestamps. A value that does not parse is rejected by the extractor.
- **Errors:** `VALIDATION_ERROR` (`invalid_query_parameter`) → 400 → a value does not parse.
- **Source:** `src/models/audit.rs:210-258,313-370`; `src/repositories/postgresql/user_audit_log_repository.rs:66-96`; `src/repositories/postgresql/workspace_audit_log_repository.rs:68-105`; `src/extractors/query.rs:22-32,102-160,274-291`.
- **Tests:** `tests/repository_smoke.rs::workspace_audit_log_find_by_workspace_executes_against_schema` (binds only). No test checks result correctness.

### AUDIT-005 Filter allowlists and size caps

- **Behavior:** Each `action` value must be an action string of the log being queried: a workspace action sent to the user endpoint is rejected, and the reverse is also rejected. Each `resource_type` value must be one of that log's resource types. For the user log these are `session`, `user`, `user_identity`, `workspace` and `workspace_invitation`. For the workspace log they are `workspace`, `workspace_invitation` and `workspace_member`. Every multi-value filter accepts at most 50 values. The error message names the bad value and lists every valid option.
- **Kind:** `rule`
- **Trigger:** both list endpoints
- **Input & validation:** The 50-value cap applies to `action`, `resource_type`, `resource_id` and `user_id` (`AUDIT_FILTER_VEC_MAX`). Matching against the allowlists is exact and case-sensitive. The allowlists are derived from the action enums, so `workspace.transfer` is valid on both endpoints.
- **Errors:** `VALIDATION_ERROR` → 400, with validation details (`invalid_action`, `invalid_resource_type`, or the length message `Too many <field> filter values (max 50)`).
- **Source:** `src/models/audit.rs:17,216-245,319-357,283-300,387-404`; `src/validation/mod.rs:268-412`; `src/models/audit_actions.rs:355-396,592-625`.
- **Tests:** `src/validation/mod.rs` tests `validate_user_audit_action_list_valid/_invalid`, `validate_workspace_audit_action_list_valid/_invalid`, `validate_user_audit_resource_type_list_valid/_invalid`, `validate_workspace_audit_resource_type_list_*`.

### AUDIT-006 Time-range rule

- **Behavior:** `from` and `to` must be given together or not at all. When both are present, `from < to` must hold and the span may be at most 365 days. The span check uses whole days (`num_days() > 365`), so a span of 365 days 23:59:59 passes.
- **Kind:** `rule`
- **Trigger:** both list endpoints
- **Errors:** `VALIDATION_ERROR` → 400, with code `time_range_must_be_bounded`, `time_range_invalid_order` or `time_range_exceeds_max_duration`.
- **Guarantees:** The comments describe this as a query-cost guard against planner index flips. It does not limit history: a client can still page back through all retained rows by leaving out both bounds.
- **Source:** `src/models/audit.rs:166-176,211,314`; `src/validation/mod.rs:79-119`.
- **Tests:** `src/validation/mod.rs` tests `validate_time_range_business_rules_valid`, `_from_after_to`, `_from_equals_to`, `_exceeds_max_range`, `_exactly_365_days`, `_rejects_only_from`, `_rejects_only_to`, `_no_constraints`.

### AUDIT-007 Keyset pagination and ordering

- **Behavior:** Pages are cursor-based, keyed on `(created_at, id)`. `limit` runs from 1 to 100 and defaults to 25. `sort_by` accepts only `created_at`, which is also the default. `sort_direction` is `asc` or `desc` (default `desc`). The cursors are `after` and `before`, and the two are mutually exclusive. A cursor must carry the same sort field and direction as the request. The repository fetches `limit + 1` rows to compute `has_next` and `has_previous`. The response holds `pagination: {limit, has_next, has_previous, next_cursor, previous_cursor, sort_by, sort_direction, total_count}`, and `total_count` is always null for these endpoints.
- **Kind:** `capability`
- **Trigger:** both list endpoints
- **Input & validation:** `limit`, `after`, `before`, `sort_by`, `sort_direction`. Cursors are base64 JSON `{t: unix-ms, i: id, s: sort field, d: direction}`.
- **Errors:** `VALIDATION_ERROR` → 400 → `limit` is out of range, `sort_by` is not `created_at` (internal code `invalid_cursor`), both cursors are given, a cursor is malformed, or a cursor's sort context does not match the request.
- **Guarantees:** Ordering is stable on ties because `id` is the tiebreak. The cursor timestamp has millisecond precision while the stored value has microsecond precision; see Observations.
- **Source:** `src/handlers/user_audit_logs.rs:42,49`; `src/handlers/workspace_audit_logs.rs:57,67`; `src/utils/pagination.rs:196-222,353-415,451-462,491-509`; `src/repositories/postgresql/keyset.rs:39-94`; `src/repositories/postgresql/user_audit_log_repository.rs:47-57,358-397`; `src/repositories/postgresql/workspace_audit_log_repository.rs:49-59,385-427`; `src/errors/response_envelope.rs:444-458`.
- **Tests:** Generic pagination tests in `src/utils/pagination.rs` and `src/repositories/postgresql/keyset.rs`. No audit-specific pagination test was found.

### AUDIT-008 Entry wire shape

- **Behavior:** Each item carries `id`, `user_id` (the actor), `action`, `resource_type`, `resource_id` (null when absent), `old_value`, `new_value`, `metadata`, `trace_id` and `created_at`. Workspace items also carry `workspace_id`. `old_value`, `new_value` and `metadata` are free-form JSON, decrypted on read, and are left out of the JSON entirely when null. In practice `metadata` is always present, because the server adds `request_context` to every entry (AUDIT-012).
- **Kind:** `capability`
- **Trigger:** both list endpoints
- **Source:** `src/models/audit.rs:37-54,106-122`; `src/repositories/postgresql/user_audit_log_repository.rs:117-172`; `src/repositories/postgresql/workspace_audit_log_repository.rs:127-183`.
- **Tests:** none found.

### AUDIT-009 Audit action discovery endpoint

- **Behavior:** Returns `{user_actions: {<action>: {label, description, resource_type, scope}}, workspace_actions: {...}, version}` under the standard `ApiResponse` envelope. The maps hold every action of each enum: 18 user and 10 workspace. `scope` serializes as lowercase `"user"` or `"workspace"`. `version` is the crate's `CARGO_PKG_VERSION`, currently `0.1.0`.
- **Kind:** `capability`
- **Trigger:** `GET /v1/audit-actions/schema`
- **Auth:** Public, with no authentication. The route is exempt from rate limiting.
- **Effects:** None. The response is static and comes from compile-time metadata.
- **Source:** `src/handlers/mod.rs:59-62`; `src/handlers/audit_actions.rs:38-91`; `src/models/audit_actions.rs:33-80,225-342,411-419,516-579,640-648`; `src/middleware/rate_limit.rs:144-150`; `Cargo.toml:3`.
- **Tests:** `src/models/audit_actions.rs` tests `audit_action_scope_serializes_lowercase`, `all_action_strings_covers_every_variant`, `all_metadata_covers_every_action_string`. No HTTP test was found.

### AUDIT-010 Actor binding: an entry is always attributed to the caller

- **Behavior:** Every write is checked against the request's `AuthContext`. `input.user_id` must equal `ctx.user_id`, and for workspace entries `input.workspace_id` must equal `ctx.workspace_id`. A mismatch is treated as a programmer error and nothing is written. As a result, each entry's `user_id` is always the acting user. No column records a target or subject user.
- **Kind:** `guarantee`
- **Trigger:** every `log_event` / `log_event_in_tx` call (see the catalog)
- **Errors:** A user or workspace id mismatch → internal 500 (`INTERNAL_SERVER_ERROR`). A workspace entry with no workspace in the context → `VALIDATION_ERROR` (`invalid_header`) 400. A nil user or a missing workspace inside `create_in_tx` → `InvalidAuthContext` (`AUTHENTICATION_FAILED`, 401). Callers in best-effort mode swallow all of these (AUDIT-015).
- **Guarantees:** The database does not enforce the actor binding for workspace entries: the INSERT policy checks only `workspace_id`. For user entries the INSERT policy checks `user_id` against `app.current_user_id`.
- **Source:** `src/services/user_audit_log_service.rs:31-55,108,148`; `src/services/workspace_audit_log_service.rs:34-76,133,173`; `src/repositories/postgresql/user_audit_log_repository.rs:463-469`; `src/repositories/postgresql/workspace_audit_log_repository.rs:494-516`; `migrations/0004_row_level_security.sql:277-279,289-291`.
- **Tests:** `src/services/user_audit_log_service.rs::log_event_rejects_user_id_mismatch`; `src/services/workspace_audit_log_service.rs::log_event_rejects_missing_workspace_context`, `::log_event_rejects_workspace_id_mismatch`, `::log_event_rejects_user_id_mismatch`.

### AUDIT-011 Typed action vocabulary and derived resource type

- **Behavior:** Actions are two Rust enums stored as `resource.verb` strings: `UserAuditAction` with 18 variants and `WorkspaceAuditAction` with 10. A write input takes the enum of its own scope, so a workspace action cannot be written to the user log. `resource_type` is derived from the action's metadata (the full mapping is in the catalog). An optional `resource_type_override` exists on the input type, but no caller sets it. No database enum or CHECK constraint limits the stored values (`action VARCHAR(50)`, `resource_type VARCHAR(100)`).
- **Kind:** `rule`
- **Trigger:** every write
- **Source:** `src/models/audit_actions.rs:97-176,185-214,346-348,448-505,583-585`; `src/models/audit.rs:63-84,131-151`; `migrations/0002_tables.sql:380-381,404-405`.
- **Tests:** `src/models/audit_actions.rs` tests `user_audit_action_serialization`, `user_audit_action_as_str_round_trips_through_from_string`, `user_audit_action_from_string`, `user_audit_action_resource_types`, `workspace_audit_action_*`, `action_format_convention`.

### AUDIT-012 Client IP and user agent recorded on every request-driven entry

- **Behavior:** Before each write, the service reads the task-local `REQUEST_CONTEXT` and sets `metadata.request_context = {client_ip, user_agent}`. When the caller passed no metadata, a new object is created. When the caller passed metadata, the key is inserted into it, replacing any existing `request_context`. `client_ip` is taken from the first `X-Forwarded-For` entry, then `X-Real-IP`, then the socket peer, and falls back to `"unknown"`. `user_agent` is the raw `User-Agent` header, or null. Neither value is validated or capped by length. Outside a request scope the enrichment is skipped, but every current emitter runs inside a request.
- **Kind:** `guarantee`
- **Trigger:** every write
- **Effects:** Stores the IP and user agent inside the encrypted `metadata` column.
- **Guarantees:** Both headers are spoofable unless a proxy overwrites them (`src/utils/request.rs:38-40`). If caller metadata were a non-object JSON value, the request context would be dropped without notice (no caller does this today).
- **Source:** `src/services/user_audit_log_service.rs:59-85`; `src/services/workspace_audit_log_service.rs:80-109`; `src/middleware/request_context.rs:31-49,80-92`; `src/utils/request.rs:48-82,105-111`.
- **Tests:** IP extraction tests in `src/utils/request.rs` (`extract_client_ip_*`). No test checks the enrichment itself.

### AUDIT-013 Trace id stored per entry

- **Behavior:** Each entry stores the W3C trace id of the current span (32 lowercase hex characters). When no span is active, it stores the invalid-trace sentinel `00000000000000000000000000000000` and logs an error.
- **Kind:** `guarantee`
- **Trigger:** every write
- **Source:** `src/repositories/postgresql/user_audit_log_repository.rs:235,516`; `src/repositories/postgresql/workspace_audit_log_repository.rs:247,563`; `src/utils/trace.rs:49,104-126`; `migrations/0002_tables.sql:386,410`.
- **Tests:** `src/utils/trace.rs` tests (`invalid_trace_id_matches_w3c_spec`, `extract_returns_none_without_otel_context`). No audit-specific test was found.

### AUDIT-014 Transactional write mode (`log_event_in_tx`)

- **Behavior:** The entry is inserted inside the caller's open transaction. If the insert fails, the error propagates, the whole transaction rolls back, and the triggering request fails. The repository first calls `set_user_request_context` (user log) or `set_request_context` (workspace log) on that transaction. This happens even inside `with_public_context` flows, where no RLS context was set before. Callers that wrap the transaction in `retry_database_operation` re-run the audit insert on each attempt, so exactly one entry commits.
- **Kind:** `guarantee`
- **Trigger:** The actions marked "tx" in the catalog: `auth.register`, `auth.password_reset`, `auth.password_changed`, `auth.email_changed`, `auth.identity_unlinked`, `workspace.create`, `workspace.transfer` (both scopes), and `invitation.accept`.
- **Effects:** One row is inserted, atomically with the business change.
- **Errors:** Whatever the audit failure produced, mapped back through `ApiError::into_repository_error`.
- **Guarantees:** The entry exists if and only if the change committed. A user-log `create_in_tx` resets `app.current_workspace_id` to `''` for the rest of the caller's transaction (see Observations).
- **Source:** `src/services/user_audit_log_service.rs:141-167`; `src/services/workspace_audit_log_service.rs:166-191`; `src/repositories/postgresql/user_audit_log_repository.rs:455-562`; `src/repositories/postgresql/workspace_audit_log_repository.rs:486-617`; `src/repositories/postgresql/rls_context.rs:49-91`; `src/repositories/postgresql/transaction.rs:168-207`.
- **Tests:** `tests/public_workspace_member_repository_live.rs::accept_rolls_back_membership_and_invitation_when_in_tx_audit_fails` (simulates the audit failure with an injected `Err`). Row-count checks in `tests/auth_session_fanout_live.rs::reset_password_fans_out_session_revoked_and_audits`, `tests/user_sse_fanout_live.rs::change_password_publishes_session_revoked_to_other_devices` / `::confirm_email_change_publishes_identity_email_changed`, `tests/identity_unlink_fanout_live.rs` (both unlink tests), `tests/invitation_accept_fanout_live.rs::accept_invitation_fans_out_member_joined_and_refresh_and_audits`, `tests/workspace_fanout_live.rs::create_workspace_fans_out_connection_refresh_and_audits_workspace_create` / `::transfer_workspace_fans_out_transferred_and_audits_both_logs`, `tests/oauth_login_http.rs` (`auth.register`).

### AUDIT-015 Best-effort write mode (`log_event`)

- **Behavior:** The entry is written in its own short transaction (`with_user_context` or `with_workspace_context`), after the business change has committed. On failure the caller logs `warn` with `error_type = "audit_log_failed"` (identity flows use `"audit_log_emission_failed"`) and `action = "failing_open"`, and the request still succeeds. The change may therefore commit with no audit entry.
- **Kind:** `guarantee`
- **Trigger:** The actions marked "post" in the catalog. That covers most actions, including `auth.login`, `session.revoke`, `user.delete`, every `member.*`, and `invitation.create/update/cancel/resend`.
- **Effects:** One row is inserted, or none on failure, in which case a warning log is written.
- **Guarantees:** None about the entry's existence. The ordering relative to the SSE fan-out is caller-defined: in practice the audit write comes before the SSE publish.
- **Source:** `src/services/user_audit_log_service.rs:102-127`; `src/services/workspace_audit_log_service.rs:127-151`; `src/repositories/postgresql/user_audit_log_repository.rs:188-309`; `src/repositories/postgresql/workspace_audit_log_repository.rs:200-333`; the call sites are listed in the catalog.
- **Tests:** `*_succeeds_when_audit_log(ging)_fails` tests in `src/services/{session_service,identity_service,public_auth_service,user_invitation_service,user_service,workspace_member_service,workspace_service}.rs` (for example `session_service.rs::revoke_session_succeeds_when_audit_log_fails`, `user_service.rs::delete_user_succeeds_when_audit_logging_fails`).

### AUDIT-016 Server-generated id and timestamp, INSERT without RETURNING

- **Behavior:** The repository generates `id` (`Uuid::new_v4`) and `created_at` (`Utc::now()`, from the app server's clock) and issues a plain INSERT with no RETURNING clause. It returns the entry rebuilt from its own input. The database defaults (`uuid_generate_v4()`, `NOW()`) are never used. This matters after a cascade delete: once the actor's memberships are gone, a RETURNING projection would be checked against `workspace_audit_logs_select`, fail with 42501, and make a successful insert look like a failure. The user log follows the same pattern for consistency, even though its SELECT and INSERT policies match.
- **Kind:** `hardening`
- **Trigger:** every write
- **Source:** `src/repositories/postgresql/user_audit_log_repository.rs:237-308,518-561`; `src/repositories/postgresql/workspace_audit_log_repository.rs:249-332,565-616`; `migrations/0002_tables.sql:377,387,402,411`.
- **Tests:** `tests/audit_insert_no_returning_live.rs::workspace_audit_no_returning_survives_post_cascade_rls`, `::user_audit_returning_is_safe_because_select_matches_insert`.

### AUDIT-017 Field-level encryption of old_value, new_value and metadata

- **Behavior:** `old_value`, `new_value` and `metadata` are serialized to a JSON string and encrypted with AES-256-GCM under the active key version. They are stored as `TEXT` and decrypted on read. `action`, `resource_type`, `resource_id`, `user_id`, `workspace_id`, `trace_id` and `created_at` are stored in plaintext, so filters work only on plaintext columns. On read, if any field of any row in the page fails to decrypt or to parse, the whole request fails.
- **Kind:** `guarantee`
- **Trigger:** every write and every list read
- **Errors:** A decrypt failure → `DATA_INTEGRITY_ERROR` → 500. Invalid JSON after decryption → `RepositoryError::ValidationError` → `VALIDATION_ERROR` → 400.
- **Effects:** Key-rotation re-encryption (DATA) rewrites these three columns through `sp_system_re_encrypt_*_audit_log(s)`.
- **Source:** `src/repositories/postgresql/user_audit_log_repository.rs:99-172,193-231,474-512`; `src/repositories/postgresql/workspace_audit_log_repository.rs:108-183,205-243,521-559`; `src/errors/api_error.rs:718-739`; `migrations/0002_tables.sql:383-385,407-409`; `migrations/0005_stored_procedures.sql:2004-2057`; `src/encryption/re_encryption.rs:196-202,603-745`.
- **Tests:** `tests/re_encryption_all_tables.rs::re_encryption_cycle_rotates_every_encrypted_table` (rotation of audit rows). No test was found for decrypt failure on read.

### AUDIT-018 Immutability

- **Behavior:** Both tables have only INSERT and SELECT policies. With RLS enabled and the application connected as `app_user`, UPDATE and DELETE match zero rows, even for the caller's own rows. The only paths that change existing rows are SECURITY DEFINER procedures: the retention DELETE (AUDIT-021) and the key-rotation UPDATE of the three ciphertext columns.
- **Kind:** `guarantee`
- **Trigger:** any UPDATE or DELETE attempted by `app_user`
- **Guarantees:** Immutability rests entirely on the missing policies. `app_user` holds table-level UPDATE and DELETE grants, and no trigger blocks changes. The table owner and superusers bypass RLS, because FORCE RLS is not used.
- **Source:** `migrations/0004_row_level_security.sql:21-24,43-44,275-295`; `migrations/0008_grants.sql:37,124-127,145-146`; `migrations/0002_tables.sql:414`.
- **Tests:** `tests/rls_isolation.rs::audit_logs_are_immutable_to_app_user`.

### AUDIT-019 Row-level security on the audit tables

- **Behavior:** `user_audit_logs`: both INSERT and SELECT require `user_id = app.current_user_id`. `workspace_audit_logs`: INSERT requires `workspace_id = app.current_workspace_id`, and SELECT requires `workspace_id` to be in `get_current_user_workspace_ids()` (workspaces the user is a member or owner of). Under RLS, a user can therefore read the workspace logs of every workspace they belong to, whatever their permissions, and can insert a row naming any `user_id` into the current workspace.
- **Kind:** `guarantee`
- **Trigger:** every audit read and write
- **Source:** `migrations/0004_row_level_security.sql:43-44,104-117,144-160,275-295`; `src/repositories/postgresql/rls_context.rs:49-91`.
- **Tests:** `tests/rls_isolation.rs::app_user_cannot_read_other_tenants_data`, `::app_user_cannot_write_other_tenants_data`.

### AUDIT-020 Entries survive deletion of their user or workspace

- **Behavior:** `user_id` and `workspace_id` are not foreign keys. Rows outlive account deletion and workspace deletion, and the post-cascade audit writes for `user.delete` and `workspace.delete` succeed even though their subjects no longer exist. Afterwards no API caller can read those rows. A deleted user cannot authenticate, and no member remains to pass the workspace SELECT policy. The rows stay until retention removes them.
- **Kind:** `guarantee`
- **Trigger:** account deletion (USER) and workspace deletion (WS)
- **Source:** `migrations/0002_tables.sql:414-416,641-643`; `src/services/user_service.rs:769-918`; `src/services/workspace_service.rs:778-806`.
- **Tests:** `tests/audit_insert_no_returning_live.rs::workspace_audit_no_returning_survives_post_cascade_rls`; `src/services/user_service.rs::delete_user_success_cascade_emits_audit_and_sse_fanout`.

### AUDIT-021 Retention cleanup

- **Behavior:** A background task named `audit_log_cleanup` runs once at startup (the first `tokio::time::interval` tick fires immediately) and then every `AUDIT_LOG_CLEANUP_INTERVAL_HOURS`. Each run deletes every row in both tables with `created_at < NOW() - AUDIT_LOG_RETENTION_DAYS days`, across all tenants, in one unbatched DELETE per table. The workspace table goes first, then the user table. Both logs use the same retention setting.
- **Kind:** `guarantee`
- **Trigger:** background job `audit_log_cleanup`, spawned only when `ENABLE_BACKGROUND_CLEANUP=true`
- **Effects:** Calls `sp_cleanup_expired_workspace_audit_logs(INTEGER)` and `sp_cleanup_expired_user_audit_logs(INTEGER)`, both SECURITY DEFINER with a pinned `search_path`, each returning `deleted_count`. On success it logs `info` with `removed_count` when rows were removed, or `debug` when none were. On failure it logs `error` ("Cleanup failed after all retries"). Cleanup writes no audit entry of its own.
- **Guarantees:** Only one instance runs each cycle, through the `background_task_locks` distributed lock (lock TTL `BACKGROUND_TASK_LOCK_DURATION_SECS`). Each table gets up to 3 attempts with backoff, and permanent errors stop retries early. The retention value is passed as `u64 as i32` (see Observations).
- **Source:** `src/server.rs:752-797,1707-1770`; `src/background.rs:55-127,201-230`; `src/services/user_audit_log_service.rs:207-219`; `src/services/workspace_audit_log_service.rs:231-243`; `src/repositories/postgresql/user_audit_log_repository.rs:413-438`; `src/repositories/postgresql/workspace_audit_log_repository.rs:443-468`; `migrations/0006_background_tasks.sql:191-245`; `migrations/0003_indexes.sql:93,101`.
- **Tests:** `tests/repository_smoke.rs::user_audit_log_cleanup_expired_executes_against_schema`, `::workspace_audit_log_cleanup_expired_executes_against_schema` (empty DB only); `src/server.rs` tests `all_tasks_spawn_when_enabled_and_all_backings_present`, `nothing_spawns_when_cleanup_disabled`. No test checks that rows older or newer than the cutoff are handled correctly.

## Data owned

- **`user_audit_logs`** (`migrations/0002_tables.sql:401-412`): `id UUID PK`, `user_id UUID NOT NULL` (no FK), `action VARCHAR(50)`, `resource_type VARCHAR(100)`, `resource_id UUID NULL`, `old_value TEXT` / `new_value TEXT` / `metadata TEXT` (AES-256-GCM ciphertext of JSON, nullable), `trace_id VARCHAR(32) NOT NULL`, `created_at TIMESTAMPTZ NOT NULL`.
- **`workspace_audit_logs`** (`migrations/0002_tables.sql:376-388`): the same columns, plus `workspace_id UUID NOT NULL` (no FK).
- **Indexes** (`migrations/0003_indexes.sql:86-101`): workspace table `(workspace_id, created_at DESC)`, `(user_id)`, `(resource_type, resource_id)`, `(action)`, `(created_at)`; user table `(user_id, created_at DESC)`, `(resource_type, resource_id)`, `(action)`, `(created_at)`.
- **RLS** (`migrations/0004_row_level_security.sql:43-44,275-295`): `workspace_audit_logs_insert`, `workspace_audit_logs_select`, `user_audit_logs_insert`, `user_audit_logs_select`. There are no UPDATE or DELETE policies.
- **Stored procedures:** `sp_cleanup_expired_workspace_audit_logs(INTEGER)`, `sp_cleanup_expired_user_audit_logs(INTEGER)` (`migrations/0006_background_tasks.sql:198-245`); `sp_system_re_encrypt_fetch_workspace_audit_logs(UUID, INT)`, `sp_system_re_encrypt_update_workspace_audit_log(UUID, TEXT, TEXT, TEXT)`, `sp_system_re_encrypt_fetch_user_audit_logs(UUID, INT)`, `sp_system_re_encrypt_update_user_audit_log(UUID, TEXT, TEXT, TEXT)` (`migrations/0005_stored_procedures.sql:2004-2057`; DATA drives them). All are granted to `app_user` (`migrations/0008_grants.sql:124-127,145-146`).
- **Triggers:** none on the audit tables.
- **Background task:** `audit_log_cleanup` (AUDIT-021).
- **Code-level vocabulary:** `UserAuditAction` and `WorkspaceAuditAction`, together with `AuditActionMetadata` (`src/models/audit_actions.rs`).

## Configuration

| Variable | Default | Validation | Read at |
|---|---|---|---|
| `AUDIT_LOG_RETENTION_DAYS` | `365` (u64) | Must be > 0, otherwise startup fails. A warning is logged when < 30 or > 3650. No upper bound. Passed to SQL as `i32` | `src/config/environment.rs:424,707-711,1471-1482`; `.env.example:574` |
| `AUDIT_LOG_CLEANUP_INTERVAL_HOURS` | `24` (u64) | Must be > 0, otherwise startup fails. A warning is logged when > 168 | `src/config/environment.rs:425-428,714-718,1460-1468`; `.env.example:580` |
| `ENABLE_BACKGROUND_CLEANUP` | `true` | Master switch. When false, no cleanup runs | `src/config/environment.rs:407-410`; `src/server.rs:786-797` |
| `BACKGROUND_TASK_LOCK_DURATION_SECS` | `300` (i32) | Must be > 0. Shared with all background tasks | `src/config/environment.rs:411-414,725-729` |

Also used, but owned by other domains: `REQUIRE_EMAIL_VERIFICATION` (gates the user audit endpoint), and the encryption key ring (decides whether re-encryption runs).

## Cross-domain dependencies

- **Audit calls out to:**
  - DATA: `DatabaseEncryption::encrypt_data` and `decrypt_data`; `transaction::with_user_context` and `with_workspace_context`; `rls_context::activate_*`; the SECURITY DEFINER cleanup procedures; the background-task runner and distributed lock (`src/background.rs`, `utils::run_with_distributed_lock`); `utils::retry::retry_with_backoff`.
  - HTTP: `REQUEST_CONTEXT` together with `extract_client_ip` and `extract_user_agent`; the `Query` and `ValidatedQuery` extractors; `PaginationQuery` and `keyset`; `PaginatedApiResponse` and `ApiResponse`; the error mapping.
  - OBS: `extract_or_invalid_in`, for the trace id.
  - AUTH and WS: `AuthContext`, `require_permission`, `Permission::ReadWorkspaceAuditLog`, and the user-scoped and workspace-scoped auth middleware.
- **Audit is called by:**
  - AUTH: `PublicAuthServiceImpl`, `SessionServiceImpl` and `IdentityServiceImpl`.
  - USER: `UserServiceImpl`.
  - WS: `WorkspaceServiceImpl`, `WorkspaceMemberServiceImpl` and `UserInvitationServiceImpl`.
  - Wiring: the services are built in `src/server.rs:169-196` and injected into each owning service (`src/server.rs:255-377`).
- **DATA re-encryption** reads and rewrites the audit ciphertext through the four `sp_system_re_encrypt_*` procedures (`src/encryption/re_encryption.rs:603-745`).
- **HTTP idempotency:** a replay with the same `Idempotency-Key` returns the cached response without running the handler again, so it writes no second audit entry (`src/middleware/idempotency.rs:138-179`).
- **Desktop client:** consumes `/audit-actions/schema`, keyed on `scope` (per the comment at `src/models/audit_actions.rs:674-676`).

## Audit action catalog

Mode: **tx** means the entry is written in the business transaction (AUDIT-014). **post** means it is written best-effort after commit (AUDIT-015). Every entry also gets `metadata.request_context = {client_ip, user_agent}` (AUDIT-012). The "metadata" column below lists only what the caller supplies. All paths are under `src/services/`. Every defined action has at least one emitter. No action is defined but never emitted.

### User scope (`user_audit_logs`)

| Action | resource_type | Emitter (`file:line`) | Trigger | Mode | resource_id | old_value | new_value | metadata |
|---|---|---|---|---|---|---|---|---|
| `auth.login` | user | `public_auth_service.rs:438` (call 432-456, in `handle_existing_user_login`, reached from :1185 and :1007) | `POST /auth/login`; `POST /auth/oauth` for an existing identity | post | user id | — | `{auth_method}` | — |
| `auth.register` | user | `public_auth_service.rs:559` (OAuth), `:666` (credentials) | `POST /auth/oauth` for a new user; `POST /auth/register` | tx | user id | — | `{auth_method}` | — |
| `auth.password_reset` | user | `public_auth_service.rs:2164` | `POST /auth/reset-password` | tx | user id | — | `{auth_method:"credentials"}` | — |
| `auth.password_changed` | user | `user_service.rs:1186` | `PUT /users/me/password` | tx | user id | — | `{auth_method:"credentials", revoked_other_session_count}` | — |
| `auth.email_verified` | user | `public_auth_service.rs:1765` | `POST /auth/verify-email` | post | user id | — | `{source:"verification_code", auth_method:"credentials"}` | — |
| `auth.email_verified` | user | `identity_service.rs:682` | `POST /auth/identities/oauth/complete`, only when the link flips `email_verified` from false to true (same normalized email) | post | user id | — | `{source:"oauth_link", auth_method:<provider>}` | — |
| `auth.email_changed` | user | `user_service.rs:1579` | `POST /users/me/email/confirm` | tx | user id | `{email_hash}` (HMAC of old) | `{email_hash, auth_method}` | — |
| `auth.identity_linked` | user_identity | `identity_service.rs:150-157` (via `emit_identity_change`, called from :373 and :545) | `POST /auth/identities/credentials`; `POST /auth/identities/oauth/complete` | post | user id, not the identity id | — | `{method}` | — |
| `auth.identity_unlinked` | user_identity | `identity_service.rs:920` (credentials), `:1162` (OAuth); only on a `Deleted` outcome | `POST /auth/identities/credentials/unlink`; `DELETE /auth/identities/{method}` | tx | user id | — | `{method, revoked_other_session_count}` | — |
| `session.revoke` | session | `session_service.rs:126` | `DELETE /users/me/sessions/{session_id}`; `POST /auth/logout` (`handlers/auth.rs:508-511`) | post | revoked session id | — | — | — |
| `session.revoke` | session | `session_service.rs:269` (one entry per revoked session, written concurrently with `join_all`) | `DELETE /users/me/sessions/others` | post | revoked session id | — | — | `{via:"revoke_others", current_session_id}` |
| `session.reuse_detected` | session | `public_auth_service.rs:932` | `POST /auth/refresh` with a rotated-out refresh token | post | reused session id | — | `{auth_method}` | — |
| `user.update` | user | `user_service.rs:442` | `PUT /users/me`, only when the name or avatar changed | post | user id | `{name, avatar_file_id}` | `{name, avatar_file_id}` | — |
| `user.delete` | user | `user_service.rs:778` | `POST /users/me/delete`, written after the cascade | post | user id | `{email, name, avatar_file_id}` | — | `{cascade_owned_workspace_count, cascade_owned_workspace_ids, cascade_member_only_workspace_count, cascade_member_only_workspace_ids, cascade_authored_invitation_count, cascade_authored_invitation_ids, revoked_session_count}` |
| `user.telemetry_consent_changed` | user | `user_service.rs:1718` | `PUT /users/me/telemetry-consent`, only when the value changes | post | user id | `{telemetry_consent}` | `{telemetry_consent}` | — |
| `workspace.create` | workspace | `workspace_service.rs:298` | `POST /users/me/workspaces` | tx (inside retry) | workspace id | — | `{name}` | — |
| `workspace.switch` | workspace | `workspace_service.rs:939` | `POST /users/me/workspaces/{id}/switch` | post | target workspace id | `{from_workspace_id}` | `{to_workspace_id}` | — |
| `workspace.transfer` | workspace | `workspace_service.rs:1167` | `POST /workspace/transfer`. Written to the previous owner's log only | tx (same transaction as the workspace entry) | workspace id | — | `{workspace_id, new_owner_id, new_owner_email}` | — |
| `invitation.accept` | workspace_invitation | `user_invitation_service.rs:213` | `POST /users/me/invitations/{id}/accept` | tx (inside retry) | invitation id | — | `{workspace_id, permissions}` | — |
| `invitation.decline` | workspace_invitation | `user_invitation_service.rs:380` | `POST /users/me/invitations/{id}/decline` | post | invitation id | `{workspace_id, permissions}` | — | — |

### Workspace scope (`workspace_audit_logs`)

| Action | resource_type | Emitter (`file:line`) | Trigger | Mode | resource_id | old_value | new_value | metadata |
|---|---|---|---|---|---|---|---|---|
| `workspace.update` | workspace | `workspace_service.rs:629` | `PUT /workspace`, only when the name or picture changed | post | workspace id | `{name, picture_file_id}` | `{name, picture_file_id}` | — |
| `workspace.delete` | workspace | `workspace_service.rs:787` | `DELETE /workspace` | post | workspace id | `{name, owner_id}` | — | — |
| `workspace.delete` | workspace | `user_service.rs:819` (one per owned workspace, under a per-workspace context) | `POST /users/me/delete` | post | workspace id | `{name, owner_id}` | — | `{cascade_source:"user.delete"}` |
| `workspace.transfer` | workspace | `workspace_service.rs:1144` | `POST /workspace/transfer` | tx | workspace id | `{owner_id}` | `{owner_id, new_owner_email}` | — |
| `member.update_permissions` | workspace_member | `workspace_member_service.rs:400` | `PUT /workspace/members/{member_id}/permissions` | post | member id | `{permissions, user_id}` | `{permissions, user_id}` | — |
| `member.remove` | workspace_member | `workspace_member_service.rs:571` | `DELETE /workspace/members/{member_id}` | post | member id | `{user_id, permissions}` | — | — |
| `member.leave` | workspace_member | `workspace_member_service.rs:737` | `POST /workspace/leave` | post | member id | — | — | — |
| `member.leave` | workspace_member | `user_service.rs:857` (one per member-only workspace) | `POST /users/me/delete` | post | member id (row already deleted) | — | — | `{cascade_source:"user.delete"}` |
| `invitation.create` | workspace_invitation | `workspace_member_service.rs:985` | `POST /workspace/invitations` (also on a re-invite that upserts an existing pending invitation) | post | invitation id | — | `{email, permissions, expires_at}` | — |
| `invitation.update` | workspace_invitation | `workspace_member_service.rs:1207` | `PUT /workspace/invitations/{invitation_id}` | post | invitation id | `{permissions, expires_at}` | `{permissions, expires_at}` | — |
| `invitation.cancel` | workspace_invitation | `workspace_member_service.rs:1339` | `DELETE /workspace/invitations/{invitation_id}` | post | invitation id | `{email, permissions}` | — | — |
| `invitation.cancel` | workspace_invitation | `user_service.rs:894` (one per pending invitation the user authored in a workspace they do not own) | `POST /users/me/delete` | post | invitation id | `{email, permissions}` | — | `{cascade_source:"user.delete"}` |
| `invitation.resend` | workspace_invitation | `workspace_member_service.rs:1465` | `POST /workspace/invitations/{invitation_id}/resend` | post | invitation id | `{expires_at}` | `{expires_at}` | — |

### Mutations that write no audit entry

Compared against every mutating route in `src/handlers/mod.rs` and against background jobs:

| Route or job | What changes, unaudited |
|---|---|
| `POST /auth/refresh` (`mod.rs:68`), normal rotation | Session tokens are rotated. Only reuse is audited. |
| `POST /auth/forgot-password` (`mod.rs:70`) | A password-reset code is issued and emailed. |
| `POST /auth/resend-verification` (`mod.rs:113`) | A verification code is regenerated and emailed. |
| `POST /users/me/email` (`mod.rs:123`) | A pending email change and its code are staged, and emails go to the new and old addresses. |
| `POST /users/me/files`, `DELETE /users/me/files/{file_id}` (`mod.rs:242-247`) | User file rows and stored objects are created or deleted. |
| `POST /workspace/files`, `DELETE /workspace/files/{file_id}` (`mod.rs:399-407`) | Workspace file rows and stored objects are created or deleted by any member. |
| `POST /telemetry/{traces,metrics,logs}` (`mod.rs:323-325`) | Proxy only; no domain data changes. |
| Session creation in `POST /auth/login`, `/auth/oauth` and `/auth/register` (`handlers/auth.rs:256,348,455`), plus provider-token storage (`:278`) | A session row and an `oauth_tokens` row are written. There is no session-level action, and `/auth/register` emits `auth.register` but no `auth.login`. |
| Sessions reaped inside audited flows (reset, change, and unlink password; account deletion) | Only a count is recorded (`revoked_other_session_count`, `revoked_session_count`). No per-session `session.revoke` entries are written, unlike `DELETE /users/me/sessions/others`. |
| Cascades of `DELETE /workspace` | Other members' memberships, the workspace's invitations and files are removed. Only `workspace.delete` is written. |
| Default-workspace preference cleared on `DELETE /workspace` and `POST /workspace/leave` | The preference row is updated. |
| Failed logins, failed password checks, permission denials and rate-limit rejections | No failure or denial action exists. |
| Background `auth_cleanup` (expired sessions, invitations, verification codes and pending email changes), `audit_log_cleanup`, re-encryption, and the object-deletion reaper (`src/server.rs:752-797`) | Rows are deleted or rewritten with no audit entry. An expired invitation disappears with no `invitation.*` record. |

`POST /auth/validate-session` (`mod.rs:107`) uses POST but changes nothing, so it is not a gap.

## Observations

1. **A login is recorded before its session exists.** `auth.login` is written inside `authenticate_*` (`src/services/public_auth_service.rs:432-456`). The handler creates the session only afterwards (`src/handlers/auth.rs:244-257,449-456`), so if session creation fails, the trail still shows a login that produced no session. Registration creates a session too (`handlers/auth.rs:342-349`) but emits only `auth.register`.
2. **Only successes are audited.** No action covers failed authentication, refused authorization or a rejected request (`src/models/audit_actions.rs:97-176,448-475`). The SOC 2 comments on the transactional paths (for example `src/services/user_invitation_service.rs:200-206`) cover successful changes only.
3. **The transactional/best-effort split follows no stated rule.** Ownership transfer, invitation accept and workspace create are transactional. Member removal, permission changes, invitation create/cancel and workspace deletion are best-effort and can commit without an entry. The comments justify both choices with the same SOC 2 language: `workspace_member_service.rs:386-392` and `workspace_service.rs:1128-1134`.
4. **The workspace log misses lifecycle events.** `workspace.create`, `invitation.accept` and `invitation.decline` are written only to the actor's user log (`workspace_service.rs:292-309`, `user_invitation_service.rs:207-225,374-399`), and no `member.join` action exists. A workspace's own log therefore does not record its creation, members joining, or declined invitations.
5. **A user's log shows only what the user did.** The actor binding (AUDIT-010) means nothing done to a user (removed, permissions changed, ownership received) ever appears in that user's log. The target id exists only inside the encrypted `old_value` and `new_value`, and cannot be filtered. The user-scope `workspace.transfer` goes only to the previous owner (`workspace_service.rs:1161-1180`).
6. **PII handling is inconsistent.** `auth.email_changed` stores HMAC blind indexes instead of plaintext addresses (`user_service.rs:1539-1541,1571-1588`). Other actions store plaintext addresses inside the ciphertext: `invitation.create` and `invitation.cancel` hold the invitee email (`workspace_member_service.rs:989-993,1342-1345`, `user_service.rs:897-900`), `workspace.transfer` holds `new_owner_email` (`workspace_service.rs:1150-1153,1171-1175`), and `user.delete` holds email and name (`user_service.rs:781-785`). Every entry also carries IP and user agent (AUDIT-012). The table comment mentions only IP and user agent as PII (`migrations/0002_tables.sql:385,409`).
7. **Read-only members can see other members' IPs and invitee emails.** The default Member preset includes `read:workspace_audit_log` (`src/models/permissions.rs:207-209,385-390`). Holders can read every member's IP and user agent and every invitee's email.
8. **Audit email hashes go stale after an HMAC key rotation.** `email_hash` is computed with the active key at write time (`src/encryption/database_encryption.rs:268-277`). Re-encryption rewrites the ciphertext but passes the JSON through unchanged (`migrations/0005_stored_procedures.sql:2049-2057`), so a stored hash is never recomputed. After a rotation, old audit hashes stop matching the rotated `users.email_hash`.
9. **The retention value can wrap when cast to i32.** The u64 `AUDIT_LOG_RETENTION_DAYS` has no upper bound (`src/config/environment.rs:424,707`) and is passed as `retention_days as i32` (`user_audit_log_repository.rs:422`, `workspace_audit_log_repository.rs:452`). A value of 2^31 or more wraps. For example, 4294967295 becomes -1, which puts the cutoff at `NOW() + 1 day` and deletes every row. Values above roughly 2.4 million days push the cutoff before 4713 BC, which PostgreSQL rejects as out of range.
10. **Retention comments disagree with the code.** The SQL comment says the range is "90 days - 10 years" (`migrations/0006_background_tasks.sql:194`). The code accepts anything from 1 day up and only warns below 30 or above 3650 (`environment.rs:1471-1482`).
11. **Cleanup is unbatched.** Each table is cleared by a single cross-tenant DELETE with a CTE count (`migrations/0006_background_tasks.sql:207-214,234-241`). The first run happens at startup.
12. **Pagination can skip or repeat rows near a page boundary.** Cursors encode `created_at` in milliseconds (`src/utils/pagination.rs:492-495`, `user_audit_log_repository.rs:395`), while `created_at` is stored with microsecond precision from `Utc::now()` (`user_audit_log_repository.rs:248,523`). The comparison `(created_at, id) <op> (cursor_ms, id)` can therefore skip rows in descending order, or repeat the boundary row in ascending order, when rows share the boundary row's millisecond. The bulk revocation at `session_service.rs:264-289` writes many rows concurrently, which makes this likely. The shared keyset code (HTTP) has the same issue.
13. **Timestamps come from the app server clock.** Using `Utc::now()` instead of the database `NOW()` (`user_audit_log_repository.rs:248`, `workspace_audit_log_repository.rs:266`) means clock skew between instances can reorder entries relative to commit order.
14. **One bad row fails the whole page, sometimes with a 400.** Invalid JSON after decryption is mapped to `RepositoryError::ValidationError`, which becomes 400 `VALIDATION_ERROR` (`user_audit_log_repository.rs:123-129`, `src/errors/api_error.rs:718-723`). That reports a server-side data fault as a client error. A decrypt failure gives 500.
15. **RLS does not bind the actor on the workspace log.** The INSERT policy checks only `workspace_id` (`migrations/0004_row_level_security.sql:277-279`), so only `validate_input` stops a row from naming another user. The SELECT policy ignores `read:workspace_audit_log`, so the service check (`workspace_audit_log_service.rs:212`) is the only gate on reads, as the inline test comment at `:325-327` also says.
16. **A user-log write changes the caller's RLS context.** `create_in_tx` calls `set_user_request_context`, which clears `app.current_workspace_id` for the rest of the caller's transaction (`user_audit_log_repository.rs:472`; `migrations/0004_row_level_security.sql:113-115`). In `transfer_workspace` the user entry is the last statement (`workspace_service.rs:1161`), so nothing breaks today, but the correctness depends on statement order.
17. **Immutability relies only on missing policies.** `app_user` holds UPDATE and DELETE on every table (`migrations/0008_grants.sql:37`). No trigger guards the audit tables. The audit re-encrypt UPDATEs have no compare-and-swap guard (`migrations/0005_stored_procedures.sql:2022-2030,2049-2057`), unlike the other re-encrypt procedures (for example `:1992-2001`).
18. **Trails of deleted accounts and workspaces are retained but unreadable.** After deletion, nobody can reach them through the API (AUDIT-020), yet they keep PII until retention expires.
19. **Logout cannot be told apart from revoking a session in the list.** `POST /auth/logout` and `DELETE /users/me/sessions/{id}` both reach `revoke_session` and write `session.revoke` with no metadata (`session_service.rs:120-133`, `handlers/auth.rs:508-511`). The actor's session id is not recorded for single revokes, so a logout looks the same as revoking a device from the list.
20. **A re-invite is logged as a fresh `invitation.create`.** `create_invitation` upserts on `(workspace_id, email_hash)` and overwrites permissions, expiry and `invited_by` on the existing row (`src/repositories/postgresql/workspace_member_repository.rs:1425-1434`). The re-invite is logged with `old_value` null, so the previous permissions and inviter are lost from the trail.
21. **An OAuth link writes its two entries out of order.** `link_oauth_complete` writes `auth.email_verified` (`identity_service.rs:542`) before `auth.identity_linked` (`:545`), so the effect is recorded before its cause.
22. **The same action string exists in both scopes.** `workspace.transfer` is defined in both enums with different descriptions (`src/models/audit_actions.rs:168-169,323-328,455-456,530-535`) and appears in both maps of `/audit-actions/schema`.
23. **`resource_type_override` is never used.** It exists on both inputs (`src/models/audit.rs:68-70,135-137`), but every call site passes `None`.
24. **Unknown query keys are silently ignored.** The filter structs and `PaginationParams` lack `deny_unknown_fields` (`src/models/audit.rs:210,313`; `src/utils/pagination.rs:196`), so a misspelled filter, or `user_id` on the user endpoint, is ignored and the response is unfiltered.
25. **Schema response ordering and versioning.** Both maps are `HashMap`s, so the key order is unstable (`src/handlers/audit_actions.rs:43,48`). `version` is the crate version (`0.1.0`), not a version of the action set.
26. **Comments disagree with the code.**
    - `src/models/audit_actions.rs:128-131` says the identity `method` goes into "metadata"; the code puts it in `new_value` (`identity_service.rs:145-157`).
    - `identity_service.rs:128-135,141-143` says `emit_identity_change` and `emit_identity_audit` serve OAuth unlink; OAuth unlink audits in-transaction (`:1156-1174`), and the helper is called only from link paths (`:373,545`).
    - `audit_actions.rs:115-118` says OAuth users never emit `auth.email_verified`; the OAuth-link path emits it with the provider as `auth_method` (`identity_service.rs:682-689`).
    - The JSON example at `src/handlers/audit_actions.rs:16-37` omits `label`.
    - The example `?resource_type=workspace&resource_type=member` at `src/models/audit.rs:310` uses `member`, which is not a valid value (`workspace_member` is).
    - `src/services/traits.rs:685,737` claims `log_event` validates the action format; no runtime check exists.
    - The scope lists in `src/models/audit.rs:22-25,89-94`, `audit_actions.rs:88-94,441-444` and `migrations/0002_tables.sql:370-374,393-398` are incomplete: they omit transfer, identity, consent, email, password and reuse actions.
27. **Documentation is thin.** `documentation/DATABASE.md` and `README.md` do not mention the audit tables. `documentation/TELEMETRY.md:116` describes `user.telemetry_consent_changed` accurately.
28. **No HTTP-level test covers the three audit endpoints.** Coverage is limited to unit tests of the validators and the permission gate, plus repository and RLS tests against a live database.

## Open questions

1. Should failed or denied security events be audited: failed logins, wrong current passwords, permission denials, refresh-token reuse already handled? Today only successes are.
2. Should the unaudited mutations listed above be audited, in particular file upload and delete (user and workspace), forgot-password and email-change requests, and session creation?
3. What decides whether an action is transactional (guaranteed) or best-effort? Should member removal, permission changes, invitation changes and workspace or account deletion be guaranteed?
4. What is the PII policy for audit values? Should invitee and new-owner emails, and the deleted user's email and name, be stored as plaintext-in-ciphertext, as hashes like `auth.email_changed`, or not at all?
5. Should holders of `read:workspace_audit_log` (including the read-only Member preset) see other members' IP addresses and user agents?
6. Under account deletion (GDPR erasure), should the user's own audit history and the `user.delete` entry with email and name be kept until retention, anonymized, or purged? Is anyone meant to read the trails of deleted accounts and workspaces?
7. Should the workspace log record workspace creation, members joining and declined invitations? Should actions taken on a user also appear in that user's log, or be filterable by target?
8. Should `auth.login` be written only after the session is created, and should registration also record a login?
9. Should retention be separate per log, and should `AUDIT_LOG_RETENTION_DAYS` have an upper bound? The i32 wrap is almost certainly unintended.
10. Should a re-invite be recorded as `invitation.update` (or carry the previous values) instead of a second `invitation.create`?
11. Should `workspace.transfer` keep the same string in both scopes, and should the new owner get an entry too?
12. Should the half-open millisecond cursor issue be fixed in the shared keyset code (HTTP), or by storing `created_at` with millisecond precision?
13. Should `/audit-actions/schema` stay public and unauthenticated, and should `version` track changes to the action set rather than the crate version?
14. Should unknown query keys on the audit endpoints be rejected rather than ignored?
15. Is `resource_type_override` wanted, or is it dead API?
