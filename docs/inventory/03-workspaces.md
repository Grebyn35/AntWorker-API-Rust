# Inventory: Workspaces, members, invitations, permissions (`WS`)

Reference: `AntWorker-API` @ `0210895` (2026-10-20). This file records what the old API does. It does not prescribe what the new one should do.

## Scope

Covers: workspace create / read / update (name + picture) / delete (and what the delete cascades), listing the caller's workspaces, switching and the stored "active workspace", ownership transfer, members (list, change permissions, remove, leave), invitations (admin side: list, create, update, cancel, resend; invitee side: list pending, accept, decline; expiry), the permission model (`Permission` enum, `Role` presets, `/permissions/schema`, storage, loading, enforcement, owner protections), the workspace-scoped auth context built from `X-Workspace-Id`, and what a newly registered user gets.

Boundary with neighbours:
- AUTH owns login, registration, refresh, validate-session and the session token itself. Their workspace-resolution logic is described here only inside `## Current active-workspace model` (it reads and writes the WS-owned preference), not catalogued as WS behaviors.
- AUDIT owns the audit log machinery; this file lists which audit actions WS operations emit.
- RT owns SSE delivery; this file lists which events WS operations publish and their payloads.
- MAIL owns the invitation email template and transport.
- FILE owns `workspace_files` and the byte store; this file covers only the picture path of `PUT /workspace`.
- USER owns account deletion, whose cascade reuses WS fan-out helpers (see Cross-domain dependencies).
- DATA owns the background-task scheduler that runs the expired-invitation sweep; HTTP owns pagination, extractors, rate limiting, idempotency.

Files read in full: `src/handlers/{workspaces,workspace_members,user_invitations,permissions}.rs`, `src/services/{workspace_service,workspace_member_service,user_invitation_service}.rs` (including all inline tests), `src/models/{workspace,workspace_member,permissions}.rs`, `src/models/auth.rs:1-303`, `src/repositories/postgresql/{workspace_repository,workspace_member_repository,public_workspace_repository,public_workspace_member_repository,rls_context}.rs`, `src/repositories/postgresql/transaction.rs` (production part), `src/auth/middleware.rs`, `migrations/0004_row_level_security.sql`. Read in part: `src/repositories/traits.rs` (workspace traits), `src/services/traits.rs:445-675`, `src/services/public_auth_service.rs:350-750,1460-1620`, `src/handlers/auth.rs:20-200,516-640`, `src/handlers/mod.rs`, `src/server.rs:230-700,1560-1640`, `src/extractors/multipart.rs`, `src/repositories/postgresql/user_preferences_repository.rs:280-420`, `src/errors/{api_error,auth_error}.rs` (mapping), `src/validation/mod.rs:79-246`, `src/middleware/idempotency.rs:820-848`, `src/handlers/sse_notifications.rs:105-135`, `migrations/0002_tables.sql:218-278,636-681`, `0003_indexes.sql:60-130`, `0005_stored_procedures.sql:320-430,690-870`, `0006_background_tasks.sql:70-100`, `0008_grants.sql`. Tests read: `tests/{workspace_fanout_live,workspace_invitation_repository_live,workspace_member_pagination_live,invitation_accept_fanout_live,authored_invitations_repository_live,public_workspace_member_repository_live,public_workspace_repository_live,login_workspace_resolution_live}.rs` (outlines and key assertions), workspace parts of `tests/rls_isolation.rs`, and grep-level coverage of `tests/{http_smoke,repository_column_scoping_live,file_swap_repository_live,user_preferences_repository_live}.rs`. `.env` and `alertmanager-secrets/` exist and were not opened. `documentation/` and `README.md` contain no WS-specific claims beyond a note in `documentation/OBJECT_STORAGE.md:35-38` that any member may write workspace files.

All paths below are relative to `AntWorker-API/`. Wire error codes are the `SCREAMING_SNAKE_CASE` envelope codes from `src/errors/response_envelope.rs:242-276`; the mapping used throughout is `src/errors/api_error.rs:978-1020` (validation → 400 `VALIDATION_ERROR`, service not-found → 404 `RESOURCE_NOT_FOUND` with `{entity_type, entity_id}`, conflict → 409 `RESOURCE_CONFLICT` with generic message and no details, service-unavailable → 503 `SERVICE_UNAVAILABLE`) and `src/errors/api_error.rs:941-950` + `src/errors/auth_error.rs:377-380` (permission denied / workspace access revoked / privilege escalation → 403 `PERMISSION_DENIED`; only the latter two carry `details.reason`).

## Behaviors

### WS-001 New accounts start with zero workspaces

- **Behavior:** Registering (credentials or OAuth) creates no workspace and no membership; the new `user_preferences` row has `default_workspace_id = NULL`, and the registration response carries `workspace: null` and `permissions: []`. The client must create or join a workspace.
- **Kind:** `rule` (constrains what registration yields; trigger is AUTH's, recorded here because the scope asks what a new user gets).
- **Trigger:** `POST /v1/auth/register`, `POST /v1/auth/oauth` for a first-time user.
- **Auth:** public.
- **Effects:** `user_preferences` insert with `default_workspace_id NULL`, `telemetry_consent false`.
- **Source:** `src/services/public_auth_service.rs:531-541,589-591,638-648,688-691`; `src/handlers/auth.rs:357-358`.
- **Tests:** `tests/http_smoke.rs` (step 5 notes signup leaves the account workspace-less).

### WS-002 Create a workspace

- **Behavior:** Any authenticated user creates a workspace and becomes its owner (`owner_id = caller`); in the same transaction a membership row is created for the caller holding every entry of `PERMISSIONS`. Returns the workspace (`id, name, owner_id, picture_file_id: null, created_at, updated_at`).
- **Kind:** `capability`
- **Trigger:** `POST /v1/users/me/workspaces`
- **Auth:** session (user-scoped); verified email required for credentials users when `REQUIRE_EMAIL_VERIFICATION=true`. No permission.
- **Input & validation:** JSON `{ "name": string }`; name normalized at deserialization (trim, collapse spaces, NFC), then 1-255 chars and not whitespace-only. No `owner_id` field exists. IDs and timestamps are server-generated.
- **Effects:** `workspaces` INSERT (name AES-GCM encrypted) under user-scoped RLS; `workspace_memberships` INSERT via `sp_public_create_owner_membership` (SECURITY DEFINER); user audit `workspace.create` (`resource_id` = workspace id, `new_value {name}`) written in the same transaction. Post-commit: user-scoped SSE `connection.refresh_required {reason: "workspace_created", workspace_id}` to the creator. No workspace audit entry. Does not change `default_workspace_id` (WS-003).
- **Errors:** 400 `VALIDATION_ERROR` on name; transient DB errors retried (whole transaction, fresh workspace id per attempt) up to 3 attempts.
- **Guarantees:** Workspace row, owner membership and user audit commit atomically (audit failure rolls back creation). RLS `WITH CHECK` (implicit from `USING`) only admits rows whose `owner_id` is the caller.
- **Source:** `src/handlers/workspaces.rs:91-116`; `src/models/workspace.rs:37-49`; `src/services/workspace_service.rs:198-356`; `src/repositories/postgresql/workspace_repository.rs:922-974`; `src/repositories/postgresql/public_workspace_member_repository.rs:58-96`; `migrations/0005_stored_procedures.sql:359-372`; `migrations/0004_row_level_security.sql:207-215`.
- **Tests:** `tests/workspace_fanout_live.rs` (`create_workspace_fans_out_connection_refresh_and_audits_workspace_create`, `create_workspace_succeeds_when_sse_publish_fails`); `tests/http_smoke.rs`.

### WS-003 Creating a workspace does not make it active

- **Behavior:** After create, `user_preferences.default_workspace_id` is unchanged (still NULL for a new user). The next login / validate-session / refresh will not return the new workspace until the client calls switch.
- **Kind:** `rule`
- **Trigger:** `POST /v1/users/me/workspaces`
- **Auth:** session.
- **Source:** `src/services/workspace_service.rs:198-356` (no preference write); `src/services/public_auth_service.rs:746-748` (NULL default resolves to no workspace).
- **Tests:** none found.

### WS-004 List my workspaces

- **Behavior:** Returns every workspace the caller has a membership row in (owned or not), paginated, newest workspace first by default, with optional case-insensitive substring filter on the decrypted name. Items are full `Workspace` objects (no per-workspace permissions, no `is_owner`; the client compares `owner_id`).
- **Kind:** `capability`
- **Trigger:** `GET /v1/users/me/workspaces`
- **Auth:** session (user-scoped), verified email when enforced.
- **Input & validation:** Pagination params (`limit` 1-100, default 25; `sort_by` allowlist `["created_at"]`, which is the workspace's `created_at`, not join time; cursors). `?name=` normalized (trim/collapse/NFC), 1-100 chars, not whitespace-only.
- **Effects:** Read only. All memberships are fetched, decrypted, filtered, sorted (`created_at`, then `id`) and paginated in memory; response meta includes a total count.
- **Errors:** 400 `VALIDATION_ERROR` for bad params; 403 `PERMISSION_DENIED` if the user id differs from the caller (unreachable from HTTP, the handler passes the caller's id).
- **Source:** `src/handlers/workspaces.rs:49-64`; `src/models/workspace.rs:99-142`; `src/services/workspace_service.rs:406-431`; `src/repositories/postgresql/workspace_repository.rs:641-727`; `src/utils/pagination.rs:734-780`.
- **Tests:** `src/services/workspace_service.rs` (`list_workspaces_by_user_rejects_other_users_id`, `list_workspaces_by_user_rejects_nil_user_id`).

### WS-005 Read the current workspace

- **Behavior:** Returns the workspace named by `X-Workspace-Id`. Any member may read it; no permission required.
- **Kind:** `capability`
- **Trigger:** `GET /v1/workspace`
- **Auth:** workspace-scoped (membership). No permission.
- **Errors:** 404 `RESOURCE_NOT_FOUND` (`workspace`) if the row is not visible; header errors per WS-016.
- **Source:** `src/handlers/workspaces.rs:224-243`; `src/services/workspace_service.rs:369-392`; `src/repositories/postgresql/workspace_repository.rs:128-167`.
- **Tests:** `tests/http_smoke.rs` (member 200, non-member 403).

### WS-006 Update workspace name and/or picture

- **Behavior:** Partial update of the current workspace: an omitted name is unchanged; the picture is set, cleared or unchanged. Returns the updated workspace.
- **Kind:** `capability`
- **Trigger:** `PUT /v1/workspace` (`multipart/form-data`)
- **Auth:** workspace-scoped; `update:workspace`. Checked before any storage or DB work.
- **Input & validation:** Parts: `name` (text; normalized, 1-255, not whitespace-only), `picture` (file; part `Content-Type` and `Content-Disposition` filename both required), `remove_picture` (exactly `"true"` or `"false"` after trim). Sending `picture` and `remove_picture=true` together is rejected. Unknown parts are ignored. The body limit is `STORAGE_MAX_UPLOAD_BYTES` (default 10 MiB) for this route only. The service then checks MIME shape (`type/subtype`), file-name shape and non-empty bytes. MIME is not restricted to images.
- **Effects:** See WS-007 (no-ops) and WS-008 (picture). Name-only: `UPDATE workspaces SET name, updated_at`. Post-commit, best-effort: workspace audit `workspace.update` (`old_value`/`new_value` = `{name, picture_file_id}`), then workspace-scoped SSE `workspace.updated {workspace_id, updated_by, updated_by_session_id}` (ids only; observers re-fetch).
- **Errors:** 403 `PERMISSION_DENIED` ("Missing required permission: update:workspace"); 400 `VALIDATION_ERROR` (multipart shape, name, flag, both set+remove, MIME, filename, empty picture "Picture image must not be empty"); 413 `PAYLOAD_TOO_LARGE`; 503 when a picture is supplied and object storage is disabled; 404 if the workspace row is not visible.
- **Source:** `src/handlers/workspaces.rs:268-286`; `src/handlers/mod.rs:347-350`; `src/extractors/multipart.rs:84-113,120-139,236-328`; `src/services/workspace_service.rs:444-687`.
- **Tests:** `src/services/workspace_service.rs` (`update_workspace_requires_update_permission`, `update_workspace_picture_rejects_malformed_mime_before_touching_storage`, `update_workspace_picture_rejects_empty_image_before_touching_storage`, `update_workspace_picture_requires_storage`, `update_workspace_name_only_writes_the_new_name`, `update_workspace_name_only_emits_audit_and_workspace_updated_sse`, fail-open variants); inline tests in `src/extractors/multipart.rs`.

### WS-007 Workspace update no-op cases

- **Behavior:** If neither `name` nor a picture change is supplied, or `remove_picture=true` is sent with no name while the workspace has no picture, the current row is returned with no write, no audit and no SSE. A supplied name equal to the current name is not a no-op (it writes, audits and publishes).
- **Kind:** `rule`
- **Trigger:** `PUT /v1/workspace`
- **Auth:** workspace-scoped; `update:workspace` (still required for a no-op).
- **Source:** `src/services/workspace_service.rs:572-575,589-615`.
- **Tests:** `src/services/workspace_service.rs` (`update_workspace_empty_request_is_a_noop`, `update_workspace_clear_when_no_picture_and_no_name_is_a_noop`, `update_workspace_clear_with_picture_and_no_name_forwards_to_clear_repo`).

### WS-008 Workspace picture set and clear

- **Behavior:** Setting a picture stores the bytes as a new PUBLIC `workspace_files` row (`storage_key = workspace/{workspace_id}/{file_id}`, `uploaded_by` = caller, file name recorded encrypted), points `workspaces.picture_file_id` at it, and deletes the previous picture's file row (its bytes are queued for reaping by the `workspace_files` delete trigger). Clearing nulls `picture_file_id` and deletes the previous file row. Either may carry a rename in the same transaction. Clients resolve `picture_file_id` to `/files/public/workspace/{id}`.
- **Kind:** `capability`
- **Trigger:** `PUT /v1/workspace` with `picture` or `remove_picture=true`.
- **Auth:** workspace-scoped; `update:workspace`.
- **Effects:** `workspace_files` INSERT (idempotent on id), `workspaces` UPDATE (`name = COALESCE(new, name)`, `picture_file_id`, `updated_at`), prior `workspace_files` DELETE scoped `AND workspace_id = $wid`; same audit + SSE as WS-006.
- **Source:** `src/services/workspace_service.rs:493-585`; `src/repositories/postgresql/workspace_repository.rs:324-477,491-604`; `src/models/workspace.rs:14-19`.
- **Tests:** `tests/file_swap_repository_live.rs` (`swap_picture_inserts_new_file_repoints_and_reaps_prior_into_pending`, `swap_picture_reap_is_workspace_scoped_foreign_file_survives`, `swap_picture_is_idempotent_when_new_file_is_current_picture`, `clear_picture_nulls_pointer_reaps_prior_and_preserves_name`); `src/services/workspace_service.rs` (`update_workspace_picture_forwards_name_option_to_swap_for_column_scoping`, `update_workspace_clear_with_picture_forwards_to_clear_repo`).

### WS-009 Picture write ordering, locking and orphan compensation

- **Behavior:** The object is written to the byte store before the DB transaction; if the transaction fails, the service best-effort deletes the just-written object (a failed cleanup is logged and swallowed; the original error is returned). Inside the transaction the workspace row is locked `FOR UPDATE` and the reap target is read under the lock, so racing picture changes reap the actually-current file. The reap is skipped when the prior picture is the new file (safe on retry).
- **Kind:** `hardening`
- **Trigger:** `PUT /v1/workspace` with a picture.
- **Auth:** workspace-scoped; `update:workspace`.
- **Effects:** log `error_type = "orphan_object_cleanup_failed"`, `action = "failing_open"` on cleanup failure.
- **Source:** `src/services/workspace_service.rs:504-563`; `src/repositories/postgresql/workspace_repository.rs:373-383,440-463,525-537`.
- **Tests:** `src/services/workspace_service.rs` (`update_workspace_writes_object_first_and_compensates_on_swap_failure`).

### WS-010 Column-scoped workspace writes

- **Behavior:** Rename writes only `name` and `updated_at`; ownership transfer writes only `owner_id` and `updated_at`; picture paths write `picture_file_id` and `COALESCE` the optional name. No path writes a column it does not own, so a concurrent rename and transfer cannot revert each other.
- **Kind:** `guarantee`
- **Trigger:** `PUT /v1/workspace`, `POST /v1/workspace/transfer`.
- **Auth:** as the triggering endpoints.
- **Source:** `src/repositories/postgresql/workspace_repository.rs:194-209,420-438,546-557,1009-1024`.
- **Tests:** `tests/repository_column_scoping_live.rs` (`workspace_rename_does_not_clobber_a_completed_ownership_transfer`, `workspace_ownership_transfer_does_not_revert_a_concurrent_rename`).

### WS-011 Delete a workspace (owner only)

- **Behavior:** The workspace owner deletes the current workspace. Member user ids are captured before the delete. The API does not auto-switch the caller elsewhere.
- **Kind:** `capability`
- **Trigger:** `DELETE /v1/workspace`
- **Auth:** workspace-scoped; caller must be `workspaces.owner_id` (no permission can grant this).
- **Effects:** `DELETE FROM workspaces` (idempotent; retried on transient errors) and the cascade in WS-012; then, best-effort, workspace audit `workspace.delete` (`old_value {name, owner_id}`), then clear the caller's `default_workspace_id` (WS-013, not best-effort), then SSE: workspace-scoped `workspace.deleted {workspace_id, workspace_name, deleted_by, deleted_by_session_id}` followed by one user-scoped `connection.refresh_required {reason: "workspace_deleted", workspace_id}` per captured member (owner included), in that order. Returns an empty success envelope.
- **Errors:** 404 if the row is not visible; 403 `PERMISSION_DENIED` "Only workspace owners can delete workspaces"; 404 (`user_preferences`) if the caller's preferences row is missing, after the deletion has committed.
- **Source:** `src/handlers/workspaces.rs:316-326`; `src/services/workspace_service.rs:53-121,711-838`; `src/repositories/postgresql/workspace_repository.rs:248-278`; `src/repositories/postgresql/workspace_member_repository.rs:707-739`.
- **Tests:** `src/services/workspace_service.rs` (`delete_workspace_rejects_non_owner`, `delete_workspace_emits_audit_and_deletion_fanout`, `delete_workspace_succeeds_when_audit_logging_fails`, `delete_workspace_succeeds_when_sse_publish_fails`).

### WS-012 Workspace deletion cascade

- **Behavior:** Deleting a workspace row removes all its memberships, invitations and `workspace_files` rows (FK `ON DELETE CASCADE`; file bytes are queued for the reaper by the delete trigger), and sets `user_preferences.default_workspace_id` to NULL for every user whose stored default was that workspace (FK `ON DELETE SET NULL`). `workspace_audit_logs` rows are kept (no FK) but become unreadable through the API (select policy requires membership). Pending invitees get no SSE and no email.
- **Kind:** `guarantee`
- **Trigger:** `DELETE /v1/workspace`; also account deletion of the owner (USER).
- **Auth:** owner (see WS-011).
- **Source:** `migrations/0002_tables.sql:271,642-681`; `migrations/0004_row_level_security.sql:281-283`; `src/services/workspace_service.rs:753-776`.
- **Tests:** `tests/object_deletion_trigger.rs` (file-row cascade enqueue, by grep); none found for the preference SET NULL.

### WS-013 Delete and leave clear the caller's stored active workspace

- **Behavior:** After a successful delete (WS-011) or leave (WS-025), the caller's `user_preferences.default_workspace_id` is set to NULL unconditionally, whether or not it pointed at the deleted/left workspace. Other members' defaults are not touched (except by the FK in WS-012). A member removed by an admin (WS-023) keeps their stale default.
- **Kind:** `rule`
- **Trigger:** `DELETE /v1/workspace`, `POST /v1/workspace/leave`.
- **Auth:** as the triggering endpoint.
- **Effects:** `UPDATE user_preferences SET default_workspace_id = NULL`. Not wrapped in fail-open: an error (including "no preferences row" → 404) is returned to the client after the main mutation committed, and the subsequent SSE fan-out is skipped.
- **Source:** `src/services/workspace_service.rs:808-814`; `src/services/workspace_member_service.rs:755-761`; `src/repositories/postgresql/user_preferences_repository.rs:385-420`.
- **Tests:** `src/services/workspace_member_service.rs` (`leave_workspace_success_clears_default_audits_and_fans_out`); none found for delete.

### WS-014 Transfer workspace ownership

- **Behavior:** The owner hands ownership to another existing member: `workspaces.owner_id` is set to the new owner and the new owner's membership permissions are overwritten with the full `PERMISSIONS` set. The previous owner stays a member and keeps their existing permissions (full set). Returns the updated workspace.
- **Kind:** `capability`
- **Trigger:** `POST /v1/workspace/transfer`
- **Auth:** workspace-scoped; caller must be the current owner.
- **Input & validation:** JSON `{ "new_owner_id": uuid }` (no other validation).
- **Effects:** In one transaction (retried on transient errors): `UPDATE workspaces SET owner_id`, `UPDATE workspace_memberships SET permissions = PERMISSIONS`, workspace audit `workspace.transfer` (`old_value {owner_id}`, `new_value {owner_id, new_owner_email}`) and user audit `workspace.transfer` (`new_value {workspace_id, new_owner_id, new_owner_email}`). Post-commit, best-effort: workspace-scoped SSE `workspace.transferred {workspace_id, workspace_name, previous_owner_id, new_owner_id, new_owner_name, transferred_by_session_id}`. No `connection.refresh_required`. `default_workspace_id` unchanged for both users.
- **Errors:** 404 (workspace not visible); 403 `PERMISSION_DENIED` "Only workspace owners can transfer workspaces"; 400 `VALIDATION_ERROR` "Cannot transfer workspace to yourself"; 404 (`workspace_member`) when the target is not a member of this workspace.
- **Guarantees:** Owner change, permission grant and both audit rows are atomic.
- **Source:** `src/handlers/workspaces.rs:356-367`; `src/models/workspace.rs:60-64`; `src/services/workspace_service.rs:994-1231`; `src/repositories/postgresql/workspace_repository.rs:989-1045`; `src/repositories/postgresql/workspace_member_repository.rs:1626-1728`.
- **Tests:** `src/services/workspace_service.rs` (`transfer_workspace_rejects_non_owner`, `transfer_workspace_rejects_self_transfer`, `transfer_workspace_rejects_non_member_new_owner`); `tests/workspace_fanout_live.rs` (`transfer_workspace_fans_out_transferred_and_audits_both_logs`, `transfer_workspace_succeeds_when_sse_publish_fails`).

### WS-015 Switch the active workspace

- **Behavior:** Verifies the caller is a member of the target workspace, stores it as `user_preferences.default_workspace_id`, and returns a token-free session context for it: `{session_id, user{id,email,name,avatar_file_id,email_verified,telemetry_consent}, workspace{id,name,picture_file_id,is_owner} | null, permissions[], expires_at, auth_method}`. Switching to the already-stored workspace is allowed and still writes and audits.
- **Kind:** `capability`
- **Trigger:** `POST /v1/users/me/workspaces/{id}/switch`
- **Auth:** session (user-scoped, no `X-Workspace-Id`); membership of the target; verified email when enforced.
- **Input & validation:** Path UUID (non-UUID rejected by the path extractor); nil UUID rejected.
- **Effects:** `UPDATE user_preferences SET default_workspace_id, updated_at`; best-effort user audit `workspace.switch` (`old_value {from_workspace_id: <previous stored default or null>}`, `new_value {to_workspace_id}`). No SSE (other devices of the same user are not told). Permissions in the response are loaded fresh from the membership row. `workspace` is null if the row vanished after the check.
- **Errors:** 400 `VALIDATION_ERROR` (nil id); 403 `PERMISSION_DENIED` "No access to workspace or workspace not found" (non-member or nonexistent); 404 (`user` / `user_preferences`) if those rows are missing; 500 `DATA_INTEGRITY_ERROR` if a stored permission string is unknown.
- **Source:** `src/handlers/workspaces.rs:149-194`; `src/services/workspace_service.rs:852-967`; `src/repositories/postgresql/workspace_repository.rs:873-900`; `src/repositories/postgresql/user_preferences_repository.rs:287-319`.
- **Tests:** `src/services/workspace_service.rs` (`switch_workspace_rejects_non_member`, `switch_workspace_rejects_nil_workspace_id`, `switch_workspace_emits_workspace_switch_audit`, `switch_workspace_succeeds_when_audit_logging_fails`).

### WS-016 Workspace-scoped request context from `X-Workspace-Id`

- **Behavior:** Every `/v1/workspace/*` route requires a valid session and an `X-Workspace-Id` header naming a workspace the caller has a membership row in. The resulting context carries that workspace id and the caller's stored permissions for it. The header is independent of the stored active workspace: any workspace the caller belongs to can be addressed on any request.
- **Kind:** `guarantee`
- **Trigger:** every request to `/v1/workspace`, `/v1/workspace/transfer`, `/v1/workspace/members*`, `/v1/workspace/leave`, `/v1/workspace/invitations*`, `/v1/workspace/audit-logs`, `/v1/workspace/files*`.
- **Auth:** Bearer session, then header, then membership (in that order); then rate limit, email verification (credentials users, when enforced), idempotency.
- **Input & validation:** Header must be present, ASCII, and a UUID.
- **Effects:** Reads via SECURITY DEFINER SPs `sp_public_validate_workspace_access` and `sp_public_get_workspace_permissions`; user row via `get_user_for_auth`. The context's `telemetry_consent` is always `false` (preferences are not loaded on this path).
- **Errors:** 401 `AUTHENTICATION_FAILED` (session problems, AUTH); 400 `VALIDATION_ERROR` "Missing X-Workspace-Id header" / "Invalid X-Workspace-Id header format" / "Invalid workspace ID format"; 403 `PERMISSION_DENIED` with `details {reason: "workspace_access_revoked", workspace_id}` when there is no membership (including a nonexistent workspace); 401 if the user row is gone; 500 `DATA_INTEGRITY_ERROR` on an unknown stored permission string.
- **Guarantees:** Membership is checked on every request; there is no cached workspace authorization.
- **Source:** `src/auth/middleware.rs:52-66,193-278`; `src/server.rs:646-668`; `src/handlers/mod.rs:343-408`; `src/repositories/postgresql/public_workspace_repository.rs:108-130,204-239`; `migrations/0005_stored_procedures.sql:716-747`; `src/errors/auth_error.rs:122-151,531-542`.
- **Tests:** `tests/http_smoke.rs`; `tests/public_workspace_repository_live.rs` (`validate_workspace_access_scopes_by_both_user_and_workspace`, `get_workspace_permissions_returns_membership_set_and_empty_for_non_member`); `tests/router_composition.rs` (layering).

### WS-017 Permissions are loaded per request and parsed fail-hard

- **Behavior:** The permission set used for authorization is read from `workspace_memberships.permissions` on every workspace-scoped request (and on switch, login, refresh, validate-session, verify-email, confirm-email-change), so a permission change takes effect on the caller's next request. Any stored string that is not a known `Permission` makes the request fail rather than silently dropping the grant.
- **Kind:** `guarantee`
- **Trigger:** workspace-scoped middleware; `load_permissions` in auth handlers; switch.
- **Auth:** n/a.
- **Errors:** 500 `DATA_INTEGRITY_ERROR` (generic message, details logged only) when parsing fails.
- **Source:** `src/models/permissions.rs:268-303,112-131`; `src/auth/middleware.rs:245-250`; `src/handlers/auth.rs:89-104`; `src/services/workspace_service.rs:879-884`; `src/errors/api_error.rs:734-739`.
- **Tests:** `src/models/permissions.rs` (`from_strings_rejects_unknown_with_all_invalid_listed`, `from_strings_error_maps_to_api_error_data_integrity`, `from_strings_empty_input_succeeds`).

### WS-018 List workspace members

- **Behavior:** Returns members of the current workspace, keyset-paginated on (membership `created_at`, id), newest first by default. Each item: `{id (membership id), workspace_id, user_id, user_email, user_name, avatar_file_id, permissions[], joined_at, updated_at, invited_by: null, is_owner}`. `is_owner` is derived (`user_id == workspaces.owner_id`).
- **Kind:** `capability`
- **Trigger:** `GET /v1/workspace/members`
- **Auth:** workspace-scoped; `read:member`.
- **Input & validation:** Pagination (limit 1-100 default 25; `sort_by` allowlist `["created_at"]`). Filters: `permissions` (repeatable, ≤50, each must be a known permission string; ANY-match via JSONB `?|`), `emails` (repeatable, ≤50, each normalized trim+lowercase, ≤254 chars, valid email; matched by HMAC blind index across all key versions), `joined_from`/`joined_to` (both or neither, `from < to`, span ≤ 365 days; inclusive start, exclusive end).
- **Errors:** 403 `PERMISSION_DENIED` "Missing required permission: read:member"; 400 `VALIDATION_ERROR` on filters.
- **Source:** `src/handlers/workspace_members.rs:43-58`; `src/models/workspace_member.rs:12-25,74-201`; `src/services/workspace_member_service.rs:236-257`; `src/repositories/postgresql/workspace_member_repository.rs:72-141,190-307`; `src/validation/mod.rs:79-164`.
- **Tests:** `src/services/workspace_member_service.rs` (`get_workspace_members_rejects_caller_without_read_member`); `tests/workspace_member_pagination_live.rs` (`get_workspace_members_keyset_pages_forward_and_backward`).

### WS-019 Update a member's permissions

- **Behavior:** Replaces a member's permission list with the requested list (empty allowed). Returns the updated member.
- **Kind:** `capability`
- **Trigger:** `PUT /v1/workspace/members/{member_id}/permissions`
- **Auth:** workspace-scoped; `update:member`, plus the subset rule (WS-022) on the requested list, plus WS-020 / WS-021.
- **Input & validation:** JSON `{ "permissions": [string] }`; each must be a known permission string; at most 6 entries (`PERMISSION_LIST_MAX` = number of permissions); duplicates allowed. Path `member_id` is a membership id; nil rejected.
- **Effects:** `UPDATE workspace_memberships SET permissions = <request strings as sent>, updated_at` (retried on transient errors). Best-effort workspace audit `member.update_permissions` (`resource_id` = member id, `old_value {permissions, user_id}`, `new_value {permissions, user_id}`). Best-effort workspace-scoped SSE `member.permissions_updated {workspace_id, workspace_name, member_id, user_id, user_name, old_permissions, new_permissions, updated_by, updated_by_session_id}`.
- **Errors:** 403 `PERMISSION_DENIED` (missing `update:member`, or privilege escalation with `details {reason: "privilege_escalation_prevented", missing_permissions}`); 400 `VALIDATION_ERROR` (list invalid / nil id / business rules in WS-020, WS-021); 404 (`workspace`, `workspace_member`).
- **Source:** `src/handlers/workspace_members.rs:83-95`; `src/models/workspace_member.rs:371-381`; `src/services/workspace_member_service.rs:271-464`; `src/repositories/postgresql/workspace_member_repository.rs:322-399,463-533`.
- **Tests:** `src/services/workspace_member_service.rs` (`update_member_permissions_rejects_caller_without_update_member`, `update_member_permissions_rejects_privilege_escalation`, `update_member_permissions_success_audits_and_fans_out`, fail-open variants); `tests/rls_isolation.rs` (`update_member_permissions_is_scoped_to_own_workspace_under_rls`).

### WS-020 Owner permission protection

- **Behavior:** A non-owner can never change the owner's permissions. The owner may target their own membership only with a list that contains every entry of `PERMISSIONS`; any omission is rejected and names the first missing permission.
- **Kind:** `rule`
- **Trigger:** `PUT /v1/workspace/members/{member_id}/permissions`
- **Auth:** workspace-scoped; `update:member`.
- **Errors:** 400 `VALIDATION_ERROR` "Cannot modify workspace owner permissions"; 400 `VALIDATION_ERROR` "Workspace owner must have all permissions. Missing: <perm>. Owner permissions cannot be reduced."
- **Guarantees:** Service-layer only; no DB constraint or RLS backstop.
- **Source:** `src/services/workspace_member_service.rs:326-354`.
- **Tests:** `src/services/workspace_member_service.rs` (`update_member_permissions_rejects_modifying_owner_as_non_owner`, `update_member_permissions_rejects_owner_reducing_own_permissions`).

### WS-021 Non-owners cannot change their own permissions

- **Behavior:** A non-owner member targeting their own membership is rejected regardless of the requested list.
- **Kind:** `rule`
- **Trigger:** `PUT /v1/workspace/members/{member_id}/permissions`
- **Auth:** workspace-scoped; `update:member`.
- **Errors:** 400 `VALIDATION_ERROR` "Cannot modify your own permissions".
- **Source:** `src/services/workspace_member_service.rs:355-364`.
- **Tests:** `src/services/workspace_member_service.rs` (`update_member_permissions_rejects_modifying_own_permissions`).

### WS-022 Delegation subset rule

- **Behavior:** When a request supplies a permission list to grant (invite, member permission update, invitation permission update), every requested permission must already be held by the caller in the `X-Workspace-Id` workspace. All missing permissions are reported, not just the first. The rule checks only the list being granted; it does not compare against the target's current permissions.
- **Kind:** `rule` (user-facing 403 with a missing list; also a security property).
- **Trigger:** `POST /v1/workspace/invitations`, `PUT /v1/workspace/members/{id}/permissions`, `PUT /v1/workspace/invitations/{id}` when `permissions` is present.
- **Auth:** runs right after the base permission gate.
- **Errors:** 403 `PERMISSION_DENIED`, message "Cannot grant permissions you do not hold: <comma-joined list>", `details {reason: "privilege_escalation_prevented", missing_permissions: [..]}` (request order preserved).
- **Source:** `src/models/auth.rs:96-148`; `src/errors/auth_error.rs:146-150,554-566`; `src/services/workspace_member_service.rs:281-284,891-894,1129-1135`.
- **Tests:** `src/models/auth.rs` (`require_can_grant_*`); `src/services/workspace_member_service.rs` (`invite_member_rejects_privilege_escalation`, `update_invitation_rejects_privilege_escalation`, `update_member_permissions_rejects_privilege_escalation`).

### WS-023 Remove a member

- **Behavior:** Deletes another member's membership row.
- **Kind:** `capability`
- **Trigger:** `DELETE /v1/workspace/members/{member_id}`
- **Auth:** workspace-scoped; `delete:member`; WS-024.
- **Input & validation:** membership id; nil rejected.
- **Effects:** `DELETE FROM workspace_memberships WHERE id` (idempotent, retried on transient errors). Best-effort workspace audit `member.remove` (`resource_id` = member id, `old_value {user_id, permissions}`). Best-effort SSE: workspace-scoped `member.removed {workspace_id, workspace_name, member_id, removed_user_id, removed_user_name, removed_by, removed_by_session_id}`, then user-scoped `connection.refresh_required {reason: "member_removed", workspace_id}` to the removed user. The removed user's `default_workspace_id` is not changed. The removed user's sessions are not touched.
- **Errors:** 403 `PERMISSION_DENIED` (missing `delete:member`); 400 `VALIDATION_ERROR` (nil id, WS-024); 404 (`workspace`, `workspace_member`).
- **Source:** `src/handlers/workspace_members.rs:120-131`; `src/services/workspace_member_service.rs:478-657`; `src/repositories/postgresql/workspace_member_repository.rs:414-448`.
- **Tests:** `src/services/workspace_member_service.rs` (`remove_member_rejects_caller_without_delete_member`, `remove_member_success_audits_and_fans_out`, fail-open variants).

### WS-024 The owner and the caller cannot be removed

- **Behavior:** Removing the owner's membership is rejected; removing your own membership via the remove endpoint is rejected (leave instead).
- **Kind:** `rule`
- **Trigger:** `DELETE /v1/workspace/members/{member_id}`
- **Auth:** workspace-scoped; `delete:member`.
- **Errors:** 400 `VALIDATION_ERROR` "Cannot remove workspace owner"; 400 `VALIDATION_ERROR` "Cannot remove yourself from workspace".
- **Guarantees:** Service-layer only.
- **Source:** `src/services/workspace_member_service.rs:511-533`.
- **Tests:** `src/services/workspace_member_service.rs` (`remove_member_rejects_removing_workspace_owner`, `remove_member_rejects_removing_self`).

### WS-025 Leave a workspace

- **Behavior:** Any non-owner member removes their own membership from the `X-Workspace-Id` workspace. The API does not auto-switch.
- **Kind:** `capability`
- **Trigger:** `POST /v1/workspace/leave`
- **Auth:** workspace-scoped; no permission; WS-026. Verified email required when enforced (route is in the workspace-scoped group).
- **Effects:** `DELETE FROM workspace_memberships` (not retried); best-effort workspace audit `member.leave` (`resource_id` = member id, no values); caller's default cleared (WS-013); best-effort SSE: workspace-scoped `member.left {workspace_id, workspace_name, member_id, user_id, user_name, left_by_session_id}`, then user-scoped `connection.refresh_required {reason: "workspace_left", workspace_id}` to the leaver.
- **Errors:** 404 (`workspace`, `workspace_member`); 400 per WS-026; errors from WS-013 after commit.
- **Source:** `src/handlers/workspace_members.rs:161-171`; `src/services/workspace_member_service.rs:59-97,681-822`.
- **Tests:** `src/services/workspace_member_service.rs` (`leave_workspace_success_clears_default_audits_and_fans_out`, `leave_workspace_succeeds_when_audit_logging_fails`, `leave_workspace_succeeds_when_sse_publish_fails`).

### WS-026 The owner cannot leave

- **Behavior:** The owner's leave request is rejected; they must transfer ownership or delete the workspace.
- **Kind:** `rule`
- **Trigger:** `POST /v1/workspace/leave`
- **Auth:** workspace-scoped.
- **Errors:** 400 `VALIDATION_ERROR` "Cannot leave workspace you own - transfer ownership or delete workspace first".
- **Source:** `src/services/workspace_member_service.rs:710-716`.
- **Tests:** `src/services/workspace_member_service.rs` (`leave_workspace_rejects_owner`).

### WS-027 List workspace invitations

- **Behavior:** Returns every invitation row of the current workspace, including expired ones not yet swept, with decrypted `email` and `workspace_name`, paginated in memory on (`invited_at`, id), newest first. Item: `{id, workspace_id, email, permissions[], invited_by, invited_at, updated_at, expires_at, workspace_name}`. There is no status field (existence means pending).
- **Kind:** `capability`
- **Trigger:** `GET /v1/workspace/invitations`
- **Auth:** workspace-scoped; `read:member` (not `invite:member`).
- **Input & validation:** Pagination as WS-018 (`created_at` maps to `invited_at`). `?email=` normalized (trim/collapse/NFC), 1-100 chars, not whitespace-only, case-insensitive substring match.
- **Errors:** 403 `PERMISSION_DENIED` "Missing required permission: read:member"; 400.
- **Source:** `src/handlers/workspace_members.rs:198-215`; `src/models/workspace_member.rs:33-46,296-334`; `src/services/workspace_member_service.rs:846-869`; `src/repositories/postgresql/workspace_member_repository.rs:768-885`.
- **Tests:** `src/services/workspace_member_service.rs` (`get_workspace_invitations_rejects_caller_without_read_member`).

### WS-028 Invite by email

- **Behavior:** Creates (or refreshes, WS-030) a pending invitation for an email address with a permission list, expiring `INVITATION_EXPIRY_DAYS` (default 7) days from now. The invitee does not need an account. Returns the invitation (without `workspace_name`).
- **Kind:** `capability`
- **Trigger:** `POST /v1/workspace/invitations`
- **Auth:** workspace-scoped; `invite:member`; subset rule (WS-022).
- **Input & validation:** JSON `{ "email": string, "permissions": [string] }`; email normalized (trim + lowercase), valid format, ≤254 chars; permissions as WS-019 (empty allowed, ≤6, known strings).
- **Effects:** `workspace_invitations` upsert (email encrypted, `email_hash` = HMAC with the current key, permissions stored canonical). Best-effort workspace audit `invitation.create` (`resource_id` = invitation id, `new_value {email, permissions, expires_at}`). Invitee lookup by email; best-effort email `workspace_invitation` to the address (every invitee; recipient name if they have an account; inviter = caller's name; workspace = current name; expiry days from config). Best-effort user-scoped SSE `invitation.received {invitation_id, workspace_id, workspace_name, invited_by_name, permissions}` only if the invitee has an account.
- **Errors:** 403 `PERMISSION_DENIED` (missing `invite:member`; escalation); 400 validation; 404 (`workspace`); 409 per WS-029.
- **Source:** `src/handlers/workspace_members.rs:239-250`; `src/models/workspace_member.rs:347-364`; `src/services/workspace_member_service.rs:882-1105`; `src/repositories/postgresql/workspace_member_repository.rs:1403-1473`; `src/config/environment.rs:176-187,401`.
- **Tests:** `src/services/workspace_member_service.rs` (`invite_member_rejects_caller_without_invite_member`, `invite_member_success_audits_and_fans_out`, `invite_member_skips_sse_when_invitee_has_no_account`, fail-open variants for audit, email, SSE).

### WS-029 Existing members cannot be invited

- **Behavior:** If the normalized email belongs to a user who already has a membership in this workspace (owner and caller included), the invitation is rejected and nothing is written.
- **Kind:** `rule`
- **Trigger:** `POST /v1/workspace/invitations`
- **Auth:** workspace-scoped; `invite:member`.
- **Errors:** 409 `RESOURCE_CONFLICT` "Resource already exists" (no details on the wire; constraint info and message logged only, email not included).
- **Source:** `src/services/workspace_member_service.rs:925-949`; `src/repositories/postgresql/workspace_member_repository.rs:637-675`.
- **Tests:** `src/services/workspace_member_service.rs` (`invite_member_rejects_existing_member`).

### WS-030 Re-inviting an email refreshes the pending invitation

- **Behavior:** There is at most one invitation per (workspace, email blind index). Inviting an email that already has a pending (or expired, unswept) invitation updates that row in place: permissions, `invited_by`, `invited_at`, `updated_at` and `expires_at` are replaced; the original invitation `id` is kept and returned. Audit, email and SSE run as for a new invitation.
- **Kind:** `rule`
- **Trigger:** `POST /v1/workspace/invitations`
- **Auth:** as WS-028.
- **Source:** `src/repositories/postgresql/workspace_member_repository.rs:1423-1434`; `migrations/0002_tables.sql:263-265`.
- **Tests:** `tests/workspace_invitation_repository_live.rs` (`create_invitation_executes_and_upserts_on_the_named_constraint`).

### WS-031 Update an invitation

- **Behavior:** Changes an invitation's permissions and/or expiry; an omitted field keeps its current DB value. An empty body is accepted (only `updated_at` changes) and still audits and publishes. Returns the invitation (without `workspace_name`). No email is sent.
- **Kind:** `capability`
- **Trigger:** `PUT /v1/workspace/invitations/{invitation_id}`
- **Auth:** workspace-scoped; `invite:member`; subset rule only when `permissions` is present.
- **Input & validation:** JSON `{ "permissions"?: [string], "expires_at"?: RFC 3339 }`; permissions as WS-019 (stored as sent, not canonicalized); `expires_at` must be strictly in the future (no upper bound). Nil id rejected.
- **Effects:** `UPDATE workspace_invitations SET permissions = COALESCE, expires_at = COALESCE, updated_at` (retried). Best-effort workspace audit `invitation.update` (`old_value`/`new_value` `{permissions, expires_at}`). Best-effort user-scoped SSE `invitation.updated {invitation_id, workspace_id, workspace_name, old_permissions, new_permissions}` to the invitee if they have an account.
- **Errors:** 403; 400; 404 (`invitation`).
- **Source:** `src/handlers/workspace_members.rs:275-287`; `src/models/workspace_member.rs:388-401`; `src/services/workspace_member_service.rs:1119-1276`; `src/repositories/postgresql/workspace_member_repository.rs:1488-1558`; `src/validation/mod.rs:195-204`.
- **Tests:** `src/services/workspace_member_service.rs` (`update_invitation_rejects_caller_without_invite_member`, `update_invitation_forwards_only_supplied_fields`, `update_invitation_fans_out_to_invitee`, fail-open variants).

### WS-032 Cancel an invitation

- **Behavior:** Deletes a pending invitation (no tombstone; the audit log is the history).
- **Kind:** `capability`
- **Trigger:** `DELETE /v1/workspace/invitations/{invitation_id}`
- **Auth:** workspace-scoped; `invite:member`.
- **Input & validation:** nil id rejected.
- **Effects:** `DELETE FROM workspace_invitations WHERE id` (idempotent, not retried). Best-effort workspace audit `invitation.cancel` (`old_value {email, permissions}`). Best-effort user-scoped SSE `invitation.cancelled {invitation_id, workspace_id, workspace_name}` to the invitee if they have an account (lookup failures swallowed). Other admins get no event.
- **Errors:** 403; 400; 404 (`invitation`).
- **Source:** `src/handlers/workspace_members.rs:313-324`; `src/services/workspace_member_service.rs:124-165,1290-1381`; `src/repositories/postgresql/workspace_member_repository.rs:1573-1607`.
- **Tests:** `src/services/workspace_member_service.rs` (`cancel_invitation_rejects_caller_without_invite_member`, `cancel_invitation_deletes_and_records_audit`, `cancel_invitation_succeeds_when_audit_logging_fails`, `cancel_invitation_succeeds_when_sse_publish_fails`).

### WS-033 Resend an invitation

- **Behavior:** Resets the invitation's expiry to now + `INVITATION_EXPIRY_DAYS` (works on expired, unswept invitations too), leaves permissions untouched, and re-sends the invitation email and `invitation.received` event. Returns the invitation (without `workspace_name`).
- **Kind:** `capability`
- **Trigger:** `POST /v1/workspace/invitations/{invitation_id}/resend`
- **Auth:** workspace-scoped; `invite:member` (no subset check against the invitation's permissions).
- **Input & validation:** nil id rejected.
- **Effects:** `UPDATE ... expires_at` (not retried). Best-effort workspace audit `invitation.resend` (`old_value`/`new_value` `{expires_at}`). Best-effort email (inviter name = the resender, workspace name = current `X-Workspace-Id` workspace). Best-effort user-scoped SSE `invitation.received {invitation_id, workspace_id, workspace_name, invited_by_name, permissions}` if the invitee has an account.
- **Errors:** 403; 400; 404 (`workspace`, `invitation`).
- **Source:** `src/handlers/workspace_members.rs:349-360`; `src/services/workspace_member_service.rs:1395-1583`.
- **Tests:** `src/services/workspace_member_service.rs` (`resend_invitation_rejects_caller_without_invite_member`, `resend_invitation_forwards_none_permissions`, `resend_invitation_fans_out_to_invitee`, fail-open variants for audit, email, SSE).

### WS-034 List my pending invitations

- **Behavior:** Returns non-expired invitations whose `email_hash` equals the caller's current `users.email_hash`, across all workspaces, with decrypted `workspace_name`, paginated in memory on (`invited_at`, id). Works with zero workspaces.
- **Kind:** `capability`
- **Trigger:** `GET /v1/users/me/invitations/pending`
- **Auth:** session (user-scoped); verified email when enforced. No permission.
- **Input & validation:** Pagination as WS-018. `?workspace_name=` normalized, 1-100 chars, not whitespace-only, case-insensitive substring; invitations with a NULL workspace name never match a filter.
- **Source:** `src/handlers/user_invitations.rs:42-59`; `src/models/workspace_member.rs:224-265`; `src/services/user_invitation_service.rs:84-104`; `src/repositories/postgresql/workspace_member_repository.rs:911-1033`.
- **Tests:** none found.

### WS-035 Accept an invitation

- **Behavior:** The invitee joins the workspace with the invitation's permissions and the invitation is consumed. Returns an empty success envelope. The API does not set the joined workspace as active.
- **Kind:** `capability`
- **Trigger:** `POST /v1/users/me/invitations/{invitation_id}/accept`
- **Auth:** session (user-scoped); verified email when enforced; guards in WS-036.
- **Effects:** In one user-scoped transaction (retried on transient errors): read the invitation, `sp_public_accept_workspace_invitation` (INSERT membership, see WS-038, then DELETE the invitation), user audit `invitation.accept` (`resource_id` = invitation id, `new_value {workspace_id, permissions}`). No workspace audit entry. Post-commit, best-effort: workspace-scoped `member.joined {workspace_id, workspace_name, member_id, user_id, user_name, user_email, joined_by_session_id}`, then user-scoped `connection.refresh_required {reason: "invitation_accepted", workspace_id}` to the joiner.
- **Errors:** 404 (`invitation`) when not visible; WS-036 errors.
- **Source:** `src/handlers/user_invitations.rs:84-96`; `src/services/user_invitation_service.rs:117-309`; `src/repositories/postgresql/workspace_member_repository.rs:1223-1301`; `src/repositories/postgresql/public_workspace_member_repository.rs:123-166`; `migrations/0005_stored_procedures.sql:801-822`.
- **Tests:** `tests/invitation_accept_fanout_live.rs` (`accept_invitation_fans_out_member_joined_and_refresh_and_audits`, `accept_invitation_succeeds_when_sse_publish_fails`); `tests/public_workspace_member_repository_live.rs` (`accept_creates_membership_and_consumes_only_that_invitation`).

### WS-036 Accept guards: expiry and email match

- **Behavior:** Accepting is rejected if `expires_at <= now`, or if the invitation's normalized email differs from the caller's normalized email (possible because RLS also shows invitations of workspaces the caller belongs to). These are the only expiry and email checks on the accept path; the SP and RLS check neither.
- **Kind:** `rule`
- **Trigger:** `POST /v1/users/me/invitations/{invitation_id}/accept`
- **Auth:** session.
- **Errors:** 400 `VALIDATION_ERROR` "Invitation has expired" with `details {reason: "invitation_expired", field: "expires_at"}`; 403 `PERMISSION_DENIED` "Email mismatch - invitation not for this user".
- **Source:** `src/services/user_invitation_service.rs:21-44,175-178`.
- **Tests:** `src/services/user_invitation_service.rs` (`check_invitation_acceptable_*`).

### WS-037 Accept is atomic with its audit record

- **Behavior:** If the in-transaction `invitation.accept` audit write fails, the membership insert and invitation delete are rolled back and the request fails.
- **Kind:** `guarantee`
- **Trigger:** `POST /v1/users/me/invitations/{invitation_id}/accept`
- **Auth:** session.
- **Source:** `src/services/user_invitation_service.rs:200-225`; `src/repositories/postgresql/transaction.rs` (`with_user_context` commits only on `Ok`).
- **Tests:** `tests/public_workspace_member_repository_live.rs` (`accept_rolls_back_membership_and_invitation_when_in_tx_audit_fails`).

### WS-038 Accepting while already a member overwrites that membership's permissions

- **Behavior:** If the caller already has a membership in the invitation's workspace, accepting does not fail: the existing row's `permissions` and `updated_at` are replaced with the invitation's permissions (membership id and `created_at` kept), and the invitation is deleted. This applies to the owner's membership too.
- **Kind:** `rule` (described in code as idempotency hardening, but its visible outcome is a permission change).
- **Trigger:** `POST /v1/users/me/invitations/{invitation_id}/accept`
- **Auth:** session.
- **Source:** `migrations/0005_stored_procedures.sql:812-817`.
- **Tests:** `tests/public_workspace_member_repository_live.rs` (`accept_is_idempotent_and_upserts_permissions_on_repeat`).

### WS-039 Decline an invitation

- **Behavior:** The invitee deletes an invitation addressed to them. Expired (unswept) invitations can be declined.
- **Kind:** `capability`
- **Trigger:** `POST /v1/users/me/invitations/{invitation_id}/decline`
- **Auth:** session (user-scoped); verified email when enforced; normalized email must match.
- **Input & validation:** nil id rejected.
- **Effects:** `DELETE FROM workspace_invitations` (idempotent, not retried). Best-effort user audit `invitation.decline` (`old_value {workspace_id, permissions}`). Best-effort workspace-scoped SSE `invitation.declined {workspace_id, workspace_name, invitation_id, declined_by_email, declined_by, declined_by_session_id}` to every member of that workspace. No workspace audit entry.
- **Errors:** 400 (nil id); 404 (`invitation`); 403 `PERMISSION_DENIED` "Email mismatch - invitation not for this user".
- **Source:** `src/handlers/user_invitations.rs:121-133`; `src/services/user_invitation_service.rs:322-439`; `src/repositories/postgresql/workspace_member_repository.rs:1061-1132,1160-1195`.
- **Tests:** `src/services/user_invitation_service.rs` (`decline_invitation_*`).

### WS-040 Invitation expiry semantics

- **Behavior:** `expires_at` is set at create/resend to now + `INVITATION_EXPIRY_DAYS` (1-90, default 7) and can be set to any future instant by update. An expired invitation is hidden from the invitee's pending list and cannot be accepted, but remains visible, updatable, resendable and cancellable by admins and declinable by the invitee until the background sweep deletes rows with `expires_at < NOW()` (no audit, no SSE, no email). The workspace RLS "invited" branch also ignores expired invitations.
- **Kind:** `rule`
- **Trigger:** invitation endpoints; background job `auth_cleanup` (scheduler owned by DATA) every `CLEANUP_INTERVAL_HOURS`.
- **Auth:** n/a.
- **Source:** `src/services/workspace_member_service.rs:965-966,1442-1444`; `src/repositories/postgresql/workspace_member_repository.rs:940`; `src/services/user_invitation_service.rs:28-34`; `migrations/0006_background_tasks.sql:76-97`; `migrations/0004_row_level_security.sql:177-188`; `src/server.rs:1574-1617`; `src/config/environment.rs:1330-1337`.
- **Tests:** `src/services/user_invitation_service.rs` (`check_invitation_acceptable_rejects_expired`); `src/config/environment.rs` (`config_rejects_invitation_expiry_days_zero`, `config_rejects_invitation_expiry_days_above_90`).

### WS-041 Permission schema discovery

- **Behavior:** Returns `{permissions: {<perm string>: {label, description, default_roles: [role]}}, roles: {<role>: {label, description}}, version: <CARGO_PKG_VERSION, "0.1.0">}` in the standard success envelope. Role presets are not sent as role→permission maps; clients derive them from `default_roles`.
- **Kind:** `capability`
- **Trigger:** `GET /v1/permissions/schema`
- **Auth:** public; exempt from rate limiting.
- **Source:** `src/handlers/permissions.rs:40-95`; `src/models/permissions.rs:345-409,448-470`; `src/handlers/mod.rs:55-58`; `src/middleware/rate_limit.rs:144-150`.
- **Tests:** `src/models/permissions.rs` (`permission_metadata_completeness`, `role_metadata_completeness`).

### WS-042 Permission list validation on writes

- **Behavior:** Every entry of a write-side permission list must parse as a known permission; the first unknown entry is rejected with a message listing all valid strings. Lists longer than 6 are rejected. Duplicates are accepted. Parsing wraps the input in quotes and runs JSON string deserialization, so JSON-escaped spellings of a valid string (for example `read:member`) are accepted.
- **Kind:** `rule`
- **Trigger:** `POST /v1/workspace/invitations`, `PUT /v1/workspace/members/{id}/permissions`, `PUT /v1/workspace/invitations/{id}`; the same validator gates the members `permissions` filter (≤50).
- **Auth:** as the endpoints.
- **Errors:** 400 `VALIDATION_ERROR` "Invalid permission: '<x>'. Valid permissions are: ..." / "Too many permissions (max matches the number of defined permissions)".
- **Source:** `src/validation/mod.rs:226-246`; `src/models/permissions.rs:173,268-270`; `src/models/workspace_member.rs:356-397`.
- **Tests:** `src/models/permissions.rs` (`permission_from_string`).

### WS-043 Membership-based tenant isolation in the database

- **Behavior:** The app connects as a non-owner role without BYPASSRLS. Workspaces are visible to their owner, members and (non-expired) invitees; memberships and workspace invitations are visible within any workspace the caller is a member/owner of; invitations are also visible to the addressed email; `users` rows are visible only for self and for members of the `app.current_workspace_id` workspace. Invitation INSERT/UPDATE must target a workspace the caller belongs to. Policies are membership-based, not permission-based.
- **Kind:** `guarantee`
- **Trigger:** every repository call under `with_user_context` / `with_workspace_context`.
- **Auth:** n/a.
- **Source:** `migrations/0004_row_level_security.sql:72-117,143-254`; `src/repositories/postgresql/transaction.rs` (`with_user_context`, `with_workspace_context`); `src/repositories/postgresql/rls_context.rs`.
- **Tests:** `tests/rls_isolation.rs` (`app_user_cannot_read_other_tenants_data`, `rls_cross_tenant_visibility_is_intentional_where_designed`, `app_user_cannot_write_other_tenants_data`, `workspace_invitations_write_check_rejects_own_email_in_foreign_workspace`, `app_user_cannot_delete_other_tenants_data`, `users_co_member_visible_under_workspace_context`, `workspace_context_repo_path_scopes_reads_to_the_authenticated_tenant`, `update_member_permissions_is_scoped_to_own_workspace_under_rls`, `update_member_permissions_in_tx_guards_and_scopes_foreign_writes`).

### WS-044 Side-effect failure policy

- **Behavior:** Three operations write their audit record inside the business transaction and fail if it fails: workspace create (`workspace.create`), transfer (`workspace.transfer` in both logs), accept (`invitation.accept`). Every other WS audit write, every SSE publish and every invitation email runs after commit and is fail-open (logged with `action = "failing_open"`, error types `audit_log_failed` / `sse_publish_failed` / `email_send_failed` / `email_template_render_failed`), and one failed publish does not stop later publishes in the same fan-out. When SSE (`SSE_NOTIFICATION_BACKEND=disabled`) or email is not configured, those steps are skipped.
- **Kind:** `guarantee`
- **Trigger:** all WS mutations.
- **Auth:** n/a.
- **Source:** `src/services/workspace_service.rs:285-309,618-680,778-806,931-959,1128-1180`; `src/services/workspace_member_service.rs:386-456,557-649,724-813,976-1098`; `src/services/user_invitation_service.rs:200-225,243-297,367-430`.
- **Tests:** the `*_succeeds_when_{audit_logging,sse_publish,email_send}_fails` tests in the three service files and in `tests/{workspace_fanout_live,invitation_accept_fanout_live}.rs`.

## Data owned

Tables (`migrations/0002_tables.sql`):
- `workspaces` (222-235): `id`, `name` (AES-256-GCM ciphertext), `owner_id` → `users` ON DELETE CASCADE, deferrable (644), `picture_file_id` → `workspace_files` ON DELETE SET NULL (681), `created_at`, `updated_at`. Index `idx_workspaces_owner`, partial index on `picture_file_id` (`0003_indexes.sql:67,126`).
- `workspace_memberships` (238-246): `id`, `user_id` → `users` CASCADE, `workspace_id` → `workspaces` CASCADE (both deferrable, 649-650), `permissions JSONB NOT NULL` (array of permission strings; no role column, no owner flag), timestamps, `UNIQUE(user_id, workspace_id)`. Indexes on user, workspace, (workspace, created_at DESC) (`0003_indexes.sql:70-73`).
- `workspace_invitations` (253-266): `id`, `workspace_id` → CASCADE (651), `email` (ciphertext), `email_hash` (HMAC blind index), `permissions JSONB`, `invited_by` → `users` CASCADE (652), `invited_at`, `updated_at`, `expires_at`, `UNIQUE(workspace_id, email_hash)`; no status column. Indexes on workspace, email_hash, expires_at, (workspace, invited_at DESC) (`0003_indexes.sql:76-80`).
- `user_preferences.default_workspace_id` (269-278): the stored active workspace, FK `ON DELETE SET NULL`. The table itself is shared with USER (telemetry consent).

RLS (`migrations/0004_row_level_security.sql`): context setters `set_request_context` (72-87), `set_user_request_context` (104-117, clears workspace GUC); helpers `get_current_user_email_hash` (128-141), `get_current_user_workspace_ids` (144-160, memberships ∪ owned), `get_current_workspace_user_ids` (163-175), `get_current_user_invited_workspace_ids` (178-188); policies `users_workspace_access` (198-204), `workspaces_access_control` (207-215), `workspace_memberships_access` (233-235), `workspace_invitations_access` (245-254). All are `FOR ALL` with no separate write policies except the invitation `WITH CHECK`.

Stored procedures (`migrations/0005_stored_procedures.sql`, all SECURITY DEFINER, granted to `app_user` in `0008_grants.sql:56-92,141`): `sp_public_create_workspace` (329-353, unused by production code), `sp_public_create_owner_membership` (359-372), `sp_public_update_user_preferences` (393-404), `sp_public_clear_user_default_workspace` (410-425), `sp_public_validate_workspace_access` (716-730), `sp_public_get_workspace_permissions` (733-747, returns `[]` for non-members), `sp_public_get_workspace_by_id` (750-767), `sp_public_get_user_accessible_workspaces` (770-789, ordered by workspace `created_at ASC`), `sp_public_accept_workspace_invitation` (801-822), `sp_public_find_authored_invitations_in_unowned_workspaces` (850-868, used by USER), plus key-rotation SPs for workspaces and invitations (1918-1978, owned by DATA).

Background task: `sp_cleanup_expired_invitations` (`migrations/0006_background_tasks.sql:76-97`), invoked from the `auth_cleanup` job (`src/server.rs:1574-1617`) via `PublicAuthService::cleanup_expired_invitations` (`src/services/public_auth_service.rs:1616-1620`).

## Configuration

- `INVITATION_EXPIRY_DAYS`: default 7; must be 1-90 or startup fails (`src/config/environment.rs:176-187,401,1330-1337`). Used for create/resend expiry and the email text. The startup log of the cleanup task hardcodes `invitation_ttl_days = 7` regardless (`src/server.rs:1584`).
- `STORAGE_MAX_UPLOAD_BYTES`: default 10,485,760; body limit for `PUT /v1/workspace` (`src/config/environment.rs:504-507`, `src/handlers/mod.rs:347-350`). Picture uploads need object storage configured (FILE/OPS).
- `REQUIRE_EMAIL_VERIFICATION`: default false; when true, unverified credentials users are blocked from every WS route (user-scoped and workspace-scoped groups) (`src/config/environment.rs:364-367`, `src/auth/middleware.rs:85-123`).
- `SSE_NOTIFICATION_BACKEND` and email settings: disable SSE / email side effects when off (owned by RT / MAIL).
- `CLEANUP_INTERVAL_HOURS`, `ENABLE_BACKGROUND_CLEANUP`: schedule of the expired-invitation sweep (owned by DATA/OPS).

## Cross-domain dependencies

Calls out of WS:
- AUTH: `PublicAuthService::{validate_access_token, get_user_for_auth, validate_workspace_access, get_user_workspace_permissions, get_workspace_for_auth}` from the workspace-scoped middleware and switch (`src/auth/middleware.rs:141-248`, `src/services/workspace_service.rs:879-882`, `src/handlers/workspaces.rs:166-169`).
- AUDIT: `WorkspaceAuditLogService::{log_event, log_event_in_tx}` (requires `input.workspace_id == ctx.workspace_id`, `src/services/workspace_audit_log_service.rs:40-57`) and `UserAuditLogService` for `workspace.create`, `workspace.switch`, `workspace.transfer`, `invitation.accept`, `invitation.decline`.
- RT: `SseEventPublisher::{publish_workspace_event, publish_user_event}` (events listed per behavior). Workspace-scoped events are delivered to every SSE connection whose cached workspace set (all memberships at connect time) contains the workspace (`src/handlers/sse_notifications.rs:112-131`, `src/services/sse_notification_service.rs:182-255`); hence the `connection.refresh_required` events after membership changes.
- MAIL: `EmailTemplateService::render_workspace_invitation` + `EmailService::send_email`.
- FILE: object store `put`/`delete` and `workspace_files` rows for the picture; the delete trigger and reaper (DATA) remove bytes.
- HTTP: `ValidatedJson`, `ValidatedQuery`, `Path`, `UpdateWorkspaceMultipart`, `PaginationQuery`, `retry_database_operation` (3 attempts on 40001/40P01/connection errors, `src/utils/database_retry.rs`), rate limiting and idempotency (idempotency keys on the `X-Workspace-Id` header, not the stored default, `src/middleware/idempotency.rs:821-848`).

Calls into WS:
- AUTH login/refresh/validate-session/verify-email/confirm-email-change read the stored active workspace and permissions (see the active-workspace model).
- USER `delete_user` reuses `publish_workspace_deletion_fanout`, `publish_member_left_fanout`, `publish_invitation_cancelled_fanout` and the repository methods `find_owned_by_user`, `find_member_only_workspaces_with_membership_id`, `find_authored_invitations_in_unowned_workspaces` to emit one `workspace.delete` / `member.leave` / `invitation.cancel` audit and matching SSE per cascaded entity (`src/services/user_service.rs:686-727,806-905,962-1000`). USER profile update fans out `member.profile_updated` to every workspace of the user (`src/services/user_service.rs:504-534`).
- FILE: workspace file routes rely on the same workspace-scoped context (WS-016) and gate only on membership; `DELETE /v1/workspace/files/{id}` can delete the picture file (see Observations).
- AUDIT: `GET /v1/workspace/audit-logs` is gated by `read:workspace_audit_log` from this domain's permission set (`src/services/workspace_audit_log_service.rs:212`).

## Current permission model

### Permissions

The `Permission` enum is the source of truth; its serde rename is both the wire form and the stored JSONB string (`src/models/permissions.rs:79-93`). `PERMISSIONS` lists all six in declaration order (`src/models/permissions.rs:144-151`).

| String | Variant | Label | Description | Default roles | Gates (production code) |
|---|---|---|---|---|---|
| `read:member` | `ReadMember` | View Members | View workspace members and pending invitations | admin, member | `GET /workspace/members`, `GET /workspace/invitations` |
| `invite:member` | `InviteMember` | Invite Members | Invite new members with permissions to workspace | admin | `POST /workspace/invitations`, `PUT/DELETE /workspace/invitations/{id}`, `POST /workspace/invitations/{id}/resend` |
| `update:member` | `UpdateMember` | Update Members | Update member permissions | admin | `PUT /workspace/members/{id}/permissions` |
| `delete:member` | `DeleteMember` | Remove Members | Remove members from workspace | admin | `DELETE /workspace/members/{id}` |
| `update:workspace` | `UpdateWorkspace` | Update Workspace | Update workspace configuration | admin | `PUT /workspace` |
| `read:workspace_audit_log` | `ReadWorkspaceAuditLog` | View Workspace Audit Logs | View workspace audit logs (member management, workspace configuration) | admin, member | `GET /workspace/audit-logs` |

Source of labels/roles: `src/models/permissions.rs:345-392`. Every permission is workspace-scoped; there are no user-level or global permissions, and user-scoped request contexts always carry an empty permission list (`src/auth/middleware.rs:129-185`).

### Roles

`Role` has two variants, `admin` and `member` (`src/models/permissions.rs:199-210,425-471`). A role is only a label plus description delivered by `/permissions/schema`; membership of a permission in a role is declared per permission via `default_roles`. The server never accepts, stores or returns a role per member, and no production code path reads `Role` except the schema handler. Admin = all six permissions; Member = `read:member` + `read:workspace_audit_log`. These preset relationships ("Admin equals `PERMISSIONS`", "Member is exactly the two read permissions", "every role has at least one permission") are asserted only by tests (`src/models/permissions.rs:789-837`). "Custom" is a client-side UI state.

### Ownership (structural authority, separate from permissions)

`workspaces.owner_id` is the only owner record; `is_owner` is derived at read time (`src/repositories/postgresql/workspace_member_repository.rs:139`). Owner-only actions, checked in the service against `owner_id`, not against permissions: delete workspace (`src/services/workspace_service.rs:731-735`) and transfer ownership (`src/services/workspace_service.rs:1018-1022`). Owner protections: cannot be removed (`src/services/workspace_member_service.rs:520-525`), cannot leave (`:710-716`), non-owners cannot change the owner's permissions, and the owner can only set their own list to a superset of `PERMISSIONS` (`:326-354`). The owner's operational authority (members, invitations, settings) comes from the permissions stored on their membership row, not from ownership: creation seeds the full set (`src/services/workspace_service.rs:270-283`) and transfer grants it to the new owner (`:1101-1112`).

### Storage

- Members: `workspace_memberships.permissions` JSONB array of strings (`migrations/0002_tables.sql:242`). No schema constraint on contents.
- Invitations: `workspace_invitations.permissions` JSONB array, copied to the membership on accept (`src/services/user_invitation_service.rs:187-198`).
- Writers and canonical form: create workspace, transfer, invite and accept write canonical strings produced from the enum (`src/services/workspace_service.rs:272,1103`; `src/repositories/postgresql/workspace_member_repository.rs:1412`; `src/services/user_invitation_service.rs:187-188`). Member permission update and invitation update write the request strings exactly as sent, including duplicates and JSON-escape spellings accepted by the validator (`src/services/workspace_member_service.rs:374-380`; `src/services/workspace_member_service.rs:1180-1190`; `src/repositories/postgresql/workspace_member_repository.rs:328-344,1501-1521`).
- No backfill exists: adding a variant to `PERMISSIONS` does not change stored rows, including owners'.

### Loading

- Workspace-scoped requests: `sp_public_get_workspace_permissions(user, workspace)` → JSON → `Permission::from_strings` (fail-hard) → `AuthContext.permissions` (`src/auth/middleware.rs:245-250`; `migrations/0005_stored_procedures.sql:733-747`). Loaded on every request; no cache.
- Session responses (login, refresh, validate-session, verify-email, confirm-email-change): `load_permissions` for the resolved active workspace, or `[]` when none (`src/handlers/auth.rs:89-104`).
- Switch: loaded for the target workspace (`src/services/workspace_service.rs:879-884`).
- Member and invitation reads parse stored strings fail-hard per row (`src/repositories/postgresql/workspace_member_repository.rs:124-126,823-825`).

### Enforcement point per endpoint

All checks are in the service layer, after the workspace-scoped middleware has established membership. RLS (WS-043) only enforces membership.

| Endpoint | Gate | Where |
|---|---|---|
| `GET /permissions/schema` | none (public) | `src/handlers/permissions.rs:81` |
| `GET /users/me/workspaces` | session | `src/services/workspace_service.rs:413-425` |
| `POST /users/me/workspaces` | session | `src/services/workspace_service.rs:203-207` |
| `POST /users/me/workspaces/{id}/switch` | session + membership of target | `src/services/workspace_service.rs:866-875` |
| `GET /users/me/invitations/pending` | session (email-hash filter) | `src/repositories/postgresql/workspace_member_repository.rs:940-941` |
| `POST /users/me/invitations/{id}/accept` | session + not expired + email match | `src/services/user_invitation_service.rs:24-44` |
| `POST /users/me/invitations/{id}/decline` | session + email match | `src/services/user_invitation_service.rs:349-357` |
| `GET /workspace` | membership | `src/services/workspace_service.rs:385-386` |
| `PUT /workspace` | `update:workspace` | `src/services/workspace_service.rs:450` |
| `DELETE /workspace` | owner | `src/services/workspace_service.rs:731-735` |
| `POST /workspace/transfer` | owner | `src/services/workspace_service.rs:1018-1022` |
| `GET /workspace/members` | `read:member` | `src/services/workspace_member_service.rs:243` |
| `PUT /workspace/members/{id}/permissions` | `update:member` + subset + owner/self rules | `src/services/workspace_member_service.rs:279-284,326-364` |
| `DELETE /workspace/members/{id}` | `delete:member` + not owner + not self | `src/services/workspace_member_service.rs:480,520-533` |
| `POST /workspace/leave` | membership + not owner | `src/services/workspace_member_service.rs:690,710-716` |
| `GET /workspace/invitations` | `read:member` | `src/services/workspace_member_service.rs:855` |
| `POST /workspace/invitations` | `invite:member` + subset + not already a member | `src/services/workspace_member_service.rs:889-894,929-949` |
| `PUT /workspace/invitations/{id}` | `invite:member` + subset if permissions given | `src/services/workspace_member_service.rs:1127-1135` |
| `DELETE /workspace/invitations/{id}` | `invite:member` | `src/services/workspace_member_service.rs:1296` |
| `POST /workspace/invitations/{id}/resend` | `invite:member` | `src/services/workspace_member_service.rs:1401` |
| `GET /workspace/audit-logs` | `read:workspace_audit_log` | `src/services/workspace_audit_log_service.rs:212` |
| `POST/GET /workspace/files`, `DELETE /workspace/files/{id}` | membership only | `src/services/workspace_file_service.rs:62,196,219` |

### Invariants and where they hold

- Owner holds every permission in `PERMISSIONS`: established at create and transfer; protected only by `update_member_permissions` (service). Not protected against `sp_public_accept_workspace_invitation`'s upsert (WS-038). Not maintained when `PERMISSIONS` grows.
- Subset rule on grants: service, three call sites (WS-022). Not applied to demotion, removal, cancel or resend.
- Non-owner cannot change own permissions: service (WS-021).
- Owner cannot be removed and cannot leave: service (WS-024, WS-026). Therefore the owner's membership row persists for the workspace's lifetime (except via account deletion, which deletes the workspace).
- There is no "last admin" rule: any number of non-owner members holding admin-like permissions may be demoted or removed; the owner is the only protected full-permission holder.
- One membership per (user, workspace): `UNIQUE` constraint.
- Delete/transfer are owner-only: service only; the `workspaces` RLS policy admits UPDATE/DELETE by any member.

### What a client sees

- Session payloads (`AuthResponse` and `SessionContextResponse`): `permissions: [string]` for the active workspace (stored order, may contain duplicates or non-canonical spellings written by WS-019/WS-031) and `workspace.is_owner` (`src/handlers/auth.rs:26-84`).
- Member list items: `permissions` (parsed to canonical strings by serde) and `is_owner` per member (WS-018). Invitations carry `permissions` (WS-027, WS-034).
- `/permissions/schema` for labels, descriptions and role presets (WS-041).
- Errors: 403 `PERMISSION_DENIED` with message "Missing required permission: <perm>" and no details; 403 with `details.reason = "privilege_escalation_prevented"` and `missing_permissions`; 403 with `details.reason = "workspace_access_revoked"`; business-rule rejections are 400 `VALIDATION_ERROR` (`src/errors/auth_error.rs:122-151`).
- SSE: `member.permissions_updated` (old/new lists) to every member of the workspace; it is also the only signal a user gets that their own permissions changed (clients match on `user_id`). `invitation.received` / `invitation.updated` carry permissions to the invitee.
- There is no endpoint returning "my permissions in workspace X" other than switch or a session response for the active workspace; `GET /users/me/workspaces` carries no permissions.

### Weaknesses observed (factual)

1. Member and invitation mutations address rows by id only. `find_member_by_id`, `update_member_permissions`, `remove_member`, `find_invitation_by_id`, `update_invitation` and `delete_invitation` have no `workspace_id = ctx.workspace_id` predicate (`src/repositories/postgresql/workspace_member_repository.rs:341-348,431,473-490,1326-1341,1517-1524,1590`), and the RLS policies they run under admit rows of every workspace the caller belongs to (`migrations/0004_row_level_security.sql:233-254`). The permission check uses the caller's permissions in the `X-Workspace-Id` workspace. For member rows the `users` join (visible only for members of the header workspace, `migrations/0004_row_level_security.sql:198-204`) narrows this to targets who are also members of the header workspace; invitation rows have no such narrowing. Audit entries and SSE events are then emitted under the header workspace.
2. RLS is membership-based for `workspaces`, `workspace_memberships` and `workspace_invitations` (`FOR ALL`), so the database does not stop any member from updating or deleting these rows; every capability and owner rule lives only in service code.
3. The comment that the owner "can only ADD permissions to themselves" so as to pick up new permissions (`src/services/workspace_member_service.rs:327-329`) is not reachable for a permission the owner lacks: `require_can_grant` runs first (`:283-284`) and rejects it.
4. The subset rule constrains only the granted list: a holder of `update:member` alone can set any non-owner's list to `[]` or `[update:member]`, removing permissions the caller does not hold; a holder of `delete:member` alone can remove a more-privileged non-owner; a holder of `invite:member` can resend (re-arm) or cancel invitations granting permissions they do not hold.
5. Accepting an invitation while already a member overwrites that membership's permissions, owner included (WS-038). `member_exists_by_email` blocks this only at invite time and only against the current email hash (`src/services/workspace_member_service.rs:929-949`).
6. Permission strings are not normalized on two write paths (see Storage), so stored arrays can hold duplicates or escape spellings; the members `?|` filter compares raw strings (`src/repositories/postgresql/workspace_member_repository.rs:77-81`).
7. SSE workspace events carry member PII and permission lists to every member regardless of `read:member` (`member.joined` with `user_email`, `member.permissions_updated`, `member.removed`, `invitation.declined` with `declined_by_email`).
8. Role presets exist only as metadata plus test assertions; production code does not prevent a preset from drifting.

## Current active-workspace model

### Where it is stored

A single nullable column, `user_preferences.default_workspace_id` (`migrations/0002_tables.sql:269-278`), FK to `workspaces` `ON DELETE SET NULL`. It is per user, not per session or device: all of a user's sessions share it. Nothing about the active workspace is stored in `sessions` or in tokens.

### What sets or changes it

| Writer | Effect | Source |
|---|---|---|
| Registration | inserts NULL | `src/services/public_auth_service.rs:531-541,638-648` |
| `POST /users/me/workspaces/{id}/switch` | sets to target (after membership check) | `src/services/workspace_service.rs:898-913` |
| Login (credentials or OAuth existing user) | if stored default is set but the user is no longer a member: repoint to the user's member workspace with the oldest workspace `created_at`, or set NULL if none; if stored default is NULL: no change and no workspace | `src/services/public_auth_service.rs:702-749`; `migrations/0005_stored_procedures.sql:770-789` |
| `DELETE /workspace` | sets caller's to NULL (unconditional) | `src/services/workspace_service.rs:811-814` |
| `POST /workspace/leave` | sets caller's to NULL (unconditional) | `src/services/workspace_member_service.rs:758-761` |
| Workspace row deletion (direct or via owner account deletion) | FK sets NULL for every user whose default it was | `migrations/0002_tables.sql:271` |
| Not changed by | create workspace, accept invitation, transfer, member removal by an admin, refresh, validate-session, verify-email, confirm-email-change | `src/handlers/auth.rs:175-199` (resolution without write) |

### What depends on it

- User-scoped middleware copies it, unvalidated, into `AuthContext.workspace_id` for every `/users/me/*`, `/auth/*` (authenticated) and telemetry request (`src/auth/middleware.rs:157-184`). In WS it is read only by switch, as `from_workspace_id` in the `workspace.switch` audit.
- Session responses: `POST /auth/validate-session`, `POST /auth/verify-email`, `POST /users/me/email/confirm` resolve it from the context, `POST /auth/refresh` from a fresh preferences read; each re-validates membership via `sp_public_validate_workspace_access` and returns `workspace: null` and `permissions: []` when it is NULL or membership was lost, without clearing it (`src/handlers/auth.rs:175-199,535-573,609-622,743-774,1146-1172`). Login is the only path that repairs it.
- `workspace.is_owner` and `permissions` in session payloads describe this workspace only.

### What does not depend on it

- Every `/v1/workspace/*` request uses the `X-Workspace-Id` header, validated per request against membership (WS-016). A request may name any workspace the user belongs to, active or not, and gets that workspace's permissions. The stored default is never consulted on these routes.
- Idempotency keys use the header, not the stored default (`src/middleware/idempotency.rs:821-848`).
- `GET /users/me/workspaces` lists all memberships (WS-004).
- `GET /users/me/invitations/pending` and accept/decline are user-scoped and workspace-independent (WS-034, WS-035, WS-039).
- SSE: at connect time the stream caches the ids of all the user's member workspaces (`src/handlers/sse_notifications.rs:112-131`) and delivers workspace-scoped events for all of them, plus the user's own user-scoped events (`src/services/sse_notification_service.rs:182-255`). Membership changes trigger `connection.refresh_required` so the client reconnects with a fresh set (WS-002, WS-011, WS-023, WS-025, WS-035). Switching emits no SSE and does not affect the stream.

### What a member can and cannot see of a non-active workspace

- Can: see it in the workspace list (id, name, owner_id, picture_file_id, timestamps); receive all of its SSE events; call any workspace-scoped endpoint for it with `X-Workspace-Id`, with full effect according to their permissions there (read, update, members, invitations, audit logs, files, leave, delete/transfer if owner); download its files via `/files/workspace/{id}` (FILE, membership via RLS).
- Cannot, without a workspace-scoped call or a switch: learn their own permissions or `is_owner` flag there (the list carries `owner_id` only).
- An invitee who is not a member can see the invited workspace's name through the pending-invitation list (and RLS admits the `workspaces` row while the invitation is unexpired), but cannot call any workspace-scoped endpoint for it.
- A member removed by an admin keeps the workspace as their stored default; validate-session and refresh then return `workspace: null`, and workspace-scoped calls return 403 `workspace_access_revoked` until they switch or log in again.

### End-to-end lifecycle

1. Register → default NULL, zero workspaces (WS-001).
2. Create a workspace → still NULL (WS-003); client must call switch to make it active.
3. Accept an invitation → still unchanged (WS-035); client decides whether to switch.
4. Switch → default set; response gives the workspace context.
5. Other devices of the same user see the new default on their next validate-session/refresh/login; there is no push.
6. Leave or delete (from any workspace, not necessarily the active one) → caller's default becomes NULL (WS-013); login does not fall back from NULL, so the user is in the zero-workspace state until they switch, even if they belong to other workspaces.

## Observations

1. **Cross-workspace targeting by id.** See permission-model weakness 1. Concretely, a caller with `delete:member` in workspace A who is any member of workspace B can remove B's membership of a user who belongs to both A and B by sending that membership id with `X-Workspace-Id: A` (`src/services/workspace_member_service.rs:513-555`); likewise update its permissions to any subset of their A permissions. A caller with `invite:member` in A can update, resend or cancel any invitation of B (`:1155-1160,1316-1328,1433-1454`); a resend emails the invitee with workspace A's name (`:1425-1430,1515-1521`). The audit and SSE entries are recorded under A. `tests/rls_isolation.rs:1182-1260` describes the update path as "scoped to own workspace" but only exercises a workspace the caller is not a member of. Matters because the new spec must decide the scoping explicitly.
2. **Owner self-update comment vs code.** `src/services/workspace_member_service.rs:327-329` says owners can add new permissions to themselves; `require_can_grant` at `:283-284` makes that impossible for a permission they lack. No migration grants new permissions to existing owners. Matters when a seventh permission is added.
3. **Accept SP overwrites existing memberships, contradicting its documented safety.** `src/repositories/traits.rs` (doc of `update_member_permissions`, "Construction-time paths ... invariant-safe by design") vs `migrations/0005_stored_procedures.sql:812-817`. Reachable when a member, or the owner, accepts an invitation addressed to their current email (for example after an email change to an address that already had a pending invitation in a workspace they belong to).
4. **Unconditional default clearing.** `src/services/workspace_service.rs:808-810` and `src/services/workspace_member_service.rs:755-757` state "current workspace IS the default workspace by design", but the header-driven model does not enforce it, so deleting or leaving a non-active workspace also clears the active one. The clear is not fail-open: a failure returns an error after the delete/leave committed and skips the SSE fan-out (`src/services/workspace_service.rs:811-814`, `src/services/workspace_member_service.rs:758-761`).
5. **NULL default has no login fallback.** `src/services/public_auth_service.rs:746-748` returns no workspace when the stored default is NULL even if memberships exist; the fallback runs only when a non-NULL default lost access. Combined with WS-003, WS-013 and WS-035, users routinely land in the zero-workspace state while having workspaces.
6. **Asymmetric invitation visibility and expiry.** Admins list expired invitations (`src/repositories/postgresql/workspace_member_repository.rs:784-799`, no expiry filter); the invitee list filters them (`:940`). `PUT /workspace/invitations/{id}` accepts any future `expires_at` with no 90-day cap (`src/models/workspace_member.rs:399-400`), unlike create/resend. Decline works on expired invitations. Expired rows are swept silently.
7. **Non-canonical permission storage.** `update_member_permissions` and `update_invitation` persist raw request strings (`src/services/workspace_member_service.rs:374,1180`); `Permission::from_string` accepts JSON-escaped spellings (`src/models/permissions.rs:268-270`). `PERMISSION_LIST_MAX` explicitly tolerates duplicates (`src/models/permissions.rs:167-172`).
8. **PII and permission lists in workspace-wide SSE.** `member.joined` includes `user_email` (`src/services/user_invitation_service.rs:253-261`); `invitation.declined` includes `declined_by_email` (`:411-419`); both reach members without `read:member`, which otherwise gates roster and invitee emails (`src/services/workspace_member_service.rs:852-855`).
9. **Audit gaps.** A member joining (accept) and declining are recorded only in the user's audit log, not the workspace's (`src/services/user_invitation_service.rs:207-225,374-399`). Workspace creation is recorded only in the user log (`src/services/workspace_service.rs:292-309`). The `workspace.delete` entry is written after the workspace is gone and is readable by nobody, because the select policy requires membership (`migrations/0004_row_level_security.sql:281-283`). Expiry sweeps are not audited. The transfer audit stores the new owner's email (PII) in both logs (`src/services/workspace_service.rs:1150-1176`).
10. **Picture deletable without `update:workspace`.** The picture is an ordinary public `workspace_files` row; `DELETE /v1/workspace/files/{id}` gates only on membership (`src/services/workspace_file_service.rs:219`), and the FK nulls `picture_file_id` (`migrations/0002_tables.sql:681`) with no `workspace.update` audit or `workspace.updated` SSE. FILE owns that route.
11. **Dead or unreachable code.** `PublicWorkspaceRepository::create_default_workspace_in_tx` and `sp_public_create_workspace` are unused by production code (`src/repositories/postgresql/public_workspace_repository.rs:53-88`; traits doc says "Used for default workspace creation during OAuth", which no longer happens). `Repository<Workspace>::create` and `delete_in_tx` are unused (`src/repositories/postgresql/workspace_repository.rs:71-113,1060-1096`). `switch_workspace` loads the user and discards it (`src/services/workspace_service.rs:891-896`). The other-user check in `list_workspaces_by_user` (`:420-425`), the nil-workspace checks (`:378-383,460-465`) and the `ctx.workspace_id == None` branches in workspace-scoped services are unreachable from HTTP. `WorkspaceMember.invited_by` is always `null` (no such column; `src/repositories/postgresql/workspace_member_repository.rs:138`). `Role` is used only by the schema endpoint.
12. **Response-shape inconsistencies.** Create, update and resend invitation responses omit `workspace_name` (repository returns `NULL::TEXT`, `src/repositories/postgresql/workspace_member_repository.rs:1434,1524`), while list endpoints include it. Create returns the original id on upsert, not the id the service generated (`src/services/workspace_member_service.rs:957-974`).
13. **Inconsistent retry hardening.** Remove, update-permissions, update-invitation, delete-workspace, create, transfer and accept are wrapped in `retry_database_operation`; leave, cancel, decline and resend are not (`src/services/workspace_member_service.rs:719-722,1325-1328,1450-1454`; `src/services/user_invitation_service.rs:362-365`).
14. **Accept existence leak.** Through the membership branch of the invitation RLS policy, a member can load a co-worker's invitation by id on the accept path and gets 403 "Email mismatch" instead of 404 (`src/services/user_invitation_service.rs:35-42`). Accept has no nil-id guard; decline does (`:330-335`).
15. **In-memory pagination precision.** Workspace and invitation lists sort by full-precision timestamps but filter by millisecond cursor keys (`src/repositories/postgresql/workspace_repository.rs:705-724`, `src/utils/pagination.rs:734-760`); items sharing a millisecond may be skipped or repeated across pages. Pagination is owned by HTTP.
16. **Doc and log mismatches.** `src/handlers/permissions.rs:26-30` shows `read:member` description "View workspace members"; actual is "View workspace members and pending invitations" (`src/models/permissions.rs:362`). `src/repositories/traits.rs` cites index `idx_workspace_memberships_workspace_joined`; the real one is `idx_workspace_memberships_workspace_created` (`migrations/0003_indexes.sql:73`). `src/server.rs:1584` logs `invitation_ttl_days = 7` regardless of `INVITATION_EXPIRY_DAYS`.
17. **Tests asserting more than production enforces.** Role-preset invariants (Admin = `PERMISSIONS`, Member = two reads, non-empty presets) exist only in tests (`src/models/permissions.rs:789-837`). `tests/login_workspace_resolution_live.rs:18-21` states a user cannot revoke their own membership via a request; `POST /workspace/leave` does, though leave also clears the default so the tested fallback branch is not hit that way.
18. **Transfer leaves the former owner with full permissions** and emits no `connection.refresh_required`; clients learn of the change via `workspace.transferred` only (`src/services/workspace_service.rs:1114-1126,1198-1221`).
19. **Removal does not touch sessions or stored defaults.** A removed member keeps a stale `default_workspace_id` and live sessions; access is denied lazily per request (`src/services/workspace_member_service.rs:478-657`).

## Open questions

1. Should member and invitation mutations be confined to the `X-Workspace-Id` workspace (Observation 1), or is cross-workspace targeting by id acceptable?
2. Should accepting an invitation while already a member be rejected, be a no-op, or keep overwriting permissions (including the owner's)?
3. Should the active workspace be per user (shared across devices, as now) or per session/device? Should it exist server-side at all given workspace-scoped calls use a header?
4. Should creating or accepting a workspace set it as active? Should login fall back to some workspace when the stored default is NULL?
5. Should delete/leave clear the stored default only when it equals the affected workspace, and should that step be fail-open like other post-commit steps?
6. When a permission is added, should existing owners (and holders of the Admin preset) receive it automatically?
7. Should the subset rule also cover demotion, removal, cancel and resend (that is, may a member only act on members/invitations whose permissions they themselves hold)?
8. Is there to be any "last admin" protection beyond the owner, or a hierarchy among non-owner members?
9. Should workspace-scoped SSE payloads (member emails, permission lists) be filtered by `read:member`?
10. Should admins see expired invitations, should update be capped at the configured maximum expiry, and should expired invitations be declinable?
11. Should roles become a server concept (stored per member, or validated presets), or stay client-only labels?
12. After transfer, should the former owner keep full permissions?
13. Should deleting the workspace picture through the generic file API require `update:workspace`?
14. Should joins, declines, workspace creation and expiry sweeps appear in the workspace audit log, and should a deleted workspace's audit trail remain readable by anyone?
15. Should stored permission arrays be normalized (deduplicated, canonical spelling)?
16. Should a removed member's sessions or stored default be updated at removal time?
