# Inventory: User account and profile (`USER`)

Reference: `AntWorker-API` @ `0210895` (2026-10-20). This file records what the old API does. It does not prescribe what the new one should do.

## Scope

Everything under `/v1/users/me` that concerns the account itself:

- reading the profile (`GET /users/me`)
- updating the display name and avatar (`PUT /users/me`). Avatar storage mechanics belong to FILE; only the profile side is recorded here.
- telemetry consent (`PUT /users/me/telemetry-consent`) and the `user_preferences.telemetry_consent` column
- password change from settings (`PUT /users/me/password`)
- email change from settings, request and confirm (`POST /users/me/email`, `POST /users/me/email/confirm`). The handlers live in `src/handlers/auth.rs`; the logic is in `UserServiceImpl`.
- account deletion (`POST /users/me/delete`), including what cascades

Out of scope, owned elsewhere:

- WS: listing, creating and switching workspaces (`/users/me/workspaces*`), invitations (`/users/me/invitations*`), and `user_preferences.default_workspace_id`
- FILE: user files (`/users/me/files*`) and the object-deletion reaper
- AUDIT: user audit logs (`/users/me/audit-logs`)
- AUTH: sessions (`/users/me/sessions*`), identities (`/auth/identities*`), registration, login, verify-email, password reset
- RT: the events stream (`/users/me/events`)

Paths below drop the `/v1` prefix that `server.rs:698` adds by nesting.

Files read in full:

- `src/handlers/users.rs`
- email-change handlers and response types in `src/handlers/auth.rs` (lines 1-200, 530-590, 700-800, 1076-1178)
- `src/services/user_service.rs`, including its `#[cfg(test)]` module
- `src/models/user.rs`
- `src/repositories/postgresql/{user_repository,user_preferences_repository,public_user_repository}.rs`
- the user, preferences and verification-code parts of `src/repositories/traits.rs` and `src/services/traits.rs`
- `src/extractors/multipart.rs`
- the request models in `src/models/auth.rs:579-687`
- `src/auth/verification_code_validator.rs`
- `src/repositories/postgresql/public_verification_code_repository.rs:250-387`

Also read:

- `src/handlers/mod.rs:89-291` and `src/server.rs:577-698` (route groups and middleware order)
- `src/auth/middleware.rs:85-185`
- `src/auth/revocation.rs:200-349`
- `src/utils/{content_type,content_disposition,deserializers,database_retry}.rs` (the relevant parts)
- `src/errors/{api_error,service_error,response_envelope,auth_error}.rs` (status mapping)
- `src/config/environment.rs` (the relevant fields and validation)
- migrations `0002` (tables and FKs), `0004` (RLS), `0005` (`sp_public_change_password`, `sp_public_initiate_email_change`, `sp_public_complete_email_change`, `sp_public_find_authored_invitations_in_unowned_workspaces`), `0006` (pending-email-change cleanup) and `0007` (object-deletion trigger)
- `templates/email/{email_change_notice,email_change_confirmation,account_deletion_confirmation}.txt`
- `documentation/{OBJECT_STORAGE,TELEMETRY}.md`

Tests read:

- `tests/email_change_confirm_http.rs`
- `tests/user_preferences_repository_live.rs`
- `tests/avatar_ingest_repository_live.rs`
- `tests/public_user_repository_live.rs`
- `tests/user_sse_fanout_live.rs`

Tests whose names were scanned for coverage: `tests/auth_transaction_flows.rs`, `tests/file_swap_repository_live.rs` and `tests/audit_insert_no_returning_live.rs`.

Not opened: `.env` and `alertmanager-secrets/`. Both exist.

Wire error format, used in every Errors field: the envelope `code` is SCREAMING_SNAKE (`errors/response_envelope.rs:242-276`). The internal `ServiceErrorCode` (for example `business_rule` or `field_validation`) appears only in logs (`errors/api_error.rs:975-1038`). Where it helps, entries give it in parentheses.

## Behaviors

### USER-001 Read own profile

- **Behavior:** Returns the caller's own account as `UserResponse {id, email, name, avatar_file_id, email_verified, telemetry_consent}`. The user fields come from a fresh RLS-scoped read of `users`. `telemetry_consent` comes from the `AuthContext` that the middleware loaded from `user_preferences` on this request.
- **Kind:** `capability`
- **Trigger:** `GET /users/me`
- **Auth:** Session (user-scoped). Blocked with 403 when `REQUIRE_EMAIL_VERIFICATION=true` and the caller is an unverified credentials-session user (see USER-033).
- **Input & validation:** None. There is no user-id parameter; the subject is always `ctx.user_id`.
- **Effects:** Read only.
- **Errors:** `RESOURCE_NOT_FOUND` → 404 → the `users` row is missing ("user not found: {id}").
- **Guarantees:** Self-only. The subject is always the session's user. `avatar_file_id` is a bare file id; clients resolve it to `/files/public/user/{id}`, and the server never emits a URL (`models/user.rs:10-14`).
- **Source:** `src/handlers/users.rs:36-57`, `src/services/user_service.rs:237-260`, `src/repositories/postgresql/user_repository.rs:124-166`, `src/handlers/auth.rs:59-69`, `src/handlers/mod.rs:168`
- **Tests:** `tests/http_smoke.rs` (authenticated, invalid-token and unauthenticated `GET /users/me`), `tests/email_verification_middleware_http.rs`, `tests/oauth_login_http.rs` (probe), `tests/rate_limit_middleware_http.rs` (probe)

### USER-002 Self-access guard in `get_user_by_id`

- **Behavior:** The service method rejects a nil user id with 400 and any id other than `ctx.user_id` with 403, before touching the repository. The only production caller passes `ctx.user_id`, so neither branch can be reached over HTTP.
- **Kind:** `hardening`
- **Trigger:** Internal call `AuthenticatedUserService::get_user_by_id`
- **Auth:** Session
- **Errors:**
  - `VALIDATION_ERROR` (`invalid_path_parameter`) → 400 → nil id
  - `PERMISSION_DENIED` → 403 → id differs from the caller ("Can only access own user profile")
- **Source:** `src/services/user_service.rs:242-254`
- **Tests:** `user_service.rs` tests `get_user_by_id_rejects_mismatched_user_id`, `get_user_by_id_rejects_nil_user_id`

### USER-003 Update display name

- **Behavior:** A `name` part in the multipart profile update replaces `users.name` (encrypted at rest) and bumps `updated_at`. The response is the updated `UserResponse`; its `telemetry_consent` comes from the request's context.
- **Kind:** `capability`
- **Trigger:** `PUT /users/me` (`multipart/form-data`)
- **Auth:** Session (user-scoped), behind the verification gate (USER-033)
- **Input & validation:**
  - The `name` text part is normalized: trimmed, any run of Unicode whitespace collapsed to one space, then NFC.
  - It is then validated as 1-100 characters and not whitespace-only. A whitespace-only value normalizes to "" and fails the minimum length.
  - An omitted `name` part means "leave unchanged".
  - Form rules are in USER-004.
- **Effects:**
  - Name-only: `UPDATE users SET name, updated_at` (`email` and `avatar_file_id` are not written).
  - Then, all after the write: a best-effort user audit `user.update` with `old_value {name, avatar_file_id}` and `new_value {name, avatar_file_id}`.
  - Then a best-effort SSE `profile.updated` (user-scoped) with payload `{updated_by_session_id}`.
  - Then one SSE `member.profile_updated` per workspace the user owns or belongs to, with payload `{workspace_id, user_id, updated_by_session_id}`. This workspace enumeration runs only when SSE is enabled.
  - Logs `"User updated successfully"`.
- **Errors:**
  - `VALIDATION_ERROR` → 400 → name invalid
  - `RESOURCE_NOT_FOUND` → 404 → user row missing
- **Guarantees:**
  - Audit, SSE and enumeration failures are logged (`failing_open`) and never fail the update.
  - SSE payloads carry only ids, never the new name. Observers re-fetch.
  - The write retries transient DB errors (USER-009).
  - The name is not compared with the current value: an identical name still writes, audits and fans out.
- **Source:** `src/extractors/multipart.rs:66-78,162-169`, `src/utils/deserializers.rs:239-249`, `src/services/user_service.rs:406-429,435-565`, `src/repositories/postgresql/user_repository.rs:197-243`, `src/handlers/users.rs:90-115`
- **Tests:** `multipart.rs` tests `name_is_normalized_like_the_json_path`, `blank_name_is_rejected_400`, `overlong_name_is_rejected_400`, `name_at_100_is_accepted`; `user_service.rs` tests `update_user_name_only_writes_the_new_name`, `update_user_change_emits_user_update_audit_and_profile_sse`, `update_user_succeeds_when_audit_logging_fails`, `update_user_succeeds_when_sse_publish_fails`

### USER-004 Profile-update form contract

- **Behavior:** `PUT /users/me` accepts only `multipart/form-data` with three optional parts: `name` (text), `avatar` (file), `remove_avatar` (text). The image intent is folded into a tri-state:
  - `avatar` part present → set
  - `remove_avatar=true` with no `avatar` part → clear
  - neither → unchanged
- **Kind:** `rule`
- **Trigger:** `PUT /users/me`
- **Auth:** Session (user-scoped)
- **Input & validation:**
  - The `avatar` part needs its own `Content-Type` and a filename in `Content-Disposition`, either `filename*` (RFC 5987) or `filename` (RFC 6266 quoted-string), parsed by the shared `parse_filename`.
  - `remove_avatar` must be exactly `"true"` or `"false"` after trimming; `"false"` is the same as omitting it.
  - Sending both `avatar` and `remove_avatar=true` is rejected.
  - Unknown parts are ignored.
  - The body limit on this route is `STORAGE_MAX_UPLOAD_BYTES` (default 10 MiB) instead of the global 2 MiB. The same-path `GET` keeps the global limit.
- **Errors:**
  - `VALIDATION_ERROR` (`invalid_request_body`) → 400 → the body is not multipart ("Expected a multipart/form-data request body"), a part is malformed ("Invalid multipart form data"), the avatar part lacks a Content-Type or a filename, the remove flag is not a boolean, or set and remove are both sent
  - `PAYLOAD_TOO_LARGE` → 413 → the body exceeds the route limit
- **Source:** `src/extractors/multipart.rs:84-140,148-225`, `src/handlers/mod.rs:169-172`, `src/server.rs:628`, `src/config/environment.rs:504-507,959-963`
- **Tests:** `multipart.rs` tests `name_and_avatar_parse_ok`, `avatar_only_leaves_name_none`, `avatar_filename_with_quote_and_backslash_roundtrips`, `avatar_quoted_filename_form_is_also_accepted`, `avatar_part_without_content_type_is_rejected_400`, `avatar_part_without_filename_is_rejected_400`, `malformed_multipart_body_returns_400_with_generic_message`, `remove_avatar_true_folds_to_cleared`, `remove_avatar_with_name_clears_and_renames`, `remove_avatar_false_leaves_avatar_unchanged`, `avatar_part_and_remove_flag_together_rejected_400`, `remove_avatar_non_boolean_value_rejected_400`, `non_multipart_body_is_rejected_400`, `over_limit_returns_413`, `unknown_part_is_ignored_not_rejected`

### USER-005 Set avatar

- **Behavior:** An `avatar` part stores the image as a new PUBLIC `user_files` row and points `users.avatar_file_id` at it. A `name` sent in the same request is applied in the same transaction. Any previous avatar file row is deleted, and its bytes are queued for the reaper.
- **Kind:** `capability`
- **Trigger:** `PUT /users/me` with an `avatar` part
- **Auth:** Session (user-scoped), behind the verification gate
- **Input & validation:** All checked before anything is stored:
  - Content-Type must be non-empty, at most 255 chars and shaped `type/subtype`. It is not restricted to `image/*`.
  - File name must be non-blank, at most 255 bytes and free of control characters.
  - Bytes must be non-empty.
  - Object storage must be configured.
- **Effects:**
  1. `storage.put("user/{user_id}/{file_id}")`, with a fresh v4 `file_id`.
  2. In one transaction: `SELECT avatar_file_id … FOR UPDATE`; `INSERT user_files (…, visibility='public')` with `file_name` encrypted and the size recorded; `UPDATE users SET name=COALESCE($name,name), avatar_file_id=$new`; `DELETE FROM user_files WHERE id=$prior AND user_id=$caller` when a prior avatar exists and differs from the new one. That delete fires `user_files_enqueue_deletion`, which inserts into `pending_object_deletions`.
  3. Then the same audit and SSE as USER-003.
- **Errors:**
  - `VALIDATION_ERROR` (`field_validation`) → 400 → bad MIME, bad file name, or empty image
  - `SERVICE_UNAVAILABLE` → 503 → `STORAGE_BACKEND=disabled`
  - Storage errors → 503 or 500 (FILE)
  - `RESOURCE_NOT_FOUND` → 404 → user missing
- **Guarantees:** See USER-008 (atomicity and races) and USER-009 (compensation and retry).
- **Source:** `src/services/user_service.rs:312-381`, `src/utils/content_type.rs:23-47`, `src/utils/content_disposition.rs:96-117`, `src/repositories/postgresql/user_repository.rs:332-486`, `migrations/0007_triggers.sql:54-66`
- **Tests:** `user_service.rs` tests `update_user_avatar_rejects_malformed_mime_before_touching_storage`, `update_user_avatar_rejects_empty_image_before_touching_storage`, `update_user_avatar_requires_storage`, `update_user_avatar_forwards_name_option_to_swap_for_column_scoping`; `tests/file_swap_repository_live.rs` `swap_avatar_inserts_new_file_repoints_and_reaps_prior_into_pending`, `swap_avatar_with_name_none_preserves_existing_name`

### USER-006 Remove avatar

- **Behavior:** `remove_avatar=true` nulls `users.avatar_file_id` and deletes the prior avatar's `user_files` row, which queues its bytes. A `name` sent in the same request is applied in the same transaction.
- **Kind:** `capability`
- **Trigger:** `PUT /users/me` with `remove_avatar=true`
- **Auth:** Session (user-scoped), behind the verification gate
- **Input & validation:** USER-004
- **Effects:**
  - One transaction: `SELECT avatar_file_id … FOR UPDATE`; `UPDATE users SET name=COALESCE($name,name), avatar_file_id=NULL`; `DELETE FROM user_files WHERE id=$prior AND user_id=$caller` when a prior avatar exists.
  - Then the same audit and SSE as USER-003.
  - Storage is not required.
- **Errors:** `RESOURCE_NOT_FOUND` → 404 → user missing
- **Guarantees:** If there is no current avatar and no name, the request is a no-op (USER-007). The no-op decision uses the pre-transaction read; the reap target is re-read under the lock.
- **Source:** `src/services/user_service.rs:389-402`, `src/repositories/postgresql/user_repository.rs:500-606`
- **Tests:** `user_service.rs` tests `update_user_clear_with_avatar_forwards_to_clear_repo`, `update_user_clear_with_avatar_and_no_name_forwards_to_clear_repo`; `tests/file_swap_repository_live.rs` `clear_avatar_nulls_pointer_reaps_prior_and_preserves_name`, `clear_avatar_reap_is_owner_scoped_foreign_file_survives`

### USER-007 Empty profile update is a no-op

- **Behavior:** Two requests return 200 with the current profile and do nothing else (no write, no audit, no SSE): one with no `name` and no image intent, and one with `remove_avatar=true` and no `name` when the user has no avatar.
- **Kind:** `rule`
- **Trigger:** `PUT /users/me`
- **Auth:** Session (user-scoped)
- **Effects:** None (one read)
- **Source:** `src/services/user_service.rs:390-392,431`
- **Tests:** `user_service.rs` tests `update_user_empty_request_is_a_noop`, `update_user_clear_when_no_avatar_and_no_name_is_a_noop`

### USER-008 Profile writes are column-scoped and race-safe

- **Behavior:**
  - The profile update never writes `email`, `email_hash`, `email_verified` or `created_at`.
  - A name-only update writes only `name` and `updated_at`.
  - The avatar paths write `name` only when it is supplied, through `COALESCE` under the row lock.
  - Racing avatar swaps or clears are serialized by `SELECT … FOR UPDATE` on the user row. Each one reaps the avatar that is current under the lock, so no file row is orphaned.
  - The reap delete is scoped to the caller's own files.
- **Kind:** `guarantee`
- **Trigger:** `PUT /users/me`
- **Auth:** Session; RLS user context (`with_user_context`)
- **Guarantees:**
  - A concurrent email change, rename or avatar change is never reverted by a stale snapshot.
  - The avatar's file row and the `avatar_file_id` link commit together or not at all.
  - The `users.avatar_file_id` FK is `ON DELETE SET NULL`, with no same-owner constraint at the DB level.
- **Source:** `src/repositories/postgresql/user_repository.rs:168-243,339-354,377-393,447-470,506-517,529-544`, `migrations/0002_tables.sql:667-672`
- **Tests:** `tests/file_swap_repository_live.rs` `concurrent_avatar_swaps_serialize_and_orphan_nothing`, `swap_avatar_reap_is_owner_scoped_foreign_file_survives`, `swap_avatar_with_name_none_preserves_existing_name`

### USER-009 Avatar write compensation and retry

- **Behavior:**
  - Every profile write path is wrapped in `retry_database_operation`: 3 attempts, exponential backoff, transient errors only.
  - The avatar `INSERT` is idempotent on the pinned file id (`ON CONFLICT (id)`), and the reap is guarded `prior != new`, so retrying after a lost commit ACK is safe.
  - If the swap transaction ultimately fails, the service deletes the just-uploaded object best-effort. A failed cleanup is logged as `orphan_object_cleanup_failed` and the original error is returned.
- **Kind:** `hardening`
- **Trigger:** `PUT /users/me` failure paths
- **Source:** `src/services/user_service.rs:348-380,393-401,420-428`, `src/utils/database_retry.rs:90-104,136-160`, `src/repositories/postgresql/user_repository.rs:395-419,457-458`
- **Tests:** `user_service.rs` test `update_user_writes_object_first_and_compensates_on_swap_failure`; `tests/file_swap_repository_live.rs` `swap_avatar_is_idempotent_when_new_file_is_current_avatar`

### USER-010 Set telemetry consent

- **Behavior:** Sets `user_preferences.telemetry_consent` for the caller and returns 200 with `data: {}`. The new value reaches the client in every later session-context response (`UserResponse.telemetry_consent`).
- **Kind:** `capability`
- **Trigger:** `PUT /users/me/telemetry-consent`
- **Auth:** Session (user-scoped), behind the verification gate
- **Input & validation:** JSON `{"consent": bool}` (required). There are no other rules; unknown fields are ignored.
- **Effects:**
  - Reads the current value (a missing row counts as `false`).
  - `UPDATE user_preferences SET telemetry_consent, updated_at`, leaving `default_workspace_id` untouched.
  - Best-effort user audit `user.telemetry_consent_changed`, `old_value {telemetry_consent: old}` → `new_value {telemetry_consent: new}`.
  - Best-effort SSE `telemetry.consent_changed` (user-scoped) with payload `{updated_by_session_id}`.
  - Logs `"Telemetry consent updated"`.
- **Errors:**
  - `VALIDATION_ERROR` → 400 → malformed JSON or a missing or non-boolean `consent`
  - `RESOURCE_NOT_FOUND` → 404 → no preferences row while changing the value (the update uses `fetch_one`)
- **Guarantees:**
  - Audit and SSE failures never fail the change.
  - RLS (`user_preferences_access`) limits the write to the caller's own row.
  - There is no retry and no lock, so two concurrent sets can each audit a stale `old_value`.
- **Source:** `src/handlers/users.rs:227-238`, `src/models/auth.rs:634-637`, `src/services/user_service.rs:1682-1766`, `src/repositories/postgresql/user_preferences_repository.rs:245-272,335-370`, `src/handlers/mod.rs:186-189`
- **Tests:** `user_service.rs` tests `set_telemetry_consent_change_forwards_value_and_audits_transition`, `set_telemetry_consent_change_publishes_user_scoped_sse`, `set_telemetry_consent_change_succeeds_when_audit_logging_fails`, `set_telemetry_consent_change_succeeds_when_sse_publish_fails`; `tests/user_preferences_repository_live.rs` `telemetry_consent_self_write_is_column_scoped_and_rls_blocks_foreign_writes`

### USER-011 Redundant consent set is a no-op

- **Behavior:** Setting consent to its stored value returns 200 with no write, audit or SSE.
- **Kind:** `rule`
- **Trigger:** `PUT /users/me/telemetry-consent`
- **Auth:** Session
- **Source:** `src/services/user_service.rs:1699-1701`
- **Tests:** `user_service.rs` test `set_telemetry_consent_redundant_set_is_a_noop`

### USER-012 Telemetry consent defaults to off

- **Behavior:**
  - Registration creates the preferences row with `telemetry_consent=false`; the column default is `FALSE`.
  - Every user-scoped `AuthContext` reads the value from `user_preferences`, treating a missing row as `false`.
  - Workspace-scoped contexts always carry `false`, because they do not load preferences.
- **Kind:** `rule`
- **Trigger:** Registration (AUTH); every authenticated request (middleware)
- **Auth:** n/a
- **Source:** `migrations/0002_tables.sql:269-278`, `src/services/public_auth_service.rs:536,643`, `src/auth/middleware.rs:157-178,268-271`, `src/handlers/auth.rs:114-124`
- **Tests:** none found

### USER-013 Change password from settings

- **Behavior:** A caller with a credentials identity who supplies the correct current password gets a new password hash. Every other live session is revoked, the current session stays signed in, and the response is 200 with `data: {}`.
- **Kind:** `capability`
- **Trigger:** `PUT /users/me/password`
- **Auth:** Session (user-scoped), behind the verification gate. Gated on holding a credentials identity, not on the session's auth method (USER-014).
- **Input & validation:** JSON:
  - `current_password`: 1-128 chars, not trimmed, no common-password check
  - `new_password`: 8-128 chars plus `validate_password_not_common` (blocklist, a single repeated character, or an all-digit sequential run of 8 or more)
  - `new_password` may equal `current_password`
- **Effects:** In order:
  1. Per-user rate-limit check (USER-015).
  2. Argon2id verify of the current password, then hash of the new one.
  3. RFC 7009 provider revocation (best-effort) of OAuth tokens belonging to the user's other live sessions. This runs before the transaction.
  4. One transaction through `sp_public_change_password`: update `user_identities.password_hash` where `method='credentials'`; delete `pending_email_changes` and the `email_change` verification code (USER-016); delete the user's other sessions where `refresh_token_expires_at > NOW()` (their `oauth_tokens` cascade); then the in-tx audit `auth.password_changed` with `new_value {auth_method:"credentials", revoked_other_session_count}`.
  5. After commit, best-effort: SSE `session.revoked` (user-scoped) with `{session_ids, revoked_by_session_id}`, sent only when the list is non-empty.
  6. Best-effort `password_change_confirmation` email to `ctx.user_email`.
- **Errors:**
  - `RATE_LIMIT_EXCEEDED` → 429 → bucket exhausted
  - `AUTHENTICATION_FAILED` → 401 → no credentials identity ("Password change is only available for email and password accounts"), a NULL stored hash, a wrong current password ("Invalid current password"), or the SP finds no identity row
  - `VALIDATION_ERROR` → 400 → body validation
  - `INTERNAL_SERVER_ERROR` → 500 → hash or verify crypto failure
- **Guarantees:**
  - The password rotation, session reap and audit are atomic.
  - Sessions that have already expired are left to the expired-session cleanup.
  - Pending `password_reset` codes are not touched.
- **Source:** `src/handlers/users.rs:189-204`, `src/models/auth.rs:591-605`, `src/services/user_service.rs:1059-1277`, `src/repositories/postgresql/public_user_repository.rs:488-513`, `migrations/0005_stored_procedures.sql:1329-1392`, `src/auth/revocation.rs:245-300`, `src/handlers/mod.rs:184`
- **Tests:** `user_service.rs` tests `change_password_rejects_wrong_current_password`, `change_password_collapses_missing_password_hash_to_invalid_credentials`; `tests/user_sse_fanout_live.rs` `change_password_publishes_session_revoked_to_other_devices`, `change_password_succeeds_when_sse_publish_fails`, `change_password_succeeds_when_confirmation_email_fails`; `tests/auth_transaction_flows.rs` `change_password_rotates_hash_reaps_alive_others_and_cancels_pending_email_change`, `change_password_noops_for_user_without_credentials_identity`

### USER-014 Password change needs a credentials identity, not a credentials session

- **Behavior:** The gate is whether a `user_identities` row with `method='credentials'` exists. A Google or GitHub session on an account with a linked password can change the password; an OAuth-only account gets 401.
- **Kind:** `rule`
- **Trigger:** `PUT /users/me/password`
- **Auth:** Session
- **Errors:** `AUTHENTICATION_FAILED` → 401 → no credentials identity
- **Source:** `src/services/user_service.rs:1065-1067,1113-1127`, `src/repositories/postgresql/public_user_repository.rs:310-337`
- **Tests:** `user_service.rs` tests `change_password_rejects_oauth_session_without_credentials_identity`, `change_password_proceeds_for_oauth_session_with_linked_credentials`, `change_password_collapses_missing_credentials_identity_to_invalid_credentials`

### USER-015 Current-password probes share one per-user rate-limit bucket

- **Behavior:** Password change, account deletion (credentials accounts only) and credentials unlink (AUTH) all check the key `password_change:{user_id}` before the Argon2id verify. The limits are `PASSWORD_CHANGE_RATE_LIMIT_MAX_ATTEMPTS` (default 5) per `PASSWORD_CHANGE_RATE_LIMIT_WINDOW_SECS` (default 900), in a fixed window where every call consumes one unit. Email change uses a separate bucket (USER-021).
- **Kind:** `guarantee`
- **Trigger:** `PUT /users/me/password`, `POST /users/me/delete`, `POST /auth/identities/credentials/unlink`
- **Auth:** Session
- **Errors:** `RATE_LIMIT_EXCEEDED` → 429 → limit reached and `ENFORCE_RATE_LIMITS=true`. The message names the retry seconds (at least 1), and `X-RateLimit-*` headers are set (HTTP).
- **Guarantees:**
  - Fail-open: if the limiter itself errors, the verify still runs, with a warning logged (`rate_limit_check_failed`).
  - With `ENFORCE_RATE_LIMITS=false` the bucket is counted but never enforced.
  - For password change, the check runs before the identity lookup, so OAuth-only callers also consume budget.
  - For deletion, it runs only for credentials accounts.
- **Source:** `src/services/user_service.rs:43-49,597-639,1069-1111`, `src/services/identity_service.rs:774`, `src/services/rate_limit_service.rs:78-97`, `src/config/environment.rs:469-476,872-883,1533-1539`
- **Tests:** `user_service.rs` tests `change_password_rejects_when_rate_limit_exceeded`, `delete_user_rejects_when_rate_limit_exceeded`

### USER-016 Password change cancels an in-flight email change

- **Behavior:** A successful settings password change deletes the user's `pending_email_changes` row and `email_change` verification code in the same transaction, so a pending change can no longer be confirmed. Password reset (AUTH) does the same. Credentials unlink and OAuth unlink do not.
- **Kind:** `rule`
- **Trigger:** `PUT /users/me/password`
- **Auth:** Session
- **Effects:** `DELETE pending_email_changes`, `DELETE verification_codes WHERE purpose='email_change'`
- **Source:** `migrations/0005_stored_procedures.sql:1353-1363` (reset: `:1252-1260`)
- **Tests:** `tests/auth_transaction_flows.rs` `change_password_rotates_hash_reaps_alive_others_and_cancels_pending_email_change`

### USER-017 Request email change (phase 1)

- **Behavior:** Stages a pending change of the account email to `new_email` and sends a 6-digit code to the new address. It also sends a security notice, which includes the new address, to the current address. Returns 200 with `{expires_at}`.
- **Kind:** `capability`
- **Trigger:** `POST /users/me/email`
- **Auth:** Session (user-scoped). Exempt from the verification gate (USER-033).
- **Input & validation:** JSON:
  - `new_email`: trimmed and lowercased at deserialization, 1-254 chars, email format
  - `current_password`: optional, 1-128 chars if present
  - Check order: rate limit (USER-021) → email backend present (USER-020) → same address (USER-019) → credential challenge (USER-018) → collision with another account (USER-019)
- **Effects:**
  1. Encrypts `new_email` (AES-GCM) and computes its HMAC blind index.
  2. Generates a 6-digit code; only its HMAC is stored.
  3. `expires_at = now + EMAIL_CHANGE_CODE_EXPIRY_MINUTES` (default 15).
  4. One SP call, `sp_public_initiate_email_change`, upserts `pending_email_changes` (one per user) and the `verification_codes` row for purpose `email_change` (one per user and purpose). It writes fresh ids for both, `attempts_used=0`, `max_attempts=EMAIL_CHANGE_CODE_MAX_ATTEMPTS` (default 5), and the shared expiry.
  5. Sends the `email_change_verification` email to the new address synchronously; a send failure fails the request.
  6. Sends the `email_change_notice` email to `ctx.user_email`, best-effort.
  7. No audit and no SSE.
- **Errors:**
  - `VALIDATION_ERROR` → 400 → body validation
  - Others are listed under USER-018 to USER-021.
  - Email send failure → 500 or 503 (MAIL classification). The pending state and code have already committed at that point.
- **Guarantees:** The current address remains the account email until confirmation.
- **Source:** `src/handlers/auth.rs:1080-1116,710-714`, `src/models/auth.rs:654-673`, `src/services/user_service.rs:1289-1507`, `src/repositories/postgresql/public_verification_code_repository.rs:274-305`, `migrations/0005_stored_procedures.sql:1414-1446`, `templates/email/email_change_notice.txt`, `src/handlers/mod.rs:123`
- **Tests:** `user_service.rs` test `request_email_change_credentials_success_persists_and_sends_both`; `tests/auth_transaction_flows.rs` `initiate_email_change_upserts_pending_and_resets_code_attempts`

### USER-018 Email change account-control challenge

- **Behavior:**
  - Account with a credentials identity: `current_password` is required and is verified with Argon2id.
  - OAuth-only account: `current_password` must be omitted; the session alone authorizes.
  - The decision depends on whether the identity exists, not on the session's auth method.
- **Kind:** `rule`
- **Trigger:** `POST /users/me/email`
- **Auth:** Session
- **Errors:** `AUTHENTICATION_FAILED` → 401 → password missing for a credentials account ("Current password is required to change your email"), wrong password or NULL hash ("Invalid current password"), or a password supplied for an OAuth-only account ("This account has no password to verify")
- **Guarantees:** Uses the `email_change` bucket, not the shared `password_change` bucket (see Observations).
- **Source:** `src/services/user_service.rs:1360-1405`
- **Tests:** `user_service.rs` tests `request_email_change_oauth_only_with_password_rejected`, `request_email_change_credentials_without_password_rejected`, `request_email_change_credentials_wrong_password_rejected`

### USER-019 Email change rejects a no-op or an address in use

- **Behavior:**
  - A `new_email` equal to the current email (both normalized) is rejected before any lookup.
  - A `new_email` that already belongs to another account is rejected before anything is written. The lookup is an HMAC over all key versions, and a decrypted-email mismatch raises a data-integrity error.
- **Kind:** `rule`
- **Trigger:** `POST /users/me/email`
- **Auth:** Session
- **Errors:**
  - `VALIDATION_ERROR` (`business_rule`) → 400 → same address ("The new email matches your current email")
  - `RESOURCE_CONFLICT` → 409 → the address is used by another account. The message is "That email address is already in use" and the body carries `details: {reason: "email_already_in_use"}`.
  - `INTERNAL_SERVER_ERROR` (`DATA_INTEGRITY_ERROR`) → 500 → decryption mismatch
- **Guarantees:** This is a pre-flight check only. The DB constraint at confirm is the authority (USER-026). Another user's pending change to the same address is not checked.
- **Source:** `src/services/user_service.rs:1350-1358,1407-1419`, `src/repositories/postgresql/public_user_repository.rs:128-174`, `src/errors/auth_error.rs:617-622`, `src/errors/api_error.rs:962-970`
- **Tests:** `user_service.rs` tests `request_email_change_same_email_rejected_before_lookup`, `request_email_change_rejects_address_owned_by_another_account`

### USER-020 Email change requires an email backend

- **Behavior:** With `EMAIL_BACKEND=disabled` (the default), every email-change request fails with 500 after the rate-limit check, including a same-address request.
- **Kind:** `rule`
- **Trigger:** `POST /users/me/email`
- **Auth:** Session
- **Errors:** `INTERNAL_SERVER_ERROR` → 500 → no email service ("Email service not configured — cannot change email"; generic message on the wire)
- **Source:** `src/services/user_service.rs:1341-1348`, `src/config/environment.rs:280-292`
- **Tests:** `user_service.rs` test `request_email_change_no_email_service_returns_internal_error`

### USER-021 Email change request rate limit

- **Behavior:** The per-user bucket `email_change:{user_id}` is checked first in the request, before any other check. Limits are `EMAIL_CHANGE_RATE_LIMIT_MAX_ATTEMPTS` (default 3) per `EMAIL_CHANGE_RATE_LIMIT_WINDOW_SECS` (default 300). Every request consumes one unit, including those that are rejected later. Confirm does not use this bucket.
- **Kind:** `guarantee`
- **Trigger:** `POST /users/me/email`
- **Auth:** Session
- **Errors:** `RATE_LIMIT_EXCEEDED` → 429 ("Too many email change requests. Try again in N seconds")
- **Guarantees:** Fail-open if the limiter errors. Enforcement depends on `ENFORCE_RATE_LIMITS`.
- **Source:** `src/services/user_service.rs:51-56,1295-1339`, `src/config/environment.rs:477-484,885-895,1541-1547`
- **Tests:** `user_service.rs` test `request_email_change_rejects_when_rate_limit_exceeded`

### USER-022 One in-flight email change per user; a new request replaces it

- **Behavior:** A second request replaces both the pending address and the code: new ids, attempts reset to 0, new expiry. The earlier code stops working. The pending change belongs to the user, not to a session, so any of the user's sessions can confirm it. There is no cancel endpoint.
- **Kind:** `rule`
- **Trigger:** `POST /users/me/email`
- **Auth:** Session
- **Guarantees:** The address and the code are written in one SP, so their ids always change together. The upsert lock on the pending row serializes racing requests.
- **Source:** `migrations/0005_stored_procedures.sql:1397-1446`, `migrations/0002_tables.sql:201-216`, `src/repositories/postgresql/public_verification_code_repository.rs:284-294`
- **Tests:** `tests/auth_transaction_flows.rs` `initiate_email_change_upserts_pending_and_resets_code_attempts`

### USER-023 Confirm email change (phase 2)

- **Behavior:** A valid code moves the pending address onto the account and sets `email_verified=true`. The response is 200 with a full `SessionContextResponse {session_id, user, workspace, permissions, expires_at, auth_method}`. `user.email` is the new address and `email_verified` is `true`; name and avatar come from the request context. `workspace` and `permissions` are re-resolved from the default workspace, or null and empty when the user has no access. No new tokens are issued.
- **Kind:** `capability`
- **Trigger:** `POST /users/me/email/confirm`
- **Auth:** Session (user-scoped). Exempt from the verification gate.
- **Input & validation:** JSON `{"code"}`, trimmed, exactly 6 characters, ASCII digits only.
- **Effects:**
  1. Code validation (USER-024).
  2. One transaction: `sp_public_complete_email_change` locks the pending row, checks the code id (USER-025), deletes the pending row and the code, and runs `UPDATE users SET email, email_hash, email_verified=TRUE, updated_at`. Then the in-tx audit `auth.email_changed` with `old_value {email_hash: HMAC(old)}` and `new_value {email_hash: HMAC(new), auth_method}`. No plaintext addresses go into the audit.
  3. Best-effort `email_change_confirmation` email to the new address.
  4. Best-effort SSE `identity.email_changed` (user-scoped) with `{changed_by_session_id}`.
  5. Nothing is sent to the old address at this step. Sessions, identities and OAuth tokens are not touched.
- **Errors:**
  - `VALIDATION_ERROR` → 400 → body validation, or "No pending email change. Please request a new code." (no pending row, or the code was swapped)
  - Code errors: USER-024
  - `RESOURCE_CONFLICT` → 409 → race (USER-026)
  - Workspace or permission resolution after commit can still fail the response.
- **Guarantees:**
  - The swap and the audit are atomic. OAuth login never rewrites `users.email`; only this SP and key re-encryption write that column.
  - Workspace invitations are matched by `email_hash`, so the invitations the user sees change with the address. That is why the SSE event exists.
- **Source:** `src/handlers/auth.rs:1118-1178,175-199,40-57`, `src/models/auth.rs:675-687`, `src/services/user_service.rs:1519-1669`, `src/repositories/postgresql/public_verification_code_repository.rs:332-354`, `migrations/0005_stored_procedures.sql:1448-1554`, `src/handlers/mod.rs:124`
- **Tests:** `tests/email_change_confirm_http.rs` `confirm_email_change_promotes_address_through_the_real_stack`; `tests/user_sse_fanout_live.rs` `confirm_email_change_publishes_identity_email_changed`, `confirm_email_change_succeeds_when_sse_publish_fails`, `confirm_email_change_succeeds_when_confirmation_email_fails`; `tests/auth_transaction_flows.rs` `complete_email_change_promotes_address_and_consumes_pending_and_code`, `complete_email_change_noops_without_a_pending_change`

### USER-024 Email-change code validation

- **Behavior:** The shared validator runs these checks in order:
  1. A `email_change` code row must exist.
  2. It must not be expired.
  3. `attempts_used` is incremented atomically, guarded by `max_attempts`. Every submission counts as an attempt, the correct one included.
  4. The submitted code's HMAC, computed with every key version, must match the stored hash.
- **Kind:** `rule`
- **Trigger:** `POST /users/me/email/confirm`
- **Auth:** Session
- **Errors:** `VALIDATION_ERROR` (`business_rule`) → 400 → one of:
  - "No active verification code. Please request a new code."
  - "Verification code has expired. Please request a new code."
  - "Too many failed attempts. Please request a new code."
  - "Invalid verification code. N attempt(s) remaining." with `details {reason:"invalid_verification_code", remaining_attempts}`; at zero remaining the message ends "No attempts remaining. Please request a new code."
- **Guarantees:**
  - A wrong code promotes and consumes nothing.
  - The pending row's own `expires_at` is not checked at confirm; the code shares the same expiry.
  - Confirm has no per-user rate limit beyond the attempt cap and the generic per-user tier (HTTP).
- **Source:** `src/auth/verification_code_validator.rs:23-103`, `src/services/user_service.rs:1530-1537`
- **Tests:** `tests/email_change_confirm_http.rs` `confirm_email_change_rejects_wrong_code_and_promotes_nothing`

### USER-025 Confirm is bound to the code that was validated

- **Behavior:** Validation commits on its own, before the promotion transaction. The promotion SP takes the validated code id and promotes only if the user's current `email_change` code still has that id. If a concurrent request replaced the pair in between, confirm returns "No pending email change" and nothing is promoted.
- **Kind:** `guarantee`
- **Trigger:** `POST /users/me/email/confirm` racing `POST /users/me/email`
- **Auth:** Session
- **Errors:** `VALIDATION_ERROR` → 400 → pair swapped
- **Guarantees:** An address whose code was never entered cannot be promoted. Lock order is the pending row, then the code row, matching every other writer of these tables.
- **Source:** `src/services/user_service.rs:1524-1537,1555-1563,195-204`, `migrations/0005_stored_procedures.sql:1448-1470,1507-1532`
- **Tests:** `tests/auth_transaction_flows.rs` `complete_email_change_aborts_when_expected_code_id_was_swapped`; `user_service.rs` test `resolve_email_change_outcome_maps_none_to_business_rule`

### USER-026 Cross-account race at confirm returns 409

- **Behavior:** If another account claimed the destination after the pre-flight check, the `users` update violates `users_email_hash_unique`. The service maps that constraint, and only that one, to `EmailAlreadyInUse`. The transaction rolls back, so the pending row and the code survive until a retry or expiry. The confirm attempt is still counted.
- **Kind:** `guarantee`
- **Trigger:** `POST /users/me/email/confirm`
- **Auth:** Session
- **Errors:**
  - `RESOURCE_CONFLICT` → 409 → `users_email_hash_unique` violation, with `details.reason=email_already_in_use`
  - Any other constraint violation → 409 with the generic "Resource already exists" message
- **Source:** `src/services/user_service.rs:181-221,1599-1602`, `migrations/0005_stored_procedures.sql:1480-1490,1540-1552`, `migrations/0002_tables.sql:36`
- **Tests:** `user_service.rs` tests `resolve_email_change_outcome_returns_committed_address`, `resolve_email_change_outcome_maps_email_hash_unique_to_email_already_in_use`, `resolve_email_change_outcome_passes_through_unrelated_constraint`, `resolve_email_change_outcome_passes_through_non_constraint_errors`

### USER-027 Email change doubles as email verification

- **Behavior:** Both email-change endpoints skip the verification gate, so an unverified credentials user can correct a mistyped address. A successful confirm sets `email_verified=true`, which lifts the gate. A pending `email_verification` code is not removed.
- **Kind:** `rule`
- **Trigger:** `POST /users/me/email`, `POST /users/me/email/confirm`
- **Auth:** Session, verification-exempt
- **Source:** `src/handlers/mod.rs:114-124`, `migrations/0005_stored_procedures.sql:1545-1551`
- **Tests:** `tests/email_change_confirm_http.rs` (the unverified user reaches confirm)

### USER-028 Delete own account

- **Behavior:** Permanently and immediately deletes the caller's `users` row and everything that cascades from it (USER-030). Returns 200 with `data: {}`. There is no grace period and no soft delete.
- **Kind:** `capability`
- **Trigger:** `POST /users/me/delete`
- **Auth:** Session (user-scoped). Exempt from the verification gate, so unverified users can delete. Credential challenge: USER-029.
- **Input & validation:** JSON `{"current_password"?: string}`. If present, 1-128 chars, length check only. A JSON body is required even when the field is omitted: send `{}` with `Content-Type: application/json`.
- **Effects:**
  1. Reads the challenge identity. Rate limit and verify for credentials accounts.
  2. Before deleting, captures:
     - the profile
     - owned workspaces and their member ids, each read under a per-workspace context with empty permissions
     - member-only workspaces with membership ids
     - pending invitations the user authored in workspaces they do not own (SECURITY DEFINER read, which includes workspaces they have since left)
     - ids of all live sessions
  3. Revokes OAuth tokens at the provider (USER-031).
  4. `DELETE FROM users WHERE id=$caller` under RLS user context, with retry (USER-032).
  5. After commit, best-effort audits:
     - user audit `user.delete`: `old_value {email, name, avatar_file_id}`, `metadata {cascade_owned_workspace_count, cascade_owned_workspace_ids, cascade_member_only_workspace_count, cascade_member_only_workspace_ids, cascade_authored_invitation_count, cascade_authored_invitation_ids, revoked_session_count}`
     - per owned workspace, workspace audit `workspace.delete`: `old_value {name, owner_id}`, `metadata {cascade_source:"user.delete"}`
     - per member-only workspace, `member.leave`: `resource_id` is the membership id, `metadata {cascade_source}`
     - per authored invitation, `invitation.cancel`: `old_value {email, permissions}`, `metadata {cascade_source}`
  6. After commit, best-effort SSE, in this order:
     - `session.revoked` (user) `{session_ids: all live, revoked_by_session_id}`
     - per owned workspace, `workspace.deleted` (workspace) `{workspace_id, workspace_name, deleted_by, deleted_by_session_id}` plus `connection.refresh_required` `{reason:"workspace_deleted", workspace_id}` to each other member. The deleting user is excluded.
     - per member-only workspace, `member.left` `{workspace_id, workspace_name, member_id, user_id, user_name, left_by_session_id}`
     - per authored invitation whose invitee is a registered user, `invitation.cancelled` to the invitee `{invitation_id, workspace_id, workspace_name}`
  7. Best-effort `account_deletion_confirmation` email to the old address.
  8. Info log with the cascade counts.
- **Errors:**
  - Challenge errors: USER-029
  - `RATE_LIMIT_EXCEEDED` → 429
  - `RESOURCE_NOT_FOUND` → 404 → the pre-delete profile read finds no row
  - `VALIDATION_ERROR` → 400 → body missing or invalid
- **Guarantees:**
  - Every audit, SSE and email failure is swallowed (`failing_open`) and never fails the request.
  - Audit tables have no FKs, so the rows survive the cascade. The workspace audit insert avoids `RETURNING` so post-cascade RLS does not reject it.
  - The pre-delete capture is not in the same transaction as the DELETE.
- **Source:** `src/handlers/users.rs:150-161`, `src/models/auth.rs:621-629`, `src/services/user_service.rs:580-1046,66-72`, `src/repositories/postgresql/user_repository.rs:258-290`, `migrations/0005_stored_procedures.sql:850-866`, `src/services/workspace_service.rs:53`, `src/services/workspace_member_service.rs:59,124`, `src/handlers/mod.rs:125-145`
- **Tests:** `user_service.rs` tests `delete_user_success_cascade_emits_audit_and_sse_fanout`, `delete_user_succeeds_when_audit_logging_fails`, `delete_user_succeeds_when_sse_publish_fails`; `tests/audit_insert_no_returning_live.rs` `workspace_audit_no_returning_survives_post_cascade_rls`

### USER-029 Account-deletion credential challenge

- **Behavior:**
  - The account has a credentials identity: `current_password` is required. The request is rate-limited in the shared `password_change` bucket, then the password is verified with Argon2id.
  - OAuth-only account: `current_password` must be omitted. There is no rate limit.
  - The decision depends on whether the identity exists, not on the session's auth method.
- **Kind:** `rule`
- **Trigger:** `POST /users/me/delete`
- **Auth:** Session
- **Errors:** `AUTHENTICATION_FAILED` → 401 → one of:
  - "Current password is required to delete this account"
  - "Invalid current password" (wrong password or NULL hash)
  - "This account has no password to verify" (password supplied for an OAuth-only account)
- **Source:** `src/services/user_service.rs:587-673`
- **Tests:** `user_service.rs` tests `delete_user_rejects_credentials_caller_when_password_not_supplied`, `delete_user_rejects_oauth_only_caller_when_password_supplied`, `delete_user_rejects_wrong_current_password`, `delete_user_collapses_missing_password_hash_to_invalid_credentials`, `delete_user_proceeds_for_oauth_session_with_linked_credentials`

### USER-030 What account deletion cascades

- **Behavior:** Deleting the `users` row removes the following through FKs, in one statement:
  - every workspace the user owns, with all of its memberships (other members' included), invitations and `workspace_files`. Other members' `default_workspace_id` is set to NULL.
  - the user's memberships in other workspaces
  - pending invitations the user sent (`invited_by`)
  - all of the user's sessions, live and expired, including the current one, which is invalid immediately. Their `oauth_tokens` go with them.
  - `user_identities`, `verification_codes`, `pending_email_changes`, `user_preferences`, `oauth_tokens`
  - all `user_files`, the avatar included. The trigger queues each one's bytes in `pending_object_deletions`.

  What survives:
  - `workspace_files` the user uploaded to workspaces that survive. The file is kept and `uploaded_by` becomes NULL.
  - user and workspace audit rows, until `AUDIT_LOG_RETENTION_DAYS` (default 365)
  - `rate_limits`, `idempotency_cache` and `sse_events` rows, until their own expiry

  No guards:
  - no requirement to transfer ownership of owned workspaces, whoever their other members are
  - no check for being the last admin of a workspace the user is a member of
- **Kind:** `rule`
- **Trigger:** `POST /users/me/delete`
- **Auth:** Session
- **Effects:** As listed. Cascades are not visible to RLS.
- **Source:** `migrations/0002_tables.sql:269-271,640-672`, `migrations/0007_triggers.sql:54-71`, `src/services/user_service.rs:754-767`
- **Tests:** none found for the full cascade at DB level. The `user_service.rs` cascade test uses mocks.

### USER-031 Provider OAuth tokens revoked before deletion

- **Behavior:** Before the DELETE, every stored OAuth token for the user is revoked at its provider (RFC 7009), whether or not its session is still live. The list includes never-expiring GitHub grants. Revocation is concurrent and best-effort; a fetch or provider failure is logged and deletion proceeds.
- **Kind:** `guarantee`
- **Trigger:** `POST /users/me/delete`
- **Auth:** Session
- **Guarantees:** All revoke calls finish before the cascade destroys the token rows.
- **Source:** `src/services/user_service.rs:739-752`, `src/auth/revocation.rs:300-349`
- **Tests:** `user_service.rs` test `delete_user_revokes_all_provider_tokens_before_cascade`

### USER-032 Account DELETE is retried and idempotent

- **Behavior:** The DELETE runs inside `retry_database_operation` (3 attempts, transient errors only). It treats zero affected rows as success, so a retry after a lost commit ACK does not turn into an error and still runs the post-commit fan-out. The pre-delete `find_by_id` is the not-found check that produces 404.
- **Kind:** `hardening`
- **Trigger:** `POST /users/me/delete`
- **Source:** `src/services/user_service.rs:754-767`, `src/repositories/postgresql/user_repository.rs:263-283`
- **Tests:** none found

### USER-033 Email-verification gate on account routes

- **Behavior:** When `REQUIRE_EMAIL_VERIFICATION=true` (default `false`), an unverified caller on a credentials session gets 403 on these routes: `GET /users/me`, `PUT /users/me`, `PUT /users/me/password` and `PUT /users/me/telemetry-consent`. These routes are exempt from the gate and stay reachable: `POST /users/me/email`, `POST /users/me/email/confirm` and `POST /users/me/delete`.
- **Kind:** `rule`
- **Trigger:** Any of the routes above
- **Auth:** Session
- **Errors:** `PERMISSION_DENIED` → 403 → "Email verification required", with details `EmailNotVerified`
- **Guarantees:** Middleware order:
  - Gated routes: auth → per-user rate limit → verification gate → idempotency → handler.
  - Exempt routes: the same order without the gate.
  - `Idempotency-Key` is optional on POST, PUT and DELETE (HTTP).
- **Source:** `src/handlers/mod.rs:89-191`, `src/server.rs:604-645`, `src/auth/middleware.rs:85-123`, `src/config/environment.rs:363-366`
- **Tests:** `tests/email_verification_middleware_http.rs` `unverified_user_is_blocked_403_when_verification_required`, `unverified_user_is_allowed_when_verification_disabled`, `unverified_user_reaches_verification_exempt_route_when_verification_required`, `verified_user_reaches_handler_when_verification_required`

### USER-034 Account operations are self-scoped

- **Behavior:** No `/users/me` account endpoint takes a user id. Every operation acts on `ctx.user_id` from the validated session. Writes run under RLS user context (`app.current_user_id` set, workspace cleared). Under the policies `users_workspace_access` and `user_preferences_access`, a user-context write can only reach the caller's own `users` and `user_preferences` rows. The SECURITY DEFINER procedures (`sp_public_change_password`, `sp_public_initiate_email_change`, `sp_public_complete_email_change`) are scoped by `WHERE user_id = p_user_id`, where the caller always passes `ctx.user_id`.
- **Kind:** `guarantee`
- **Trigger:** All USER endpoints
- **Auth:** Session
- **Source:** `src/services/user_service.rs:279-282,585,1072,1299`, `migrations/0004_row_level_security.sql:104-117,198-203,238-240,271-273`, `src/repositories/postgresql/user_repository.rs:132,208,261`
- **Tests:** `tests/user_preferences_repository_live.rs` `telemetry_consent_self_write_is_column_scoped_and_rls_blocks_foreign_writes`; `tests/public_user_repository_live.rs` `find_by_id_for_auth_is_scoped_to_the_requested_user`

## Data owned

**Tables:**

- `users` (`migrations/0002_tables.sql:16-37`):
  - `id`
  - `email` (AES-256-GCM ciphertext)
  - `name` (ciphertext)
  - `email_hash` (HMAC-SHA256 blind index, `UNIQUE users_email_hash_unique`)
  - `avatar_file_id` (FK → `user_files(id)` `ON DELETE SET NULL`, `0002:672`, partial index `idx_users_avatar_file_id`, `0003:114-119`)
  - `email_verified` (default FALSE)
  - `created_at`, `updated_at`
  - No CHECK on name length; the 100-char limit is enforced only in the API.
- `user_preferences` (`0002:269-278`): `user_id` (PK, FK `ON DELETE CASCADE`), `default_workspace_id` (managed by WS, FK `ON DELETE SET NULL`), `telemetry_consent` (default FALSE), timestamps.
- `pending_email_changes` (`0002:201-216`): `id`, `user_id` (UNIQUE `pending_email_changes_user_unique`, FK CASCADE), `new_email` (ciphertext), `new_email_hash`, `created_at`, `expires_at`. Index `idx_pending_email_changes_expires_at` (`0003:27-30`).
- `verification_codes` rows with `purpose='email_change'`. The table is shared with AUTH (`0002:166-189`).

**RLS policies** (`0004_row_level_security.sql`):

- `users_workspace_access`: a user can see themselves or members of the current workspace (`:198-203`)
- `user_preferences_access`: own row only (`:238-240`)
- `pending_email_changes_user_policy`: own row only (`:271-273`)

**Stored procedures** (`0005_stored_procedures.sql`):

- `sp_public_change_password` (`:1329-1392`)
- `sp_public_initiate_email_change` (`:1414-1446`)
- `sp_public_complete_email_change` (`:1495-1554`)
- `sp_public_find_authored_invitations_in_unowned_workspaces` (`:850-866`), read by deletion

Used but owned by AUTH or DATA: `sp_public_find_credentials_identity_by_user_id`, `sp_public_find_user_by_email_hash`, `sp_public_get_user_by_id`, `sp_public_get_user_preferences`, `sp_public_create_user_preferences`, `sp_system_re_encrypt_{fetch,update}_users` (CAS-guarded against concurrent profile or email writes, `:1856-1887`), `sp_system_re_encrypt_{fetch,update}_pending_email_change` (`:1980-2008`).

**Background:** `sp_cleanup_expired_pending_email_changes` (`0006_background_tasks.sql:133-150`) deletes pending changes where `expires_at < NOW()`. It runs inside the DATA-owned `auth_cleanup` task (`src/server.rs:1590-1637`), after the session, invitation and verification-code sweeps, with `retry_with_backoff(3)`. `ENABLE_BACKGROUND_CLEANUP` turns it on or off. Users themselves are never swept, so unverified accounts persist (`src/handlers/mod.rs:129-131`).

**Triggers used:** `user_files_enqueue_deletion` (`0007:62-66`), owned by FILE.

## Configuration

| Variable | Default | Validation | Used by |
|---|---|---|---|
| `PASSWORD_CHANGE_RATE_LIMIT_MAX_ATTEMPTS` | 5 | > 0; warns if > 20 | USER-015 (`environment.rs:469-472,872-876,1533-1539`) |
| `PASSWORD_CHANGE_RATE_LIMIT_WINDOW_SECS` | 900 | > 0 | USER-015 (`:473-476,878-883`) |
| `EMAIL_CHANGE_RATE_LIMIT_MAX_ATTEMPTS` | 3 | > 0; warns if > 20 | USER-021 (`:477-480,885-889,1541-1547`) |
| `EMAIL_CHANGE_RATE_LIMIT_WINDOW_SECS` | 300 | > 0 | USER-021 (`:481-484,891-895`) |
| `EMAIL_CHANGE_CODE_EXPIRY_MINUTES` | 15 | 1-60, checked only when `EMAIL_BACKEND=smtp` | USER-017 (`:383-386,1314-1321`) |
| `EMAIL_CHANGE_CODE_MAX_ATTEMPTS` | 5 | > 0, checked only when SMTP | USER-017/024 (`:387-390,1322-1326`) |
| `ENFORCE_RATE_LIMITS` | true | bool | USER-015/021 (`:429`) |
| `STORAGE_MAX_UPLOAD_BYTES` | 10485760 | > 0 | `PUT /users/me` body limit (`:504-507,959-963`) |
| `STORAGE_BACKEND` | `disabled` | `disabled` or `s3` | avatar set returns 503 when disabled (`:294-306`) |
| `EMAIL_BACKEND` | `disabled` | `disabled` or `smtp` | email change returns 500 when disabled; confirmation emails skipped (`:280-292`) |
| `SSE_NOTIFICATION_BACKEND` | `postgres` | `postgres` or `disabled` | all USER SSE emits skipped when disabled (`:254-262`) |
| `REQUIRE_EMAIL_VERIFICATION` | false | requires `EMAIL_BACKEND=smtp` | USER-033 (`:363-366,1263-1268`) |
| `AUDIT_LOG_RETENTION_DAYS` | 365 | > 0; warns if < 30 | how long deletion audit rows, including email and name, survive (`:424,707,1471`) |

## Cross-domain dependencies

USER calls out to:

- **AUTH:**
  - `verify_password` and `hash_password` (Argon2id)
  - `generate_verification_code` and the shared `validate_verification_code` (`src/auth/verification_code_validator.rs`)
  - `revocation::revoke_provider_tokens_for_user` and `revoke_all_provider_tokens_for_user`
  - `user_scoped_auth_middleware`, which builds the `AuthContext` (`user_email`, `user_name`, `avatar_file_id`, `email_verified`, `telemetry_consent`, `session_id`, `auth_method`)
  - `enforce_email_verification_middleware`
  - `PublicUserRepository::find_credentials_identity_by_user_id` and `find_by_email_for_auth`
  - `PublicAuthService::{validate_workspace_access, get_workspace_for_auth, get_user_workspace_permissions}`, used by the confirm handler's context rebuild
- **WS:**
  - workspace repository `find_owned_by_user` and `find_member_only_workspaces_with_membership_id`
  - member repository `get_member_user_ids`
  - public member repository `find_authored_invitations_in_unowned_workspaces`
  - fan-out helpers `publish_workspace_deletion_fanout`, `publish_member_left_fanout` and `publish_invitation_cancelled_fanout`
- **FILE:**
  - `ObjectStorageRepository::{put, delete}`
  - `user_files` rows and `FileVisibility::Public`
  - the object-deletion trigger and reaper
  - `validate_content_type` and `validate_file_name`
- **AUDIT:** `UserAuditLogService::{log_event, log_event_in_tx}` and `WorkspaceAuditLogService::log_event`, with a per-workspace context
- **RT:** `SseEventPublisher::{publish_user_event, publish_workspace_event}` for:
  - `profile.updated`
  - `member.profile_updated`
  - `telemetry.consent_changed`
  - `identity.email_changed`
  - `session.revoked`
  - `workspace.deleted`
  - `connection.refresh_required`
  - `member.left`
  - `invitation.cancelled`
- **MAIL:** `EmailTemplateService::render_{password_change_confirmation, account_deletion_confirmation, email_change_verification, email_change_notice, email_change_confirmation}` and `EmailService::send_email`
- **HTTP:**
  - `ValidatedJson`
  - the multipart extractor
  - the per-user rate-limit tier and `RateLimitService::check_rate_limit`
  - the idempotency middleware
  - the `ApiResponse` envelope and error mapping
- **DATA:**
  - `transaction::{with_user_context, with_public_context}`
  - `retry_database_operation`
  - `DatabaseEncryption` (`encrypt_data`, `decrypt_data`, `hmac_for_search`, `hmac_for_search_all_versions`)
  - key re-encryption of `users` and `pending_email_changes`

Calls into USER:

- AUTH:
  - Registration creates the `users` row and a `user_preferences` row with consent false (`public_auth_service.rs:536,643`).
  - OAuth signup ingests an avatar through `sp_public_ingest_user_avatar` (`0005:255-273`).
  - Login, validate-session, verify-email and refresh responses embed `UserResponse`, defined in `handlers/auth.rs:60-69`.
  - `IdentityServiceImpl` reuses `PASSWORD_CHANGE_RATE_LIMIT_PREFIX` (`identity_service.rs:24,774`) and `AuthenticatedUserRepository::mark_email_verified` (`identity_service.rs:666`).
  - Password reset cancels pending email changes (`0005:1252-1260`).
- FILE: `DELETE /users/me/files/{id}` on the avatar's file nulls `users.avatar_file_id` through the FK.
- WS: member lists expose `avatar_file_id` and the name. The workspace switch response carries the user's name and avatar.

## Observations

1. **Email change uses its own bucket for password guesses.** It verifies the current password against the separate `email_change` bucket (`user_service.rs:1299-1308,1380`). The comment at `user_service.rs:43-48` says all current-password probes share the `password_change` bucket so an attacker "cannot pivot between endpoints". This matters because a stolen session gets 3 extra guesses per 5 minutes on top of 5 per 15 minutes.
2. **Deleting an owner deletes the shared workspaces.** Deleting a workspace owner hard-deletes every owned workspace, including all other members' memberships, files and invitations. There is no ownership transfer, no guard, no grace period and no step beyond the password challenge (`user_service.rs:686-692,754-767`; `0002_tables.sql:644`). Other members lose the data with only a `workspace.deleted` SSE.
3. **The cascade comment undercounts the FKs.** `user_service.rs:754` says the delete cascades "across 8 FK relationships". The schema has 10 `ON DELETE CASCADE` FKs to `users` plus 1 `SET NULL` (`0002:270,644-666`). The trait doc's cascade list (`services/traits.rs:313-319`) leaves out `user_files`, `pending_email_changes` and `workspace_files.uploaded_by`.
4. **The trait doc for `delete_user` is out of date.** It says deletion revokes "every alive OAuth token" and fires "four" SSE fan-outs (`services/traits.rs:342-354`). The code revokes every token regardless of liveness (`user_service.rs:747`) and also emits `invitation.cancelled` (`:994-1004`).
5. **Provider tokens are revoked before the DB write.** In deletion, `user_service.rs:747` runs before `:763`. In password change, `:1151` runs before `:1170`. If the DB step then fails, the provider grants are already gone while the account and sessions remain. There is no compensation.
6. **The deletion capture is not atomic with the DELETE.** The pre-delete capture (`user_service.rs:679-737`) runs in separate statements. Workspaces, memberships, invitations or sessions created before the DELETE (`:763`) are cascaded without audit or SSE.
7. **Deletion keeps the email and name in the audit log.** The `user.delete` audit stores the plaintext email and name in `old_value`, encrypted at rest (`user_service.rs:781-785`). Audit rows survive deletion for `AUDIT_LOG_RETENTION_DAYS`, 365 by default. The deletion email says "all of your account data has been removed" (`templates/email/account_deletion_confirmation.txt`).
8. **Audit handling is inconsistent across operations.**
   - Atomic in-tx audit: password change and email confirm.
   - Best-effort post-commit audit: profile update, consent and deletion.
   - No audit at all: the email-change request and failed current-password attempts on any endpoint.
9. **Email change is weaker than password change after the fact.** It revokes no sessions and sends nothing to the old address on completion; the old address gets a notice only at request time (`user_service.rs:1470-1498,1604-1661`). The notice tells the user to "contact support". There is no endpoint to cancel a pending change, other than changing the password.
10. **A failed verification email still leaves a committed pending change.** If sending the verification email fails, the request returns an error, but the pending address and code are already committed (`user_service.rs:1443-1468`). A retry consumes rate-limit budget.
11. **The email-change pre-flight reveals whether an address is registered.** It returns 409 `email_already_in_use` (`user_service.rs:1410-1419`). Any authenticated user can use it, including OAuth-only users who need no password, rate-limited to 3 per 5 minutes.
12. **Unlink does not cancel a pending email change.** Password change and password reset do (`0005:1252-1260,1353-1363`). `sp_public_unlink_credentials` and `sp_public_unlink_oauth_identity` do not.
13. **Password change is permissive in two ways.** It does not reject a `new_password` equal to the current one (`user_service.rs:1146`). It also leaves any pending `password_reset` code valid (`0005:1305-1307`).
14. **Renames are not de-duplicated.** A name-only update with an unchanged name still writes, audits and fans out to every workspace (`user_service.rs:406-429`). Telemetry consent short-circuits when the value is unchanged (`:1699`).
15. **Avatar MIME is not restricted.** The avatar Content-Type is only shape-checked (`type/subtype`) and is not limited to images (`utils/content_type.rs:23-47`). The file is stored PUBLIC and anyone can download it anonymously at `/files/public/user/{id}`. FILE's download hardening is the only defence.
16. **Deleting the avatar file bypasses the profile flow.** The avatar is an ordinary `user_files` row. Deleting it through `DELETE /users/me/files/{id}` nulls `avatar_file_id` through the FK, with no `user.update` audit and no `profile.updated` or `member.profile_updated` SSE (`0002:672`; `user_file_repository.rs:311`).
17. **`GET /users/me` reads the user twice.** It re-reads the row (`users.rs:41-45`) although the middleware already loaded it into `AuthContext` (`middleware.rs:143-147`). `telemetry_consent` comes from the context while the other fields come from the second read.
18. **The `get_user_by_id` guards cannot be reached over HTTP** (`user_service.rs:242-254`). They are pinned only by unit tests.
19. **A missing preferences row breaks consent changes.** The read treats a missing row as `false`, but setting `true` then fails with 404 because the update uses `fetch_one` and there is no upsert (`user_service.rs:1692-1706`; `user_preferences_repository.rs:359`). The repository comment assumes the row always exists.
20. **A committed email change can still return an error.** After the confirm transaction commits, the handler resolves the workspace and permissions (`auth.rs:1149-1152`). If that fails, the client gets an error for an email change that has already happened.
21. **Deletion needs a JSON body even when it carries nothing.** `POST /users/me/delete` uses `ValidatedJson`, so OAuth-only callers must send `{}` with a JSON content type. An empty body is rejected with 400 (`users.rs:153`; `extractors/json.rs:86-130`).
22. **A disabled email backend returns 500.** The email-change request returns 500 (generic) rather than 503, and does so even for a same-address request, because the backend check comes before the same-address check (`user_service.rs:1343-1358`).
23. **The SSE payload convention is not followed everywhere.** The `profile.updated` comment says USER SSE payloads never carry display values and contain only ids, to keep PII out of stored events (`user_service.rs:466-473`). The `member.left` event that deletion emits through the WS helper carries `user_name` and `workspace_name` (`workspace_member_service.rs:75-83`).
24. **Confirm never checks the pending row's expiry.** `sp_public_complete_email_change` does not read `pending_email_changes.expires_at` (`0005:1495-1554`). Expiry is enforced only through the code, which shares the same timestamp, and the sweep removes rows later.
25. **The email-change handlers log under two subsystems.** They live in `handlers/auth.rs` with tracing `subsystem="auth"`, but the service spans use `subsystem="user"` (`auth.rs:1086-1093`; `user_service.rs:1279-1288`). The route paths are under `/users/me`.
26. **The server does not enforce telemetry consent.** The telemetry proxy accepts batches whatever the consent value; only the client enforces it (`documentation/TELEMETRY.md:118`). OBS owns the proxy.

## Open questions

1. When the owner of a shared workspace deletes their account, should the workspace still be hard-deleted for all members, or is a transfer or other safeguard wanted? (Observation 2)
2. Is immediate hard deletion with no grace period intended? Is keeping the deleted user's email and name in audit logs for the retention period acceptable, given what the deletion email says? (Observation 7)
3. Should the email-change password check share the `password_change` bucket as the comment says, or is the separate bucket intended? (Observation 1)
4. After an email change, should the API revoke other sessions, notify the old address on completion, or offer a cancel endpoint for a pending change? (Observation 9)
5. Should credentials unlink cancel a pending email change, as password change and password reset do? (Observation 12)
6. Is the pre-flight "email already in use" 409 an accepted enumeration surface for authenticated users? (Observation 11)
7. Should avatars be restricted to image MIME types, or given their own size limit below `STORAGE_MAX_UPLOAD_BYTES`? (Observation 15)
8. Should deleting the avatar's file through the files endpoint be blocked, or should it trigger the profile audit and SSE? (Observation 16)
9. Should the email-change request (phase 1) and failed current-password challenges be audited, and should profile, consent and deletion audits be atomic like password change? (Observation 8)
10. With email disabled, should the email-change request return 503 rather than 500? (Observation 22)
11. Should `new_password == current_password` be rejected? Should a settings password change invalidate pending `password_reset` codes? (Observation 13)
12. Should a consent change upsert a missing `user_preferences` row instead of returning 404? (Observation 19)
