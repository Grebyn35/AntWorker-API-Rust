# Inventory: Files and object storage (`FILE`)

Reference: `AntWorker-API` @ `0210895` (2026-10-20). This file records what the old API does. It does not prescribe what the new one should do.

## Scope

Covers user-owned and workspace-owned file metadata and bytes: upload, list, delete, authenticated download, anonymous public download, the download response headers, visibility, the raw-body and multipart upload parsing and size limits, Content-Type and Content-Disposition handling, file-name encryption at rest, the S3-compatible byte store and its "disabled" mode, the object-deletion queue (trigger, stored procedures, reaper task), and avatar and workspace-picture storage (OAuth-signup ingestion, swap and clear).

Boundaries:
- `PUT /v1/users/me` (USER), `PUT /v1/workspace` (WS) and OAuth signup (AUTH) are cataloged by their domains. Only their image-storage parts are here: multipart image parsing, MIME/name/body validation, object write, the swap/clear transaction, the reap of the old file and compensation. Their name handling, audit entries and SSE events are only listed as Effects.
- The machinery for auth middleware, rate limiting, idempotency, pagination, the error envelope, transactions/RLS context helpers, `DatabaseEncryption`, the distributed lock and the re-encryption task belongs to HTTP, AUTH and DATA. Here it is referenced only where it changes a file behavior.
- Docker/SeaweedFS, `.env.example` and the `TEST_S3_*` test settings are OPS.
- No local-disk backend exists. The only backends are `s3` and `disabled`.

Production files read: `src/handlers/files.rs`, `src/handlers/mod.rs`, `src/handlers/users.rs` (update handler), `src/handlers/workspaces.rs` (update handler), `src/services/{user_file_service,workspace_file_service,public_file_service,object_deletion_reaper_service}.rs`, `src/services/user_service.rs` (`update_user`), `src/services/workspace_service.rs` (`update_workspace`, `delete_workspace`), `src/services/public_auth_service.rs` (`ingest_oauth_avatar`, `handle_new_user_oauth`), `src/services/traits.rs` (file traits), `src/repositories/{object_storage_repository,s3_object_storage_repository,object_deletion_queue_repository,traits}.rs`, `src/repositories/postgresql/{user_file_repository,workspace_file_repository,object_deletion_queue_repository,user_repository,workspace_repository}.rs`, `src/models/file.rs`, `src/errors/{storage_error,api_error,response_envelope}.rs`, `src/extractors/{multipart,body,query,path}.rs`, `src/utils/{content_disposition,content_type,database_retry,retry}.rs`, `src/config/{storage,environment}.rs`, `src/server.rs`, `src/background.rs`, `src/middleware/{idempotency,rate_limit}.rs`, `src/auth/{middleware,oauth_provider}.rs`, `src/encryption/re_encryption.rs`, `migrations/0002`–`0008`. Claims checked against the code: `documentation/OBJECT_STORAGE.md`, `README.md`, code comments. Files noted as present but not opened: `.env`, `alertmanager-secrets/`.

Tests read for coverage only: `tests/file_download_http.rs`, `tests/file_swap_repository_live.rs`, `tests/avatar_ingest_repository_live.rs`, `tests/s3_object_storage_live.rs`, `tests/object_deletion_queue_repository_live.rs`, `tests/object_deletion_trigger.rs`, `tests/public_file_repository_live.rs`, `tests/authenticated_file_repository_live.rs`, the file parts of `tests/rls_isolation.rs` and `tests/re_encryption_all_tables.rs`, and the inline `#[cfg(test)]` modules of the files above.

All paths below are relative to `AntWorker-API/`. All routes are mounted under `/v1` (`src/server.rs:698`).

## Behaviors

### FILE-001 Upload a user-owned file (raw body)

- **Behavior:** An authenticated user uploads one file as the raw request body. The server writes the bytes to the object store under `user/<user_id>/<file_id>`, inserts a `user_files` row and returns its metadata in the standard envelope with HTTP 200.
- **Kind:** `capability`
- **Trigger:** `POST /v1/users/me/files?visibility=private|public`
- **Auth:** session (user-scoped group): `user_scoped_auth_middleware` → per-user rate limit → email verification (credentials users blocked when `REQUIRE_EMAIL_VERIFICATION`) → idempotency. No permission.
- **Input & validation:** The body is the bytes. `Content-Type` header gives the MIME, `Content-Disposition` gives the name, and `visibility` is a query parameter (see FILE-002 to FILE-005, FILE-010). Validation order:
  1. Query `visibility` (400 if invalid).
  2. Body read (413 if too large).
  3. `Content-Type` header present and UTF-8.
  4. `Content-Disposition` filename.
  5. `validate_content_type` and `validate_file_name`.
  6. Non-empty body.
  7. Storage configured (503).
- **Effects:**
  - `id` is a server-generated UUIDv4. `size_bytes` is the body length.
  - `file_name` is encrypted before insert (FILE-024). `created_at`/`updated_at` come from DB defaults via `RETURNING`.
  - INFO log "User file uploaded successfully" with `file_id`. `file_name` is kept out of spans.
  - No audit entry, SSE event or metric.
- **Errors:**
  - `VALIDATION_ERROR` → 400: invalid query, header problems, shape failures, or empty body ("File body must not be empty").
  - `PAYLOAD_TOO_LARGE` → 413.
  - `SERVICE_UNAVAILABLE` → 503: storage disabled, or the store is unavailable.
  - `INTERNAL_SERVER_ERROR` → 500: permanent store fault.
  - Repository errors pass through the envelope.
- **Guarantees:** Object first, then row (FILE-006). The insert is idempotent under retry (FILE-007).
- **Source:** `src/handlers/mod.rs:242-245`, `src/handlers/files.rs:177-199`, `src/services/user_file_service.rs:60-134`, `src/repositories/postgresql/user_file_repository.rs:111-162`, `src/errors/response_envelope.rs:158-166`
- **Tests:** `tests/file_download_http.rs` (phase 3 upload); `user_file_service.rs` tests `upload_*`; `tests/authenticated_file_repository_live.rs::create_user_file_is_idempotent_on_id_conflict_and_round_trips_name`

### FILE-002 Upload headers carry MIME and file name

- **Behavior:** On both raw-body upload routes, the MIME type must come from the request `Content-Type` header and the original name from the `Content-Disposition` header. Neither comes from the query, so the name stays out of access-log request lines.
- **Kind:** `rule`
- **Trigger:** `POST /v1/users/me/files`, `POST /v1/workspace/files`
- **Auth:** As FILE-001 / FILE-009.
- **Input & validation:**
  - `Content-Type` missing → 400 "Content-Type header is required for upload". Non-UTF-8 → 400.
  - `Content-Disposition` missing → 400. Non-UTF-8 → 400. Present but no usable filename → 400 "Content-Disposition header must include a filename".
  - The disposition type (`attachment`, `inline`, anything else, or none) is ignored.
- **Errors:** `VALIDATION_ERROR` → 400 (internal code `invalid_header`).
- **Source:** `src/handlers/files.rs:23-69`, `src/handlers/files.rs:184-185`, `src/handlers/files.rs:319-320`
- **Tests:** `src/handlers/files.rs` tests `content_type_from_headers_*`, `file_name_from_headers_*`

### FILE-003 File-name extraction from Content-Disposition

- **Behavior:** The file name is parsed from a `Content-Disposition` value. The same parser serves raw-body uploads and multipart image parts.
- **Kind:** `rule`
- **Trigger:** Upload routes (FILE-002); multipart `avatar`/`picture` parts (FILE-026).
- **Input & validation:**
  - Parameters split on `;` outside double quotes. Backslash quoted-pairs inside quotes are honored.
  - Parameter keys match case-insensitively. Only `filename` and `filename*` are read, so a multipart `name=` is ignored.
  - `filename*` (RFC 5987 `charset'lang'pct-encoded`) wins over `filename` in either order, but only when the charset is `UTF-8` or `US-ASCII` and the bytes decode to valid UTF-8. Otherwise the parser falls back to `filename`.
  - `filename` is unquoted (surrounding quotes stripped, `\X` → `X`). A bare token is taken as-is.
  - A value with no disposition type (`filename="x"`) is accepted.
  - No usable value gives `None`, which callers turn into 400.
- **Source:** `src/utils/content_disposition.rs:53-78`, `src/utils/content_disposition.rs:122-188`
- **Tests:** `src/utils/content_disposition.rs` tests `parse_*`, `ext_value_*`, `falls_back_*`; `src/extractors/multipart.rs` tests `avatar_filename_with_quote_and_backslash_roundtrips`, `avatar_quoted_filename_form_is_also_accepted`

### FILE-004 MIME, file-name and body shape validation

- **Behavior:** Every user-supplied file (generic upload, avatar, workspace picture) is shape-checked before any byte is stored.
- **Kind:** `rule`
- **Trigger:** FILE-001, FILE-009, FILE-027, FILE-029.
- **Input & validation:**
  - Content-Type must be non-empty, at most 255 bytes, and shaped `type/subtype`: split at the first `/`, both sides non-empty, no second `/`. Parameters such as `; charset=utf-8` are accepted and stored verbatim. There is no MIME allow-list, so any type (for example `text/html` or `image/svg+xml`) is accepted.
  - File name must not be blank after trim, must be at most 255 bytes (UTF-8 byte length), and must contain no Unicode control characters. Path separators and other punctuation are allowed. The name is display metadata and is never used in the storage key.
  - Body must not be empty. The message is "File body must not be empty" on generic uploads, "Avatar image must not be empty" or "Picture image must not be empty" on image updates.
- **Errors:** `VALIDATION_ERROR` → 400 (internal code `field_validation`). These fire before the storage-availability check, so they return 400 even when storage is disabled.
- **Source:** `src/utils/content_type.rs:17-47`, `src/utils/content_disposition.rs:27`, `src/utils/content_disposition.rs:96-116`, `src/services/user_file_service.rs:70-77`, `src/services/workspace_file_service.rs:72-79`, `src/services/user_service.rs:315-322`, `src/services/workspace_service.rs:496-503`
- **Tests:** `src/utils/content_type.rs` tests; `src/utils/content_disposition.rs` tests `validate_*`; service tests `upload_rejects_*`, `update_user_avatar_rejects_*`, `update_workspace_picture_rejects_*`

### FILE-005 Upload body size limit

- **Behavior:** Routes that carry file bytes accept bodies up to `STORAGE_MAX_UPLOAD_BYTES` (default 10,485,760). Every other route keeps the global `MAX_REQUEST_BODY_BYTES` (default 2 MiB).
  - Limited routes: `POST /users/me/files`, `POST /workspace/files`, `PUT /users/me`, `PUT /workspace`. The limit is set per method-router, so `GET`/`DELETE` on the same path keep the global limit.
  - On multipart routes the limit covers the whole message, so the usable image size is the limit minus framing and other parts.
  - An oversized body is rejected with the standard envelope.
- **Kind:** `rule`
- **Trigger:** The four routes above.
- **Input & validation:**
  - `DefaultBodyLimit::max(STORAGE_MAX_UPLOAD_BYTES)` overrides the global layer.
  - `UploadBody` maps a 413 rejection to `PayloadTooLarge` ("Upload exceeds the maximum allowed size") and any other body-read failure to 400 ("Failed to read request body").
  - Multipart reads map a 413 the same way and other parse failures to 400 "Invalid multipart form data".
  - With an `Idempotency-Key`, the idempotency middleware first rejects a declared `Content-Length` above max(global, upload) with 413 "Request body exceeds the maximum allowed size" (see Observations for its empty-body fallback).
- **Errors:** `PAYLOAD_TOO_LARGE` → 413; `VALIDATION_ERROR` → 400.
- **Guarantees:** The whole object is buffered in memory, so the limit is also the per-request memory bound for the upload body.
- **Source:** `src/handlers/mod.rs:162-172`, `src/handlers/mod.rs:242-245`, `src/handlers/mod.rs:343-350`, `src/handlers/mod.rs:399-402`, `src/server.rs:628`, `src/server.rs:651`, `src/server.rs:739`, `src/extractors/body.rs:45-79`, `src/extractors/multipart.rs:120-129`, `src/middleware/idempotency.rs:122-124`, `src/middleware/idempotency.rs:425-437`, `src/errors/api_error.rs:345-350`, `src/errors/api_error.rs:990-995`, `src/config/environment.rs:504-507`
- **Tests:** `src/extractors/body.rs` tests; `src/extractors/multipart.rs` tests `over_limit_returns_413`, `ws_over_limit_returns_413`; `tests/idempotency_middleware_http.rs` (body limit)

### FILE-006 Object-first write ordering with orphan compensation

- **Behavior:** All upload paths write the object before the metadata row: generic, avatar, picture and OAuth ingest. If the metadata write fails after the object landed, the service attempts one best-effort delete of that object and returns the original metadata error, not the cleanup result.
- **Kind:** `guarantee` (no listed file ever points at bytes that were never written; the cleanup is the hardening half)
- **Trigger:** FILE-001, FILE-009, FILE-027, FILE-029, FILE-033.
- **Effects:**
  - Generic, avatar and picture paths: a failed cleanup logs WARN `error_type="orphan_object_cleanup_failed"` with the `storage_key` and is swallowed.
  - OAuth ingest discards the cleanup result with `let _` and logs nothing about it.
- **Guarantees:**
  - A row never exists without an attempted object write.
  - An object with no row is possible: a crash between put and insert, or a failed cleanup. Nothing ever reaps it, because the reaper only sees keys from the delete trigger (FILE-016).
- **Source:** `src/services/user_file_service.rs:85-130`, `src/services/workspace_file_service.rs:95-132`, `src/services/user_service.rs:327-380`, `src/services/workspace_service.rs:508-563`, `src/services/public_auth_service.rs:310-350`
- **Tests:** `user_file_service.rs::upload_compensates_orphan_object_when_create_fails`; `workspace_file_service.rs::upload_compensates_orphan_object_when_create_fails`; `user_service.rs::update_user_writes_object_first_and_compensates_on_swap_failure`; `workspace_service.rs::update_workspace_writes_object_first_and_compensates_on_swap_failure`; `public_auth_service.rs::ingest_oauth_avatar_compensates_object_when_metadata_persist_fails`

### FILE-007 Idempotent metadata insert under database retry

- **Behavior:**
  - Metadata inserts run inside `retry_database_operation`: at most 3 attempts, retried only on transient errors (40001, 40P01, connection).
  - The insert is `INSERT ... ON CONFLICT (id) DO UPDATE SET updated_at = <existing>` with the file id fixed before the loop. A retry after a lost-ACK commit therefore returns the already-committed row instead of a 23505 that would trigger compensation.
  - Same pattern for the swap inserts (FILE-031). Deletes tolerate 0 affected rows.
- **Kind:** `hardening`
- **Trigger:** FILE-001, FILE-009, FILE-014, FILE-015, FILE-027 to FILE-030.
- **Guarantees:** The object put happens once, before the retry loop. If every attempt fails after a commit that actually succeeded, compensation still deletes the object (see Observations).
- **Source:** `src/repositories/postgresql/user_file_repository.rs:127-142`, `src/repositories/postgresql/workspace_file_repository.rs:127-142`, `src/repositories/postgresql/user_file_repository.rs:304-311`, `src/repositories/postgresql/workspace_file_repository.rs:358-362`, `src/utils/database_retry.rs:90-105`, `src/utils/database_retry.rs:136-155`
- **Tests:** `tests/authenticated_file_repository_live.rs::create_user_file_is_idempotent_on_id_conflict_and_round_trips_name`, `::create_workspace_file_is_idempotent_on_id_conflict`

### FILE-008 Storage-disabled mode

- **Behavior:** With `STORAGE_BACKEND=disabled` (the code default), no client is built. The server still boots.
  - Return 503: generic uploads, authenticated and anonymous downloads, and avatar/picture **set**.
  - Still work: list and delete (no storage check), and avatar/picture **clear** and name-only updates.
  - OAuth signup skips avatar ingestion silently.
  - The reaper is not spawned.
- **Kind:** `rule`
- **Trigger:** Startup config; then any file route.
- **Errors:** `SERVICE_UNAVAILABLE` → 503, generic message "Service temporarily unavailable" (internal message "Object storage is not configured", code `dependency_unavailable`).
  - Uploads: the 503 comes after input validation, so invalid inputs still get 400.
  - Downloads: the 503 comes before the metadata lookup, so a nonexistent id gets 503, not 404.
- **Source:** `src/config/storage.rs:24-62`, `src/server.rs:1418-1450`, `src/services/user_file_service.rs:35-42`, `src/services/user_file_service.rs:152`, `src/services/workspace_file_service.rs:36-43`, `src/services/workspace_file_service.rs:154`, `src/services/public_file_service.rs:48-55`, `src/services/public_file_service.rs:74`, `src/services/public_file_service.rs:117`, `src/services/user_service.rs:172-178`, `src/services/workspace_service.rs:173-179`, `src/services/public_auth_service.rs:199`, `src/errors/api_error.rs:1033-1038`
- **Tests:** service tests `upload_returns_503_when_storage_disabled`, `download_returns_503_when_storage_disabled`, `user_download_returns_503_when_storage_disabled`, `workspace_download_returns_503_when_storage_disabled`, `update_user_avatar_requires_storage`, `update_workspace_picture_requires_storage`, `ingest_oauth_avatar_no_ops_when_storage_disabled`; `src/server.rs` `plan_background_tasks` tests

### FILE-009 Upload a workspace-owned file

- **Behavior:** Any member of the current workspace uploads a file into it. The key is `workspace/<workspace_id>/<file_id>` and `uploaded_by` is the caller. Returns the `workspace_files` metadata with HTTP 200.
- **Kind:** `capability`
- **Trigger:** `POST /v1/workspace/files?visibility=private|public`
- **Auth:** workspace-scoped: `workspace_scoped_auth_middleware` (validates `X-Workspace-Id` membership) → rate limit → email verification → idempotency. No permission is required. A member with zero permissions may upload, including with `visibility=public`.
- **Input & validation:** Same as FILE-001. The service also returns 400 (`invalid_header`) if the context has no `workspace_id`; in practice the middleware has already guaranteed one.
- **Effects:**
  - Insert runs under `with_workspace_context`. RLS `WITH CHECK` requires the workspace to be in the caller's membership/ownership set.
  - INFO log "Workspace file uploaded successfully".
  - No audit entry, SSE event or metric.
- **Errors:** As FILE-001.
- **Source:** `src/handlers/mod.rs:399-402`, `src/handlers/files.rs:312-334`, `src/services/workspace_file_service.rs:62-136`, `src/repositories/postgresql/workspace_file_repository.rs:110-163`, `migrations/0004_row_level_security.sql:336-338`
- **Tests:** `workspace_file_service.rs` tests `upload_*`; `tests/authenticated_file_repository_live.rs::create_workspace_file_is_idempotent_on_id_conflict`; `tests/rls_isolation.rs::app_user_cannot_write_other_tenants_data`

### FILE-010 Visibility is chosen at upload and never changes

- **Behavior:** `visibility` is a lowercase query value, `private` or `public`, defaulting to `private`. It is stored on the row (CHECK `visibility IN ('private','public')`).
  - No endpoint changes visibility or any other file column after creation.
  - Avatar and picture rows are always `public`, set by the server and not by the client. OAuth-ingested avatars are always `public`, set inside the SP.
- **Kind:** `rule`
- **Trigger:** FILE-001, FILE-009 (query); FILE-027, FILE-029, FILE-033 (server-fixed).
- **Input & validation:** Unknown or uppercase values (for example `PUBLIC`) → 400 "Invalid query parameter: ...". Extra query parameters are ignored.
- **Errors:** `VALIDATION_ERROR` → 400.
- **Source:** `src/models/file.rs:29-65`, `src/models/file.rs:122-126`, `src/extractors/query.rs:109-160`, `src/services/user_service.rs:343`, `src/services/workspace_service.rs:525-527`, `migrations/0005_stored_procedures.sql:265-266`, `migrations/0002_tables.sql:332`, `migrations/0002_tables.sql:353`
- **Tests:** `src/models/file.rs` tests `upload_query_defaults_to_private`, `visibility_serde_is_lowercase`; `tests/file_download_http.rs` (default private)

### FILE-011 File metadata wire shape

- **Behavior:** Upload and list responses serialize the metadata as follows. Timestamps are RFC 3339.
  - User files: `{id, user_id, content_type, file_name, size_bytes, visibility, created_at, updated_at}`.
  - Workspace files: `{id, workspace_id, uploaded_by|null, content_type, file_name, size_bytes, visibility, created_at, updated_at}`.
  - `storage_key` is never serialized.
  - `file_name` is the decrypted plaintext.
  - `updated_at` never changes after creation.
- **Kind:** `guarantee`
- **Trigger:** FILE-001, FILE-009, FILE-012, FILE-013.
- **Guarantees:** The internal key, which embeds the owner id, never reaches clients. Files are addressed only by `id`.
- **Source:** `src/models/file.rs:77-115`
- **Tests:** `src/models/file.rs` tests `user_file_serializes_file_name_but_not_storage_key`, `workspace_file_serializes_file_name_but_not_storage_key`

### FILE-012 List own user files

- **Behavior:** Returns a keyset-paginated page of the caller's own `user_files` rows, all visibilities, avatar files included. Rows are ordered by `(created_at, id)`.
- **Kind:** `capability`
- **Trigger:** `GET /v1/users/me/files`
- **Auth:** session (user-scoped group, email verification applies).
- **Input & validation:** `PaginationParams`: `limit` 1–100 (default 25), `after`/`before` cursors, `sort_by` allowlist `["created_at"]` only, `sort_direction` (default `desc`). Violations → 400.
- **Effects:** Read under `with_user_context`. RLS limits rows to own or public; an explicit `user_id = caller` filter limits them to own. Every `file_name` is decrypted.
- **Errors:** `VALIDATION_ERROR` → 400 (pagination); `DATA_INTEGRITY_ERROR` → 500 if any row fails to decrypt or has an unknown visibility (FILE-025). No storage check.
- **Source:** `src/handlers/mod.rs:246`, `src/handlers/files.rs:215-229`, `src/services/user_file_service.rs:193-202`, `src/repositories/postgresql/user_file_repository.rs:41-51`, `src/repositories/postgresql/user_file_repository.rs:176-239`, `src/utils/pagination.rs:197-227`
- **Tests:** `tests/authenticated_file_repository_live.rs::list_user_files_returns_the_owners_page`, `::list_user_files_keyset_pages_forward_and_backward`

### FILE-013 List current workspace's files

- **Behavior:** Returns a keyset-paginated page of every `workspace_files` row in the `X-Workspace-Id` workspace, regardless of uploader or visibility. The workspace picture file is included.
- **Kind:** `capability`
- **Trigger:** `GET /v1/workspace/files`
- **Auth:** workspace-scoped (membership), email verification. No permission.
- **Input & validation:** As FILE-012.
- **Effects:** Read under `with_workspace_context` with an explicit `workspace_id = ctx.workspace_id` filter.
- **Errors:** As FILE-012.
- **Source:** `src/handlers/mod.rs:403`, `src/handlers/files.rs:351-365`, `src/services/workspace_file_service.rs:196-205`, `src/repositories/postgresql/workspace_file_repository.rs:178-245`
- **Tests:** none found for the workspace list query (user-list tests only).

### FILE-014 Delete own user file

- **Behavior:** Only the owner can delete a user file. A missing file and a file readable only because it is public (someone else's) both return the same 404. On success the row is deleted and 200 with `data: {}` is returned. Bytes are removed later by the reaper (FILE-016).
- **Kind:** `capability`
- **Trigger:** `DELETE /v1/users/me/files/{file_id}`
- **Auth:** session (user-scoped, email verification). Owner only.
- **Input & validation:** `file_id` must be a UUID; otherwise 400 "Invalid path parameter: ...".
- **Effects:**
  - The service checks with `find_readable`, then requires `user_id == caller`.
  - It then runs `DELETE FROM user_files WHERE id=$1 AND user_id=$2`, retried and idempotent.
  - The AFTER DELETE trigger enqueues the `storage_key`.
  - If the file was the caller's avatar, `users.avatar_file_id` becomes NULL (FILE-017).
  - INFO log. No audit entry or SSE event. No storage call.
- **Errors:** `RESOURCE_NOT_FOUND` → 404, message `user_file not found: <id>`, details `{entity_type:"user_file", entity_id}`; `VALIDATION_ERROR` → 400.
- **Guarantees:** No existence disclosure beyond what read access already grants. A concurrent double delete can return 200 to both callers.
- **Source:** `src/handlers/mod.rs:247`, `src/handlers/files.rs:245-256`, `src/services/user_file_service.rs:215-241`, `src/repositories/postgresql/user_file_repository.rs:298-329`, `src/errors/api_error.rs:181-193`, `migrations/0004_row_level_security.sql:322-324`
- **Tests:** `user_file_service.rs::delete_404_when_not_found`, `::delete_404_when_not_owned`, `::delete_succeeds_for_owner`; `tests/authenticated_file_repository_live.rs` (idempotent delete); `tests/rls_isolation.rs::app_user_cannot_delete_other_tenants_data`

### FILE-015 Delete a workspace file

- **Behavior:** Any member of the current workspace deletes any file in it, whoever uploaded it and whatever its visibility. This includes the file currently used as the workspace picture. A file outside the current workspace returns 404. Success returns 200 with `data: {}`.
- **Kind:** `capability`
- **Trigger:** `DELETE /v1/workspace/files/{file_id}`
- **Auth:** workspace-scoped (membership), email verification. No permission; `update:workspace` is not required even when the file is the workspace picture.
- **Input & validation:** UUID path parameter.
- **Effects:**
  - `find_in_workspace` (`id AND workspace_id = ctx`), then `DELETE ... WHERE id AND workspace_id`, retried and idempotent.
  - The trigger enqueues the key.
  - If the file is the workspace picture, `workspaces.picture_file_id` becomes NULL (FILE-017).
  - No audit entry or SSE event.
- **Errors:** `RESOURCE_NOT_FOUND` → 404 (`workspace_file not found: <id>`); `VALIDATION_ERROR` → 400.
- **Source:** `src/handlers/mod.rs:404-407`, `src/handlers/files.rs:382-393`, `src/services/workspace_file_service.rs:219-239`, `src/repositories/postgresql/workspace_file_repository.rs:306-380`, `migrations/0004_row_level_security.sql:345-347`
- **Tests:** `workspace_file_service.rs::delete_404_when_not_in_workspace`, `::delete_succeeds_when_member_is_not_uploader`

### FILE-016 Every metadata-row deletion enqueues its object for deletion

- **Behavior:** Every `user_files` or `workspace_files` row removed by any path enqueues its `storage_key` into `pending_object_deletions`, through an `AFTER DELETE FOR EACH ROW` trigger in the same transaction. Paths covered:
  - The explicit delete endpoints.
  - Avatar/picture reaps.
  - User account deletion: `user_files` cascade, plus owned workspaces cascading to `workspace_files`.
  - Workspace deletion: `workspace_files` cascade.
  - Uploader deletion only sets `uploaded_by` to NULL (an UPDATE), so the file and its bytes stay.
- **Kind:** `guarantee`
- **Trigger:** Any `DELETE` on the two tables, including FK cascades.
- **Effects:** Inserts `{id, storage_key, enqueued_at=NOW(), attempts=0}`. A rolled-back delete enqueues nothing. Services never call `storage.delete` on row deletion.
- **Guarantees:** No row-deletion path can drop a key, so bytes are removed eventually (FILE-035). The queue has no FK, so it outlives the owner it reaps for.
- **Source:** `migrations/0007_triggers.sql:54-71`, `migrations/0002_tables.sql:611-637`, `migrations/0002_tables.sql:657-666`, `migrations/0008_grants.sql:37`, `migrations/0008_grants.sql:168`, `src/services/workspace_service.rs:753-776`
- **Tests:** `tests/object_deletion_trigger.rs` (`direct_file_delete_enqueues_storage_key`, `user_deletion_cascade_enqueues_owned_file_key`, `workspace_deletion_cascade_enqueues_file_key`, `uploader_deletion_nulls_attribution_without_enqueue`)

### FILE-017 Deleting the backing file clears the avatar or picture pointer

- **Behavior:** `users.avatar_file_id → user_files(id)` and `workspaces.picture_file_id → workspace_files(id)` are both `ON DELETE SET NULL`. Deleting the file row by any path leaves the user or workspace in place with no image.
- **Kind:** `rule`
- **Trigger:** FILE-014, FILE-015, cascades.
- **Effects:** Pointer set to NULL. When this happens through the generic delete endpoints, no `profile.updated` or workspace-update SSE event and no audit entry is emitted.
- **Source:** `migrations/0002_tables.sql:667-681`, `migrations/0003_indexes.sql:114-128`
- **Tests:** none found for the generic-delete case.

### FILE-018 Authenticated download of a user file

- **Behavior:** Returns the raw bytes of a user file that is the caller's own (any visibility) or public (anyone's), with the headers in FILE-021. Any other file returns 404.
- **Kind:** `capability`
- **Trigger:** `GET /v1/files/user/{file_id}`. HEAD is also answered, through Axum's GET routing.
- **Auth:** session (user-scoped group; no `X-Workspace-Id`; email verification and per-user rate limit apply). Read access is decided by RLS `user_files_select`: `user_id = app.current_user_id OR visibility='public'`.
- **Input & validation:** UUID path parameter.
- **Effects:**
  1. Storage presence check.
  2. `find_readable` under `with_user_context`, with a bare `WHERE id=$1` and RLS deciding.
  3. `storage.get(storage_key)`, which loads the whole object into memory.
- **Errors:** `SERVICE_UNAVAILABLE` → 503 (disabled, checked first); `RESOURCE_NOT_FOUND` → 404 (`user_file not found: <id>`) for missing, unreadable, or missing bytes (FILE-022); store faults per FILE-023.
- **Guarantees:** A non-owner of a private file cannot tell it from a missing one. The original file name is sent to any reader, including non-owners of public files.
- **Source:** `src/handlers/mod.rs:251`, `src/handlers/files.rs:272-288`, `src/services/user_file_service.rs:147-180`, `src/repositories/postgresql/user_file_repository.rs:253-284`, `migrations/0004_row_level_security.sql:306-311`
- **Tests:** `tests/file_download_http.rs` (phase 3, owner download); `user_file_service.rs::download_*`; `tests/rls_isolation.rs::rls_cross_tenant_visibility_is_intentional_where_designed`, `::app_user_cannot_read_other_tenants_data`

### FILE-019 Authenticated download of a workspace file

- **Behavior:** Returns the bytes of a workspace file when it is public, or when the caller is a member or owner of the workspace it belongs to. Membership is checked across all of the caller's workspaces; the caller's current `X-Workspace-Id` workspace is not required. Anything else returns 404.
- **Kind:** `capability`
- **Trigger:** `GET /v1/files/workspace/{file_id}`
- **Auth:** session (user-scoped group, no workspace header). RLS `workspace_files_select`: `workspace_id = ANY(get_current_user_workspace_ids()) OR visibility='public'`. The membership set is memberships plus owned workspaces, read fresh from the DB on every request, so a removed member loses access to private files immediately. Pending invitees get no access.
- **Input & validation:** UUID path parameter.
- **Effects:** As FILE-018, against `workspace_files`.
- **Errors:** As FILE-018, with `workspace_file` as the entity type.
- **Source:** `src/handlers/mod.rs:252-255`, `src/handlers/files.rs:410-426`, `src/services/workspace_file_service.rs:149-182`, `src/repositories/postgresql/workspace_file_repository.rs:259-291`, `migrations/0004_row_level_security.sql:144-160`, `migrations/0004_row_level_security.sql:329-334`
- **Tests:** `workspace_file_service.rs::download_maps_missing_object_to_404_not_storage_error`; `tests/rls_isolation.rs` (workspace_files visibility)

### FILE-020 Anonymous download of a public file

- **Behavior:** Anyone, with no session, can download a user file or workspace file by id when its visibility is `public`. A private id and a missing id return 404 envelopes that are identical apart from the echoed id. The original file name is withheld.
- **Kind:** `capability`
- **Trigger:** `GET /v1/files/public/user/{file_id}`, `GET /v1/files/public/workspace/{file_id}`
- **Auth:** public. The routes go through the per-IP rate limit (key `ip:<ip>`, `RATE_LIMIT_PER_IP`) and the idempotency middleware, which does nothing for GET.
- **Input & validation:** UUID path parameter.
- **Effects:**
  - Storage presence check (503 first).
  - `SECURITY DEFINER` SP `sp_public_find_public_user_file` / `sp_public_find_public_workspace_file` (`WHERE id=$1 AND visibility='public'`) under `with_public_context`.
  - `storage.get`.
  - The `file_name` ciphertext is still decrypted on read even though it is not sent.
- **Errors:** `SERVICE_UNAVAILABLE` → 503; `RESOURCE_NOT_FOUND` → 404; store faults per FILE-023; `DATA_INTEGRITY_ERROR` → 500 if `file_name` fails to decrypt.
- **Guarantees:** The SP's `visibility='public'` predicate is the only barrier on this path (RLS is bypassed). File ids are UUIDv4, so the URL works as an unguessable capability link.
- **Source:** `src/handlers/mod.rs:79-86`, `src/handlers/files.rs:452-490`, `src/services/public_file_service.rs:70-139`, `src/repositories/postgresql/user_file_repository.rs:347-375`, `src/repositories/postgresql/workspace_file_repository.rs:398-427`, `migrations/0005_stored_procedures.sql:282-324`, `migrations/0008_grants.sql:77-78`, `src/middleware/rate_limit.rs:185-214`
- **Tests:** `tests/file_download_http.rs` (phases 1–2); `tests/public_file_repository_live.rs`; `public_file_service.rs` tests

### FILE-021 Download response headers

- **Behavior:** Every download response (FILE-018/019/020) is the raw bytes, not the JSON envelope, with these headers:
  - `Content-Type` = the stored row `content_type`, verbatim including parameters. The object's own content-type attribute is ignored.
  - `Content-Disposition` = `inline; filename*=UTF-8''<pct-encoded>` on authenticated routes, using the RFC 5987 attr-char set with only an extended value and no ASCII `filename=` fallback; bare `inline` on anonymous routes.
  - `X-Content-Type-Options: nosniff`.
  - `Content-Security-Policy: default-src 'none'; sandbox`.
  - `Cache-Control: private, no-store` for private files, `public, max-age=3600` for public files, whichever route served them.
  - No ETag, Last-Modified, Accept-Ranges or Range support.
- **Kind:** `guarantee`
- **Trigger:** The four download routes.
- **Errors:** `INTERNAL_SERVER_ERROR` → 500 if the stored content type or the encoded disposition is not a valid header value. This is reachable only through data corruption.
- **Guarantees:** Uploaded HTML/SVG served from the API origin cannot run scripts or be MIME-sniffed into an active type.
- **Source:** `src/handlers/files.rs:102-154`, `src/utils/content_disposition.rs:33-45`, `src/utils/content_disposition.rs:86-89`
- **Tests:** `src/handlers/files.rs` tests `object_response_*`; `src/utils/content_disposition.rs::inline_disposition_*`; `tests/file_download_http.rs`

### FILE-022 Row without backing object returns a plain 404

- **Behavior:** If a readable or public row exists but `storage.get` reports `ObjectNotFound`, every download path returns the same 404 as a missing row (`<entity> not found: <id>` with entity details), never a storage error. A WARN log records `error_type="orphaned_metadata_row"` and the full `storage_key`.
- **Kind:** `hardening`
- **Trigger:** FILE-018/019/020.
- **Errors:** `RESOURCE_NOT_FOUND` → 404.
- **Guarantees:** `storage_key` never reaches the client. The row stays listed and is never repaired.
- **Source:** `src/services/user_file_service.rs:161-178`, `src/services/workspace_file_service.rs:165-180`, `src/services/public_file_service.rs:85-99`, `src/services/public_file_service.rs:126-137`
- **Tests:** `*::download_maps_missing_object_to_404_not_storage_error`, `public_file_service.rs::*_maps_orphaned_object_to_404`

### FILE-023 Storage fault mapping to HTTP

- **Behavior:** Byte-store failures reach the wire as generic envelopes, with no key, operation or reason in the response:
  - `ObjectNotFound` → 404 `RESOURCE_NOT_FOUND` "Object not found", no details. Only reachable outside the download paths, which remap it per FILE-022.
  - `Unavailable` → 503 `SERVICE_UNAVAILABLE` "Object storage temporarily unavailable".
  - `RequestFailed` → 500 `INTERNAL_SERVER_ERROR` "An unexpected error occurred".
  - Details (operation, reason, key, trace_id) go only to the log context.
- **Kind:** `guarantee`
- **Trigger:** Any `put`/`get` failure on the upload, download or avatar/picture paths.
- **Source:** `src/errors/storage_error.rs:31-156`, `src/errors/api_error.rs:873-908`
- **Tests:** `src/errors/api_error.rs` StorageError variant tests; `src/errors/storage_error.rs` tests

### FILE-024 File names are encrypted at rest; bytes are not

- **Behavior:**
  - `file_name` is encrypted with the application `DatabaseEncryption` (AES-256-GCM, versioned key ring) on every write path: generic create, avatar/picture swap, OAuth ingest. It is decrypted on every read. There is no blind index.
  - `storage_key`, `content_type`, `size_bytes` and `visibility` are plaintext.
  - Object bytes are sent to S3 unencrypted by the app; no server-side-encryption option is set.
  - During a key rotation the DATA re-encryption task rewrites `file_name` on both tables in id-keyset batches.
- **Kind:** `guarantee`
- **Trigger:** File writes and reads; background re-encryption task (DATA).
- **Effects:** The re-encryption task reports `user_files_processed/failed` and `workspace_files_processed/failed` in its cycle log.
- **Source:** `src/repositories/postgresql/user_file_repository.rs:75-95`, `src/repositories/postgresql/user_file_repository.rs:120`, `src/repositories/postgresql/user_file_repository.rs:402`, `src/repositories/postgresql/workspace_file_repository.rs:72-93`, `src/repositories/postgresql/workspace_file_repository.rs:120`, `src/repositories/postgresql/user_repository.rs:359`, `src/repositories/postgresql/workspace_repository.rs:349`, `src/repositories/s3_object_storage_repository.rs:152-171`, `src/encryption/re_encryption.rs:208-214`, `src/encryption/re_encryption.rs:832-951`, `migrations/0005_stored_procedures.sql:2099-2146`
- **Tests:** `tests/authenticated_file_repository_live.rs` (name round-trip); `tests/re_encryption_all_tables.rs` (files rotate); `tests/file_download_http.rs` (phase 3)

### FILE-025 Corrupt metadata fails the read

- **Behavior:** A row whose `visibility` is outside `private|public`, or whose `file_name` does not decrypt, raises `DataIntegrityError` → 500 `DATA_INTEGRITY_ERROR` with no details on the wire. On list endpoints one bad row fails the whole page.
- **Kind:** `hardening`
- **Trigger:** Any metadata read (list, download, find_public, find_in_workspace).
- **Errors:** `DATA_INTEGRITY_ERROR` → 500.
- **Source:** `src/repositories/postgresql/user_file_repository.rs:75-82`, `src/repositories/postgresql/workspace_file_repository.rs:72-79`, `src/models/file.rs:58-64`, `src/errors/api_error.rs:734-736`
- **Tests:** `src/models/file.rs::visibility_unknown_db_value_returns_offending_string`

### FILE-026 Multipart image-update form contract

- **Behavior:** `PUT /users/me` and `PUT /workspace` accept `multipart/form-data` only.
  - Form parts: optional text `name`, optional file part (`avatar` or `picture`), and an optional text flag (`remove_avatar` or `remove_picture`).
  - These fold into a three-state image intent:
    - Neither the file part nor flag `true` → `Unchanged`.
    - The file part → `Set`.
    - Flag `true` without a file part → `Cleared`.
    - Both the file part and flag `true` → 400.
  - The file part must carry its own `Content-Type` and a filename in its `Content-Disposition` (parsed per FILE-003).
  - Unknown parts are ignored. Repeated parts are accepted and the last one wins.
- **Kind:** `rule`
- **Trigger:** `PUT /v1/users/me` (profile), `PUT /v1/workspace` (workspace).
- **Auth:** Profile: session. Workspace: workspace-scoped; `update:workspace` is checked only after the body is parsed (WS).
- **Input & validation:**
  - Non-multipart request or missing boundary → 400 "Expected a multipart/form-data request body".
  - Malformed part or non-UTF-8 text → 400 "Invalid multipart form data".
  - Flag must be `"true"` or `"false"` after trim; anything else → 400.
  - File part without Content-Type → 400 "The avatar|picture part is missing its Content-Type". Without filename → 400 "The avatar|picture part is missing its filename".
  - Over limit → 413 (FILE-005).
  - `name` is normalized (trim, collapse spaces) and validated: 1–100 characters for the profile, 1–255 for the workspace, not whitespace-only. Name rules belong to USER/WS.
  - An empty file part passes the extractor and is rejected by the service (FILE-004).
- **Errors:** `VALIDATION_ERROR` → 400; `PAYLOAD_TOO_LARGE` → 413.
- **Source:** `src/extractors/multipart.rs:67-140`, `src/extractors/multipart.rs:142-226`, `src/extractors/multipart.rs:236-329`, `src/models/file.rs:144-170`, `src/handlers/users.rs:90-115`, `src/handlers/workspaces.rs:268-286`
- **Tests:** `src/extractors/multipart.rs` tests (all)

### FILE-027 Set a new profile avatar

- **Behavior:** A user replaces their avatar in one request. The image is validated (FILE-004), written as a public user object at `user/<user_id>/<file_id>`, then recorded in one transaction (FILE-031):
  1. Insert the `user_files` row: public, the client's file name encrypted, `size_bytes` = image length.
  2. Point `users.avatar_file_id` at it, applying any supplied name.
  3. Delete the previous avatar row, which enqueues its bytes for deletion.

  Returns the updated user (USER owns the response shape).
- **Kind:** `capability`
- **Trigger:** `PUT /v1/users/me` with an `avatar` part.
- **Auth:** session (user-scoped, email verification). Self only.
- **Input & validation:** FILE-026, FILE-004. Storage is required (FILE-008).
- **Effects:**
  - DB writes as above.
  - Owned by USER: audit `UserUpdate` with old/new `avatar_file_id`; SSE `profile.updated` (user-scoped) and `member.profile_updated` (each workspace the user belongs to). Both are best-effort and happen after commit.
- **Errors:** As FILE-001, plus USER's errors.
- **Source:** `src/services/user_service.rs:273-381`, `src/services/user_service.rs:435-510`, `src/repositories/postgresql/user_repository.rs:332-486`
- **Tests:** `user_service.rs` `update_user_avatar_*`, `update_user_writes_object_first_and_compensates_on_swap_failure`; `tests/file_swap_repository_live.rs` `swap_avatar_*`

### FILE-028 Remove the profile avatar

- **Behavior:** `remove_avatar=true` sets `users.avatar_file_id` to NULL and deletes the prior avatar row in one transaction (FILE-031); the delete enqueues its bytes. If there is no avatar and no name change, the call does nothing and writes no audit entry or SSE event.
- **Kind:** `capability`
- **Trigger:** `PUT /v1/users/me` with `remove_avatar=true`.
- **Auth:** session.
- **Effects:** As FILE-027, minus the new file. Works with storage disabled.
- **Source:** `src/services/user_service.rs:389-402`, `src/repositories/postgresql/user_repository.rs:500-606`
- **Tests:** `user_service.rs::update_user_clear_*`; `tests/file_swap_repository_live.rs` `clear_avatar_*`

### FILE-029 Set a new workspace picture

- **Behavior:** A member holding `update:workspace` replaces the workspace picture. The image is validated, written as a public workspace object at `workspace/<wid>/<file_id>` with `uploaded_by` = caller, then in one transaction:
  1. Insert the `workspace_files` row.
  2. Point `workspaces.picture_file_id` at it, applying any supplied name.
  3. Delete the previous picture row (scoped `AND workspace_id`).
- **Kind:** `capability`
- **Trigger:** `PUT /v1/workspace` with a `picture` part.
- **Auth:** workspace-scoped plus permission `update:workspace` (checked in the service before any storage work).
- **Input & validation:** FILE-026, FILE-004; storage required.
- **Effects:** DB writes as above. Audit and SSE are owned by WS.
- **Errors:** `PERMISSION_DENIED` → 403 without the permission; otherwise as FILE-001.
- **Source:** `src/services/workspace_service.rs:444-564`, `src/repositories/postgresql/workspace_repository.rs:324-477`
- **Tests:** `workspace_service.rs` `update_workspace_picture_*`, `update_workspace_writes_object_first_and_compensates_on_swap_failure`, `update_workspace_requires_update_permission`; `tests/file_swap_repository_live.rs` `swap_picture_*`

### FILE-030 Remove the workspace picture

- **Behavior:** `remove_picture=true` sets `picture_file_id` to NULL and deletes the prior picture row in one transaction. If there is no picture and no name change, the call does nothing.
- **Kind:** `capability`
- **Trigger:** `PUT /v1/workspace` with `remove_picture=true`.
- **Auth:** workspace-scoped plus `update:workspace`.
- **Source:** `src/services/workspace_service.rs:572-585`, `src/repositories/postgresql/workspace_repository.rs:491-600`
- **Tests:** `workspace_service.rs::update_workspace_clear_*`; `tests/file_swap_repository_live.rs::clear_picture_nulls_pointer_reaps_prior_and_preserves_name`

### FILE-031 Image swap and clear are atomic, serialized and owner-scoped

- **Behavior:** Each swap or clear is one transaction, run under `with_user_context`, in this order:
  1. `SELECT <image fk> ... FOR UPDATE` on the user or workspace row, which reads the current image under the lock.
  2. Insert the new file (`ON CONFLICT (id)`).
  3. `UPDATE ... SET name = COALESCE($name, name), <fk> = new|NULL`.
  4. Delete the prior file only if it exists and differs from the new id, scoped `AND user_id = caller` or `AND workspace_id = wid`.

  The whole operation is wrapped in `retry_database_operation`.
- **Kind:** `guarantee`
- **Trigger:** FILE-027 to FILE-030.
- **Guarantees:**
  - Two racing replacements serialize, and each removes the file that was actually current, so no file row is orphaned.
  - A retry after a lost ACK sees the new file as current and skips the delete, so the live image is never removed.
  - A name-less update keeps a concurrently changed name.
  - The image is only ever changed through these methods, so other writes cannot undo a swap.
  - Old bytes are reaped through the trigger.
- **Source:** `src/repositories/postgresql/user_repository.rs:375-470`, `src/repositories/postgresql/user_repository.rs:527-590`, `src/repositories/postgresql/workspace_repository.rs:365-463`, `src/repositories/postgresql/workspace_repository.rs:518-588`, `src/services/user_service.rs:348-356`, `src/services/workspace_service.rs:532-540`
- **Tests:** `tests/file_swap_repository_live.rs` (`concurrent_avatar_swaps_serialize_and_orphan_nothing`, `*_is_idempotent_when_new_file_is_current_*`, `*_reap_is_*_scoped_foreign_file_survives`, `swap_avatar_with_name_none_preserves_existing_name`)

### FILE-032 Image swap failure removes the just-written object

- **Behavior:** If the swap transaction fails after retries, the service attempts one delete of the object it just wrote, logs WARN `orphan_object_cleanup_failed` if that fails, and returns the original error. Nothing is committed, so the prior avatar or picture is unchanged.
- **Kind:** `hardening`
- **Trigger:** FILE-027, FILE-029 failures.
- **Source:** `src/services/user_service.rs:358-380`, `src/services/workspace_service.rs:542-563`
- **Tests:** `update_user_writes_object_first_and_compensates_on_swap_failure`, `update_workspace_writes_object_first_and_compensates_on_swap_failure`

### FILE-033 OAuth signup ingests the provider avatar

- **Behavior:** When a new user registers through OAuth and the provider returns a picture URL, the server:
  1. Fetches the image over HTTP, after the user-creation transaction has committed.
  2. Stores it at `user/<user_id>/<file_id>`.
  3. Calls `sp_public_ingest_user_avatar`. In one SECURITY DEFINER call it inserts a public `user_files` row with `file_name` = the encrypted constant `"avatar"` and runs `UPDATE users SET avatar_file_id = <new id> WHERE id = <user>`.

  Later logins never re-sync the avatar, and account linking does not ingest one.
- **Kind:** `capability` (the new user sees their provider avatar without uploading one)
- **Trigger:** OAuth signup of a new user (AUTH `POST /v1/auth/oauth`), in `handle_new_user_oauth`.
- **Auth:** pre-auth (public context).
- **Effects:**
  - Object put, then the SP. The created user's `avatar_file_id` is returned in the login response (AUTH).
  - INFO log "OAuth avatar ingested at signup".
  - Every failure is fail-open: WARN with `action="failing_open"` and `error_type` one of `avatar_fetch_failed`, `avatar_fetch_non_success_status`, `avatar_not_image`, `avatar_too_large`, `avatar_body_read_failed`, `avatar_empty`, `avatar_object_store_failed`, `avatar_metadata_persist_failed`.
- **Errors:** None surface. Signup never fails because of the avatar.
- **Guarantees:** The SP's `WHERE id = p_user_id` is the only tenant boundary on the pointer write.
- **Source:** `src/services/public_auth_service.rs:181-354`, `src/services/public_auth_service.rs:577-584`, `src/repositories/postgresql/user_file_repository.rs:389-433`, `migrations/0005_stored_procedures.sql:249-270`
- **Tests:** `public_auth_service.rs` `ingest_oauth_avatar_*`; `tests/avatar_ingest_repository_live.rs::ingest_avatar_is_scoped_to_the_target_user`

### FILE-034 OAuth avatar acceptance limits

- **Behavior:** The fetched avatar is accepted only when all of these hold:
  - The response status is 2xx.
  - The response Content-Type, with parameters stripped, starts with `image/`. The stripped value is what gets stored.
  - A declared `Content-Length` is at most `STORAGE_MAX_UPLOAD_BYTES`. Otherwise the body is not read.
  - The streamed body stays within that cap, checked chunk by chunk with a running total.
  - The body is not empty.
  - Storage is enabled.
- **Kind:** `rule`
- **Trigger:** FILE-033.
- **Input & validation:**
  - The HTTP client uses `OAUTH_PROVIDER_CONNECT_TIMEOUT_SECS` / `OAUTH_PROVIDER_TOTAL_TIMEOUT_SECS` and reqwest's default redirect following (up to 10).
  - There is no host or IP allow-list.
  - `validate_content_type` and `validate_file_name` are not applied.
- **Source:** `src/services/public_auth_service.rs:145-150`, `src/services/public_auth_service.rs:199-303`, `src/auth/oauth_provider.rs:39-51`
- **Tests:** `public_auth_service.rs` `ingest_oauth_avatar_rejects_*`, `ingest_oauth_avatar_fails_open_*`

### FILE-035 Object-deletion reaper cycle

- **Behavior:** Each cycle the reaper:
  1. Claims up to `STORAGE_DELETION_BATCH_SIZE` queue rows (FILE-036).
  2. For each row in order, calls `storage.delete(key)`, which is idempotent: a missing key counts as success.
  3. On success, deletes the queue row. On failure, records the error text in `last_error` and continues with the rest of the batch.
  4. Reads the count of exhausted rows (FILE-037).

  Result: `{deleted, failed, exhausted}`.
- **Kind:** `guarantee`
- **Trigger:** Background job `object_deletion_reaper`.
- **Effects:**
  - Per-row failure: WARN with `storage_key`, `error_code` and the error. The text stored in `last_error` is the `StorageError` Display string.
  - A DB error in `complete` or `record_error` aborts the cycle with `Err`. Rows already claimed but not yet processed wait out the backoff.
- **Guarantees:** Bytes of deleted rows are removed eventually while the store is healthy. One stuck key does not block the others.
- **Source:** `src/services/object_deletion_reaper_service.rs:52-95`, `src/repositories/postgresql/object_deletion_queue_repository.rs:45-132`, `migrations/0006_background_tasks.sql:483-519`
- **Tests:** `object_deletion_reaper_service.rs` tests; `tests/object_deletion_queue_repository_live.rs` (`complete_*`, `record_error_*`)

### FILE-036 Queue claim eligibility and retry backoff

- **Behavior:** `sp_object_deletion_claim_batch(limit, backoff, max)` claims rows in a single `UPDATE`.
  - Eligible rows have `attempts < max` and either `last_attempt_at IS NULL` or `last_attempt_at < NOW() - backoff`.
  - Ordering is `last_attempt_at NULLS FIRST, enqueued_at`. The claim uses `FOR UPDATE SKIP LOCKED`.
  - Claiming sets `attempts += 1` and `last_attempt_at = NOW()`, so attempts count at claim time. A crash mid-delete uses up an attempt and the row waits out the backoff.
  - The backoff is fixed (not exponential). `record_error` changes only `last_error`.
- **Kind:** `rule`
- **Trigger:** FILE-035.
- **Source:** `migrations/0006_background_tasks.sql:444-480`, `migrations/0006_background_tasks.sql:503-519`, `migrations/0003_indexes.sql:130-134`
- **Tests:** `tests/object_deletion_queue_repository_live.rs::claim_batch_selects_only_eligible_rows_and_marks_them`, `::record_error_sets_last_error_without_advancing_attempts_or_last_attempt_at`

### FILE-037 Permanently failed deletions are reported every cycle

- **Behavior:** Rows with `attempts >= STORAGE_DELETION_MAX_ATTEMPTS` are never claimed again and are never removed. Their bytes stay in the store. Every cycle counts them, even when the batch was empty, and the task logs:
  - WARN "...completed with failures - objects may be leaked at the byte store" when `exhausted > 0` or `failed > 0` (fields `deleted`, `failed`, `exhausted`).
  - INFO when only deletions happened.
  - DEBUG when there was nothing to do.
  - ERROR when the cycle failed after retries.
- **Kind:** `guarantee`
- **Trigger:** FILE-035.
- **Effects:** Logs with `component="background_cleanup" subsystem="object_deletion" cleanup_type="object_deletion"`.
- **Source:** `src/services/object_deletion_reaper_service.rs:87-92`, `migrations/0006_background_tasks.sql:521-542`, `src/server.rs:2045-2091`
- **Tests:** `object_deletion_reaper_service.rs::surfaces_exhausted_count_even_when_batch_is_empty`; `tests/object_deletion_queue_repository_live.rs::count_exhausted_counts_rows_at_or_over_the_attempt_ceiling`

### FILE-038 Reaper scheduling and gating

- **Behavior:** The reaper is spawned only when `ENABLE_BACKGROUND_CLEANUP=true` and a storage backend is configured.
  - It runs every `STORAGE_DELETION_REAPER_INTERVAL_SECS`. The first tick fires right away at startup.
  - Each tick takes the distributed lock `object_deletion_reaper`, so only one instance runs a cycle.
  - The cycle runs under `retry_with_backoff` with 3 attempts on transient repository errors. A retry re-runs the whole cycle.
  - It stops between cycles on shutdown.
  - When it is not spawned, an INFO log gives the reason (`ENABLE_BACKGROUND_CLEANUP=false` or `storage_disabled`).
- **Kind:** `rule`
- **Trigger:** Startup.
- **Source:** `src/server.rs:766-797`, `src/server.rs:1479-1484`, `src/server.rs:1995-2115`, `src/background.rs:55-115`
- **Tests:** `src/server.rs` `plan_background_tasks` tests

### FILE-039 S3-compatible backend selection and client configuration

- **Behavior:** `STORAGE_BACKEND=s3` builds one `object_store` AmazonS3 client at startup and shares it across all services and replicas. The client is configured with:
  - Bucket, region and access key / secret.
  - Optional endpoint, for non-AWS vendors.
  - Path-style addressing unless `S3_FORCE_PATH_STYLE=false`.
  - Plain HTTP only when `S3_ALLOW_HTTP=true`.
  - Connect and per-request timeouts.
  - A retry window (`S3_RETRY_TIMEOUT_SECS`, where 0 means a single attempt) that bounds how long an outage holds a request before it returns 503.

  Endpoint reachability is not checked at boot. Success logs INFO "Object storage initialized (S3-compatible backend)" with bucket, region and endpoint.
- **Kind:** `capability` (the operator can point at any S3 vendor by changing env vars)
- **Trigger:** Startup.
- **Errors:** A client build failure aborts startup with "Failed to build S3 object-storage client: ...".
- **Source:** `src/repositories/s3_object_storage_repository.rs:49-111`, `src/server.rs:1418-1440`, `src/config/storage.rs:82-201`
- **Tests:** `s3_object_storage_repository.rs` tests `new_*`, `zero_retry_window_makes_a_single_attempt`, `retry_window_bounds_how_long_a_failing_store_holds_the_request`

### FILE-040 Byte-store operation semantics and error classification

- **Behavior:**
  - `put(key, bytes, content_type)` stores the object with a ContentType attribute and overwrites any existing key.
  - `get` returns the bytes and the content-type attribute, reading the whole object into memory.
  - `delete` is idempotent: NotFound counts as success.
  - `exists` uses HEAD. It has no production caller.
  - Errors are classified as:
    - NotFound → `ObjectNotFound`.
    - PermissionDenied, Unauthenticated, NotSupported, NotImplemented, InvalidPath, UnknownConfigurationKey, AlreadyExists, Precondition, NotModified → `RequestFailed` (500).
    - Anything else, after the client's own retries → `Unavailable` (503).
  - Keys are opaque strings. The services generate them as `user/<uid>/<fid>` and `workspace/<wid>/<fid>`.
- **Kind:** `guarantee`
- **Trigger:** All storage calls.
- **Source:** `src/repositories/object_storage_repository.rs:24-57`, `src/repositories/s3_object_storage_repository.rs:119-251`
- **Tests:** `s3_object_storage_repository.rs` `map_err_*`; `tests/s3_object_storage_live.rs`

### FILE-041 Storage configuration is validated at startup

- **Behavior:** Invalid storage configuration aborts startup with a `ConfigLoadError`.
  - Always checked:
    - `STORAGE_BACKEND` must be `disabled` or `s3`, case-insensitive, default `disabled`.
    - `STORAGE_MAX_UPLOAD_BYTES` must be greater than 0, whichever backend is set.
    - Numeric and boolean values must parse.
  - Only when `s3`:
    - Bucket, region, key id and secret must be non-empty.
    - The secret must not be the `.env.example` placeholder `replace-with-a-strong-password`.
    - An `http://` endpoint requires `S3_ALLOW_HTTP=true`.
    - Connect timeout 1–30 s. Request timeout 5–300 s and at least the connect timeout.
    - Retry window 0–180 s.
    - Reaper interval greater than 0, batch size greater than 0, max attempts greater than 0, backoff at least 0.
  - `S3_ALLOW_HTTP=true` with s3 adds a startup warning. A feature-status summary line reports the storage mode.
- **Kind:** `rule`
- **Trigger:** Startup (`Config::from_env` / `validate`).
- **Source:** `src/config/environment.rs:213-251`, `src/config/environment.rs:295-307`, `src/config/environment.rs:504-507`, `src/config/environment.rs:541-586`, `src/config/environment.rs:955-963`, `src/config/environment.rs:1039-1162`, `src/config/environment.rs:1590-1593`, `src/config/environment.rs:1710-1726`
- **Tests:** `src/config/environment.rs` storage validation tests (around lines 2070-2130 and 3086)

## Data owned

- **`user_files`** (`migrations/0002_tables.sql:311-334`). Columns: `id` PK, `user_id` NOT NULL FK → `users` ON DELETE CASCADE, `storage_key` UNIQUE, `content_type`, `file_name` (ciphertext), `size_bytes` BIGINT ≥ 0, `visibility` CHECK `private|public` default `private`, `created_at`, `updated_at`. Index `(user_id, created_at DESC)`.
- **`workspace_files`** (`migrations/0002_tables.sql:336-355`). Same columns, with `workspace_id` NOT NULL FK → `workspaces` ON DELETE CASCADE and `uploaded_by` FK → `users` ON DELETE SET NULL. Indexes `(workspace_id, created_at DESC)` and `(uploaded_by)`.
- **`pending_object_deletions`** (`migrations/0002_tables.sql:623-637`). Columns: `id`, `storage_key`, `enqueued_at`, `attempts`, `last_attempt_at`, `last_error`. No FK, no RLS. Claim index `(last_attempt_at NULLS FIRST, enqueued_at)`.
- **Pointer FKs onto these tables:** `users.avatar_file_id` → `user_files(id)` and `workspaces.picture_file_id` → `workspace_files(id)`, both ON DELETE SET NULL, with partial indexes (`migrations/0002_tables.sql:667-681`, `migrations/0003_indexes.sql:114-128`). The columns belong to USER/WS. The database does not require the pointed-to file to share the owner or be public.
- **RLS** (`migrations/0004_row_level_security.sql:45-46`, `306-347`):
  - `user_files_select`: own OR public. `insert`, `update` and `delete`: owner only.
  - `workspace_files_select`: workspace in `get_current_user_workspace_ids()` OR public. `insert`, `update` and `delete`: member or owner.
  - No `FORCE`.
- **Stored procedures:**
  - `sp_public_find_public_user_file`, `sp_public_find_public_workspace_file`, `sp_public_ingest_user_avatar`: SECURITY DEFINER (`migrations/0005_stored_procedures.sql:249-324`).
  - `sp_object_deletion_claim_batch`, `_complete`, `_record_error`, `_count_exhausted`: SECURITY INVOKER (`migrations/0006_background_tasks.sql:434-542`).
  - The `sp_system_re_encrypt_{fetch,update}_{user,workspace}_file(s)` procedures operate on these tables but are run by DATA (`migrations/0005_stored_procedures.sql:2099-2146`).
  - Grants: `migrations/0008_grants.sql:37`, `77-78`, `130-133`, `155-158`, `168`.
- **Trigger:** `enqueue_object_deletion()` AFTER DELETE FOR EACH ROW on both file tables (`migrations/0007_triggers.sql:54-71`).
- **Background task:** `object_deletion_reaper` (FILE-035 to FILE-038).
- **Object-store layout:** `user/<user_id>/<file_id>` and `workspace/<workspace_id>/<file_id>` in one bucket. Each object has a ContentType attribute and nothing else.

## Configuration

| Variable | Default | Validation / notes |
| --- | --- | --- |
| `STORAGE_BACKEND` | `disabled` | `disabled` or `s3`, case-insensitive; otherwise startup fails. |
| `STORAGE_MAX_UPLOAD_BYTES` | `10485760` | > 0 always; no upper bound. Per-route body limit (FILE-005), OAuth avatar cap (FILE-034), idempotency buffer ceiling. |
| `S3_BUCKET` | unset | Required non-empty when s3. |
| `S3_REGION` | `us-east-1` when unset | Required non-empty when s3, so an explicit empty string fails. |
| `S3_ENDPOINT` | unset (AWS default) | `http://` needs `S3_ALLOW_HTTP=true`. |
| `S3_ACCESS_KEY_ID` | unset | Required when s3. |
| `S3_SECRET_ACCESS_KEY` | unset | Required when s3; must not equal `replace-with-a-strong-password`. |
| `S3_ALLOW_HTTP` | `false` | `true`/`false` case-insensitive; `true` adds a startup warning. |
| `S3_FORCE_PATH_STYLE` | `true` | `false` selects virtual-hosted style (AWS). |
| `S3_CONNECT_TIMEOUT_SECS` | `10` | 1–30 (s3 only). |
| `S3_REQUEST_TIMEOUT_SECS` | `30` | 5–300 and ≥ connect timeout (s3 only). |
| `S3_RETRY_TIMEOUT_SECS` | `5` | 0–180 (s3 only). |
| `STORAGE_DELETION_REAPER_INTERVAL_SECS` | `300` | > 0 (s3 only). |
| `STORAGE_DELETION_BATCH_SIZE` | `100` | > 0 (s3 only); i32. |
| `STORAGE_DELETION_RETRY_BACKOFF_SECS` | `600` | ≥ 0 (s3 only); fixed per-row backoff. |
| `STORAGE_DELETION_MAX_ATTEMPTS` | `10` | > 0 (s3 only). |

Read from other domains: `ENABLE_BACKGROUND_CLEANUP` (reaper gate), `BACKGROUND_TASK_LOCK_DURATION_SECS` (reaper lock), `MAX_REQUEST_BODY_BYTES` (global limit and idempotency ceiling), `REQUIRE_EMAIL_VERIFICATION` (gates authenticated file routes), `RATE_LIMIT_PER_USER` / `RATE_LIMIT_PER_IP` / `RATE_LIMIT_WINDOW_SECS` / `ENFORCE_RATE_LIMITS`, `OAUTH_PROVIDER_CONNECT_TIMEOUT_SECS` / `OAUTH_PROVIDER_TOTAL_TIMEOUT_SECS` (avatar fetch). `TEST_S3_*` is test-only (OPS).

Sources: `src/config/storage.rs:160-201`, `src/config/environment.rs:295-307`, `src/config/environment.rs:504-507`, `src/config/environment.rs:541-586`, `src/config/environment.rs:955-963`, `src/config/environment.rs:1039-1162`.

## Cross-domain dependencies

- **Calls into AUTH:** `user_scoped_auth_middleware`, `workspace_scoped_auth_middleware`, `enforce_email_verification_middleware` (`src/server.rs:627-668`, `src/auth/middleware.rs:85-123`); `AuthContext.require_permission(UpdateWorkspace)` for the picture.
- **Calls into HTTP:** `Path`/`Query` extractors, `PaginationQuery` keyset helpers, `ApiError` and response envelope, per-route `DefaultBodyLimit`, rate-limit middleware (per-user and per-IP buckets), idempotency middleware (upload bodies are fingerprinted and responses cached under `Idempotency-Key`).
- **Calls into DATA:** `transaction::with_user_context` / `with_workspace_context` / `with_public_context` (RLS context), `DatabaseEncryption`, `retry_database_operation`, `retry_with_backoff`, `spawn_background_task` plus distributed lock, re-encryption task (rotates `file_name`).
- **Calls into OBS:** tracing spans on every handler, service and repository, plus structured WARN/INFO logs. No file metrics are emitted.
- **Calls into OPS:** bundled SeaweedFS and `seaweedfs-init` bucket creation in `docker-compose.yml`, `.env.example` defaults (`STORAGE_BACKEND=s3`).
- **Called by USER:** `UserService::update_user` (FILE-027/028) and `UserRepository::update_user_and_{swap,clear}_avatar`. Account deletion cascades to `user_files` (FILE-016). `users.avatar_file_id` is exposed through `AuthContext` and user responses.
- **Called by WS:** `WorkspaceService::update_workspace` (FILE-029/030) and `WorkspaceRepository::update_workspace_and_{swap,clear}_picture`. Workspace deletion cascades to `workspace_files`. `picture_file_id` appears in workspace responses.
- **Called by AUTH:** `PublicAuthServiceImpl::ingest_oauth_avatar` via `PublicUserFileRepository::ingest_avatar` (FILE-033).
- **Events and audit:** FILE emits no audit actions and no SSE events itself. Avatar and picture changes are audited and fanned out by USER/WS.

## Observations

1. **Workspace files have no permission gate.** Any member, even one with zero permissions, can upload (including public), list, download and delete every file in the workspace, including other members' private files. That includes the current picture: deleting it clears `picture_file_id`, which bypasses the `update:workspace` permission that `PUT /workspace` requires. Where: `src/services/workspace_file_service.rs:219-239`, `src/handlers/mod.rs:399-407`, `src/services/workspace_service.rs:450`, `src/models/permissions.rs:80-93` (no file permissions exist). Why it matters: a reimplementation that adds permission checks changes the behavior, and one that keeps the code must keep this.
2. **Avatar and picture files are ordinary rows.** They appear in `GET /users/me/files` and `GET /workspace/files`, and can be deleted through the generic DELETE endpoints. Such a delete clears the pointer through the FK, but emits no `profile.updated` or workspace SSE event and no audit entry, unlike the `PUT` remove path. Where: `src/repositories/postgresql/user_file_repository.rs:195-200`, `migrations/0002_tables.sql:672`, `migrations/0002_tables.sql:681`.
3. **Generic file operations leave no audit trail and no SSE.** Upload and delete write no audit entry and publish no SSE event, so other devices or members are not told. Where: `src/services/user_file_service.rs`, `src/services/workspace_file_service.rs` (no audit or SSE dependency in the constructors).
4. **Orphaned objects are never reaped.** Upload writes the object first. A crash before the insert, or a failed best-effort cleanup, leaves bytes with no row, and no reconciliation job ever finds them, because the reaper only knows keys captured by the delete trigger. The OAuth ingest cleanup ignores its result without logging (`src/services/public_auth_service.rs:341`), while the other paths log WARN. Where: `src/services/user_file_service.rs:85-91`, `src/services/user_file_service.rs:119-128`.
5. **A metadata row can be left without bytes.**
   - The failing sequence: attempt 1 of `retry_database_operation` commits but the ACK is lost, then attempts 2 and 3 fail with transient errors.
   - The service then returns an error and deletes the object, although the row is committed.
   - The row stays listed and every download returns 404 with an `orphaned_metadata_row` WARN.
   - `ON CONFLICT (id)` covers only the case where a retry actually reaches the database.
   - Where: `src/services/user_file_service.rs:106-129`, `src/utils/database_retry.rs:153`.
6. **Public files on the authenticated routes break the name-withholding rule.**
   - `GET /files/{user,workspace}/:id` returns `Cache-Control: public, max-age=3600` together with `filename*`.
   - Under RFC 9111 §3.5, a `public` directive allows a shared cache to store an Authorization-bearing response. The handler comment says RFC 9111 already bars this (`src/handlers/files.rs:93-96`), which does not hold for public files.
   - Authenticated non-owners and non-members also receive the original file name of any public file, the PII that the anonymous route withholds (`src/handlers/files.rs:76-78`).
   - Where: `src/handlers/files.rs:132-135`, `src/handlers/files.rs:282-287`.
7. **Any authenticated user can publish arbitrary anonymous content.** Visibility is client-chosen on generic uploads and there is no MIME allow-list, so any content type (HTML, SVG, executables) becomes downloadable from the API origin without auth. The only mitigation is nosniff plus the CSP sandbox. Avatar and picture uploads accept any MIME too, while OAuth ingest requires `image/*`. Where: `src/utils/content_type.rs:23-47`, `src/services/public_auth_service.rs:233`.
8. **OAuth ingest skips the shared content-type validation.** It skips `validate_content_type`: `image/` with an empty subtype passes, length is unbounded, and parameters are stripped. The upload paths store parameters verbatim. Where: `src/services/public_auth_service.rs:227-243`.
9. **Downloads are buffered, not streamed.** Both the S3 `get` (`.bytes()`) and the upload path buffer the whole object. There is no Range, ETag or Last-Modified support, and HEAD fetches the full object. The docs say bytes "stream THROUGH the server" (`documentation/OBJECT_STORAGE.md:4`, `src/services/traits.rs:783`) and suggest `<video>` embedding (`documentation/OBJECT_STORAGE.md:115`), which in practice depends on Range support. Where: `src/repositories/s3_object_storage_repository.rs:184-208`.
10. **Several comments and docs disagree with the code.**
    - `src/models/file.rs:22-23` and `migrations/0002_tables.sql:298-299` say public files are "readable by any authenticated user". The anonymous routes serve them to anyone, which `documentation/OBJECT_STORAGE.md:56-58` describes correctly.
    - `src/repositories/object_storage_repository.rs:37` gives an `avatars/<uuid>` example key; real keys are `user/<uid>/<fid>`.
    - `src/repositories/traits.rs:739-740` calls the swap UPDATE "a full-row overwrite"; it is a column-scoped `COALESCE` (`src/repositories/postgresql/user_repository.rs:427-433`).
    - `src/services/object_deletion_reaper_service.rs:13` and `:90` refer to the reaper spawn "in main.rs"; it is in `src/server.rs:1995`.
    - `migrations/0005_stored_procedures.sql:2102-2104` says the file re-encryption pair "mirrors the workspaces.name rotation pair". The workspaces pair has a compare-and-swap guard (`p_old_name`, line 1930/1936); the file pair does not (lines 2118-2123, 2141-2146). Today this has no effect because nothing rewrites `file_name`.
11. **Dead code.** `ObjectStorageRepository::exists` and `StoredObject.content_type` have no production caller; downloads use the row's `content_type`. Only `tests/s3_object_storage_live.rs` calls `exists`. Where: `src/repositories/object_storage_repository.rs:31-32`, `src/repositories/object_storage_repository.rs:55-56`.
12. **Storage-disabled behavior is uneven.**
    - List and delete still work, and deletes enqueue keys, but no reaper runs (the same holds with `ENABLE_BACKGROUND_CLEANUP=false`), so `pending_object_deletions` grows without bound.
    - Downloads return 503 before checking existence, so a nonexistent id returns 503, not 404.
    - Where: `src/server.rs:786-797`, `src/services/user_file_service.rs:152`.
13. **Exhausted queue rows have no exit.** Rows over the attempt ceiling are never removed. There is no CLI or operator tooling, and no shipped alert rule (nothing for object deletion in `prometheus-alerts.yml` or `loki-rules/`), although the docs say "Alert on that WARN" (`documentation/OBJECT_STORAGE.md:183-184`). The WARN repeats every cycle. Attempts used up by crashes count toward the ceiling.
14. **Reaper write-back failures strand claimed rows.** A `complete()` or `record_error()` DB failure aborts the cycle, and the remaining claimed rows (attempts already incremented) wait a full backoff. `retry_with_backoff` then re-runs the whole cycle and claims a fresh batch. Where: `src/services/object_deletion_reaper_service.rs:67`, `src/services/object_deletion_reaper_service.rs:81`, `src/server.rs:2038-2042`.
15. **Anonymous downloads share the per-IP rate-limit bucket.** They use `ip:<ip>` and `RATE_LIMIT_PER_IP`, the same bucket as login, register and refresh. A page rendering many avatars can use up a client's auth budget, and the reverse. Where: `src/middleware/rate_limit.rs:185-214`.
16. **Keyed oversized uploads can lose their body.** On a request with an `Idempotency-Key` and no declared `Content-Length` (chunked), a body over max(`MAX_REQUEST_BODY_BYTES`, `STORAGE_MAX_UPLOAD_BYTES`) makes the idempotency middleware's `to_bytes` fail. It then passes the request on with an empty body, so an upload gets 400 "File body must not be empty" instead of 413. The declared-length 413 also uses a different message from the extractor's. Where: `src/middleware/idempotency.rs:433-458`.
17. **The workspace update checks permission after reading the whole body.** `PUT /workspace` buffers and parses the full multipart body (up to the upload limit) and validates the name before the service checks `update:workspace`. A member without the permission sends the full upload and gets 400 for a bad name, or 403 otherwise. Where: `src/handlers/workspaces.rs:268-286`, `src/services/workspace_service.rs:450`.
18. **Image pointers rely on code for tenant safety.** The DB does not require `avatar_file_id` or `picture_file_id` to point at a file with the same owner, or a public one. Only the code paths enforce it: the `WHERE id = p_user_id` in `sp_public_ingest_user_avatar`, and the swap SQL. Where: `migrations/0002_tables.sql:672`, `migrations/0002_tables.sql:681`.
19. **Unused UPDATE policies.** The `user_files_update` and `workspace_files_update` RLS policies exist, but no app path updates file rows; visibility and names never change after upload. Where: `migrations/0004_row_level_security.sql:317-320`, `migrations/0004_row_level_security.sql:340-343`.
20. **No quotas.** There is no limit on file count or total bytes per user or workspace. Only the per-request size limit and request rate limits apply.
21. **Repeated multipart parts.** Repeated `avatar`, `picture`, `name` or `remove_*` parts are accepted silently and the last one wins; any earlier image parts are fully buffered and dropped. Where: `src/extractors/multipart.rs:157-214`.
22. **Unreachable check.** The service's `X-Workspace-Id` check in workspace upload (`src/services/workspace_file_service.rs:82-87`) cannot fire behind `workspace_scoped_auth_middleware`.
23. **Readiness ignores storage.** The readiness probe does not check object storage (`src/handlers/health.rs` has no storage reference), so an unreachable store does not take a replica out of rotation.
24. **Most live tests bypass RLS.** Most file tests run through the owner pool, which bypasses RLS: `tests/file_download_http.rs:188` builds the app on `db.pool`, and `tests/file_swap_repository_live.rs` uses it as well. RLS on file paths is covered only by `tests/rls_isolation.rs` and `tests/authenticated_file_repository_live.rs`. There is no test of the workspace-file list query.
25. **The anonymous route still decrypts the file name.** Anonymous public downloads decrypt `file_name` they never send, so a key-ring problem turns public avatar loads into 500s. Where: `src/repositories/postgresql/user_file_repository.rs:370`.

## Open questions

1. Should workspace file upload, delete (especially of other members' files and of the current picture) and private-file reads need specific permissions, or is "any member" intended (Obs. 1)?
2. Should every authenticated user be allowed to publish arbitrary public files with any MIME type? Should avatars and pictures be limited to images (Obs. 7, 8)?
3. Should avatar and picture files be hidden from the generic list and delete endpoints, or should a generic delete of one emit the same audit and SSE as `PUT` remove (Obs. 2)?
4. Is the lack of audit entries and SSE events for file upload and delete intended (Obs. 3)?
5. Is orphan-object accumulation acceptable, or is a reconciliation sweep (store listing versus rows) part of the contract (Obs. 4, 5)?
6. Should the authenticated download of a public file withhold the original name, or be marked private for caching (Obs. 6)?
7. What should operators do with exhausted deletion rows: manual SQL, a CLI, automatic expiry? Is an alert meant to ship (Obs. 13)?
8. Are Range, streaming and conditional requests required, given the docs mention `<video>` embedding (Obs. 9)?
9. Should anonymous file downloads have their own rate-limit bucket (Obs. 15)?
10. With storage disabled, should delete still be allowed (the queue grows), and should a download of a missing id return 404 rather than 503 (Obs. 12)?
11. Is the idempotency middleware's empty-body fallback acceptable on upload routes (Obs. 16)?
12. Should object bytes be encrypted at rest (app-side or S3 SSE), given that the file name is (FILE-024)?
13. Are per-user or per-workspace storage quotas expected for the template (Obs. 20)?
14. Should the OAuth avatar fetch keep following redirects with no host allow-list (FILE-034)?
