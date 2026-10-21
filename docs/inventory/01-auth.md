# Inventory: Authentication, sessions, identities (`AUTH`)

Reference: `AntWorker-API` @ `0210895` (2026-10-20). This file records what the old API does. It does not prescribe what the new one should do.

## Scope

**Covers:** credentials registration and login; OAuth login/registration (Google, GitHub) through a server-side authorization-code exchange; how a login picks the session's workspace; session creation, token formats and lifetimes; bearer-token validation middleware and the AuthContext it builds; token refresh with rotation and reuse detection; logout; session listing, single revoke and "sign out other sessions"; provider-side (RFC 7009) token revocation everywhere a session is torn down; the expired-session cleanup pass; email verification (send/resend, verify), the email-verification gate middleware and the verification-exempt route group; forgot/reset password; identity listing, linking and unlinking (credentials and OAuth); `GET /auth/config`; verification-code generation and validation; password hashing.

**Boundary:**
- USER owns profile, preferences, email change (`POST /users/me/email`, `/users/me/email/confirm`, whose handlers live in `src/handlers/auth.rs:1095-1178`), password change and account deletion. They share AUTH primitives (`validate_verification_code`, `revocation::*`, the `password_change` rate-limit bucket); those primitives are catalogued here, their callers there.
- WS owns workspace membership semantics and permission strings; AUTH only reads them when building an AuthContext or an auth response.
- HTTP owns `ValidatedJson`, the generic per-IP/per-user rate-limit middleware, idempotency, the response envelope and pagination. Only their auth-specific effects are noted here.
- DATA owns the background-task runner, distributed locks, transactions helpers and `DatabaseEncryption`. The body of the `auth_cleanup` job's session pass is catalogued here (AUTH-035) because it is session-lifecycle and revocation logic; its scheduling is DATA's.
- RT owns SSE delivery; MAIL owns templates and SMTP; AUDIT owns audit storage. Their effects are listed under Effects.

**What the `public_*` repositories are:** repositories whose methods need no AuthContext. Each wraps a `SECURITY DEFINER` stored procedure named `sp_public_*` (`migrations/0005_stored_procedures.sql:1-33`) that bypasses RLS, because pre-authentication flows (login, registration, refresh, password reset) have no `app.current_user_id` to set, and RLS would hide every row. They are also used by authenticated flows for (a) cross-account lookups RLS would hide (OAuth-link takeover guards, `identity_service.rs:57-61`) and (b) atomic multi-table operations (`sp_public_unlink_*`, `sp_public_change_password`). The `sp_public_` prefix marks the privilege model, not HTTP exposure (`0005_stored_procedures.sql:1309-1312`). Encryption and HMAC hashing happen at the repository boundary (`public_user_repository.rs:21-40`, `public_session_repository.rs:44-77`, `public_oauth_token_repository.rs:34-54`).

**Files read:** `src/auth/*` (all 11), `src/handlers/{auth,identities,sessions,mod}.rs`, `src/services/{public_auth_service,identity_service,session_service}.rs` (including inline tests), relevant parts of `src/services/{traits,user_service,rate_limit_service,sse_event_publisher}.rs`, `src/models/{auth,identity,session,oauth_token,verification_code}.rs`, `src/errors/{auth_error,oauth_error}.rs`, relevant parts of `src/errors/{api_error,response_envelope,repository_error}.rs`, `src/repositories/postgresql/{public_session_repository,session_repository,public_verification_code_repository,public_oauth_token_repository,identity_repository,public_user_repository}.rs`, relevant parts of `src/repositories/traits.rs`, `src/repositories/postgresql/{user_repository,transaction,user_file_repository}.rs`, `src/server.rs:495-741,1567-1648`, `src/middleware/{rate_limit,idempotency}.rs` (relevant parts), `src/config/environment.rs` (auth parts), `src/validation/mod.rs`, `src/utils/deserializers.rs`, `src/handlers/sse_notifications.rs`, `migrations/0002,0003,0004,0005,0006` (auth parts). Tests: all files listed in the brief plus `tests/rate_limit_middleware_http.rs`, `tests/router_composition.rs`, `tests/idempotency_middleware_http.rs` (headers only). Docs: `README.md`, `documentation/*` (no auth claims found beyond "sessions are Postgres-backed", `MULTI_INSTANCE.md:184`). `.env` and `alertmanager-secrets/` exist and were not opened.

**Wire conventions used below:** all routes are under `/v1`. Success is HTTP 200 with `{data, meta}` (`response_envelope.rs:158-166`); "empty success" is `data: {}` (`response_envelope.rs:215-217`). Errors carry an envelope `code` in SCREAMING_SNAKE_CASE (`response_envelope.rs:242-276`); the finer `AuthErrorCode` (snake_case) goes to logs only, so clients distinguish auth failures by `message` and `details.reason` (`api_error.rs:914-973`).

## Behaviors

### AUTH-001 Register with email and password

- **Behavior:** Creates a user, a `credentials` identity and a preferences row, opens a session, and returns tokens. The new user has `email_verified=false`, no workspace and no permissions.
- **Kind:** `capability`
- **Trigger:** `POST /auth/register`
- **Auth:** public. Per-IP rate limit `ip:<addr>` (`RATE_LIMIT_PER_IP`, default 20 per 60 s) and public-scoped idempotency apply (`server.rs:593-603`). No registration-specific limit.
- **Input & validation:** `email` trimmed and lowercased, 1-254 chars, email format. `password` 8-128 chars, not in the 2,085-entry common list (case-insensitive), not one repeated character, not an all-digit run of 8+ ascending or descending digits (`validation/mod.rs:605-645`); not trimmed. `name` trimmed, internal whitespace collapsed, NFC-normalized, 1-100 chars, not whitespace-only. `device_fingerprint` trimmed, exactly 64 hex chars. `device_name` optional, normalized, 1-100 chars, not whitespace-only (`models/auth.rs:406-459`).
- **Effects:** Argon2id hash (AUTH-055). One transaction (no RLS context): `sp_public_create_user` (email and name AES-256-GCM encrypted, `email_hash` = HMAC-SHA256 of the normalized email), `sp_public_insert_identity` (method `credentials`, hash, NULL subject), `user_preferences` row (`default_workspace_id` NULL, `telemetry_consent` false), audit `auth.register` `{auth_method:"credentials"}` written in the same transaction. Whole transaction retried on transient DB errors. Metric business event `user_signup`. Then AUTH-003, then a session (AUTH-016, AUTH-017) that publishes `session.created` (AUTH-018). No `auth.login` audit. Response body: `AuthResponse {session_id, user{id,email,name,avatar_file_id,email_verified,telemetry_consent:false}, workspace:null, permissions:[], access_token, refresh_token, expires_at, refresh_token_expires_at, auth_method:"credentials"}` (`handlers/auth.rs:28-38,360-372`).
- **Errors:** validation → 400 `VALIDATION_ERROR`; existing email → AUTH-002; a racing duplicate that passes the pre-check fails `users_email_hash_unique` → 409 `RESOURCE_CONFLICT` "Resource already exists" (`api_error.rs:747-752`).
- **Guarantees:** user, identity, preferences and audit commit atomically (the `user_identities.user_id` FK is deferred so insert order inside the transaction does not matter, `0002_tables.sql:648`).
- **Source:** `src/handlers/auth.rs:335-375`, `src/services/public_auth_service.rs:597-693,1069-1123`, `src/repositories/postgresql/public_user_repository.rs:397-473`, `migrations/0005_stored_procedures.sql:194-244`
- **Tests:** `tests/auth_session_fanout_live.rs` (`registration_fans_out_session_created`, `registration_succeeds_when_sse_publish_fails`); used as setup in `tests/email_verification_middleware_http.rs`, `tests/identities_unlink_http.rs`, `tests/login_workspace_resolution_live.rs`.

### AUTH-002 Registration with an existing email is rejected with the generic credentials error

- **Behavior:** If any account (credentials or OAuth-only) already has the email, registration runs a dummy Argon2 verify and returns the same 401 as a failed login.
- **Kind:** `rule`
- **Trigger:** `POST /auth/register`
- **Auth:** public
- **Errors:** `AUTHENTICATION_FAILED` → 401 → message "Invalid email or password".
- **Guarantees:** response time is close to the success path's, which also runs Argon2. The response itself still differs from success (see Observations).
- **Source:** `src/services/public_auth_service.rs:1075-1086`, `src/errors/auth_error.rs:480-486`
- **Tests:** inline `public_auth_service.rs::register_credentials_existing_email_returns_invalid_credentials`

### AUTH-003 Registration sends a verification code only when verification is required

- **Behavior:** When `REQUIRE_EMAIL_VERIFICATION=true`, registration calls AUTH-031's send step after the user commits. A send failure is swallowed (logged at debug level) and registration still succeeds.
- **Kind:** `rule`
- **Trigger:** `POST /auth/register`
- **Auth:** public
- **Effects:** see AUTH-031 (verification code row, `email_verification` email).
- **Source:** `src/services/public_auth_service.rs:1110-1120`
- **Tests:** none found

### AUTH-004 Log in with email and password

- **Behavior:** Verifies the password against the user's `credentials` identity, resolves the session's workspace (AUTH-014), opens a session and returns tokens plus workspace and permissions. Login does not check `email_verified`.
- **Kind:** `capability`
- **Trigger:** `POST /auth/login`
- **Auth:** public; per-IP limit plus AUTH-006; public-scoped idempotency.
- **Input & validation:** `email` as AUTH-001; `password` 8-128 chars only (no common-password check); `device_fingerprint`, `device_name` as AUTH-001 (`models/auth.rs:462-501`).
- **Effects:** lookup by email HMAC over all key versions with a LEFT JOIN on the credentials identity (`sp_public_find_user_by_email_with_credentials_hash`). Workspace resolution (may write preferences, AUTH-014). Audit `auth.login` `{auth_method:"credentials"}`, best-effort, written after the resolution transaction and before the session is created. Session per AUTH-016. Permissions loaded for the resolved workspace; `telemetry_consent` read from preferences (missing row → false). Retried on transient DB errors. Response body: `AuthResponse` with `workspace {id,name,picture_file_id,is_owner}` or null and `permissions` strings (`handlers/auth.rs:128-165`).
- **Errors:** AUTH-005 failures → 401; AUTH-006 → 429; malformed stored hash → 500 `INTERNAL_SERVER_ERROR`; decrypted email not equal to the lookup input → 500 `DATA_INTEGRITY_ERROR` (`public_user_repository.rs:213-220`).
- **Source:** `src/handlers/auth.rs:387-482`, `src/services/public_auth_service.rs:390-473,1135-1193`, `src/repositories/postgresql/public_user_repository.rs:188-236`, `migrations/0005_stored_procedures.sql:88-112`
- **Tests:** `tests/login_workspace_resolution_live.rs`; `tests/public_user_repository_live.rs::find_by_email_with_credentials_hash_returns_the_credentials_identity_hash`; setup in several HTTP tests.

### AUTH-005 Credentials login failures are uniform in body and timing

- **Behavior:** Unknown email, an OAuth-only account (no credentials identity), and a wrong password all return the same 401. The first two run Argon2 against a dummy hash computed once at startup with the same parameters, so all three take comparable time.
- **Kind:** `guarantee`
- **Trigger:** `POST /auth/login`
- **Auth:** public
- **Errors:** `AUTHENTICATION_FAILED` → 401 → "Invalid email or password".
- **Guarantees:** anti-enumeration by response body and by timing on the login path.
- **Source:** `src/services/public_auth_service.rs:90-102,142-143,1148-1178`
- **Tests:** inline `public_auth_service.rs::authenticate_credentials_unknown_email_returns_invalid_credentials`, `authenticate_credentials_oauth_user_returns_invalid_credentials`, `authenticate_credentials_wrong_password_returns_invalid_credentials`

### AUTH-006 Per-email login rate limit

- **Behavior:** Every login attempt (successful or not) for an email increments the bucket `credential_login:<HMAC(normalized email)>`. Over the limit, with enforcement on, the request is rejected before any user lookup.
- **Kind:** `rule`
- **Trigger:** `POST /auth/login`
- **Auth:** public
- **Input & validation:** limit `CREDENTIAL_LOGIN_RATE_LIMIT_MAX_ATTEMPTS` (default 5) per `CREDENTIAL_LOGIN_RATE_LIMIT_WINDOW_SECS` (default 900).
- **Effects:** `rate_limits` row keyed by HMAC, so the raw email is never stored (`rate_limit_service.rs:293-310`). Limiter errors fail open with a `failing_open` warn log.
- **Errors:** `RATE_LIMIT_EXCEEDED` → 429 → "Too many login attempts. Try again in N seconds", with `X-RateLimit-Limit/Remaining/Reset` and `Retry-After` headers (`api_error.rs:1042-1080`). When `ENFORCE_RATE_LIMITS=false` the request proceeds.
- **Source:** `src/handlers/auth.rs:393-445`
- **Tests:** `tests/rate_limit_middleware_http.rs::credential_login_email_lockout_returns_429_after_max_attempts`

### AUTH-007 OAuth login via server-side authorization-code exchange

- **Behavior:** The client sends the provider authorization code plus its PKCE verifier and redirect URI. The server exchanges the code itself, runs the security gates (AUTH-008), finds or creates the user (AUTH-009 to AUTH-011), opens a session and returns tokens. The provider access token is never returned to the client.
- **Kind:** `capability`
- **Trigger:** `POST /auth/oauth`
- **Auth:** public; per-IP limit; public-scoped idempotency. No per-email or per-provider limit.
- **Input & validation:** `auth_method` trimmed and lowercased, 1-50 chars; `auth_code` trimmed, 1-2048; `code_verifier` trimmed, 43-128; `redirect_uri` trimmed, 1-2048; `device_fingerprint`, `device_name` as AUTH-001 (`models/auth.rs:348-403`).
- **Effects:** provider lookup (AUTH-050). Google: form POST to `https://oauth2.googleapis.com/token` with `client_id, code, grant_type=authorization_code, code_verifier, redirect_uri` and no client secret; userinfo from `https://www.googleapis.com/oauth2/v2/userinfo`; subject = `id`. GitHub: form POST to `https://github.com/login/oauth/access_token` with `client_id, client_secret, code, redirect_uri, code_verifier` and `Accept: application/json`; a 200 body containing `error` is treated as failure; then always both `GET https://api.github.com/user` and `/user/emails` with `User-Agent: APP_DISPLAY_NAME`; subject = numeric `id` as a string; email = the entry flagged `primary`; name = `name` or else `login`; picture = `avatar_url`. Provider email is normalized (trim and lowercase) before use. Then a session (AUTH-016 to AUTH-018) and provider-token storage (AUTH-013). The response is `AuthResponse` with `auth_method` set to the provider id.
- **Errors:** see AUTH-008 and AUTH-050.
- **Guarantees:** outbound provider HTTP has a connect timeout (`OAUTH_PROVIDER_CONNECT_TIMEOUT_SECS`, default 10) and a total timeout (`OAUTH_PROVIDER_TOTAL_TIMEOUT_SECS`, default 30) (`oauth_provider.rs:39-51`). Audience binding is implicit because the server exchanged the code with its own client id.
- **Source:** `src/handlers/auth.rs:216-323`, `src/auth/oauth_provider.rs:380-400`, `src/auth/google_oauth.rs:108-200`, `src/auth/github_oauth.rs:100-121,211-354`
- **Tests:** `tests/oauth_login_http.rs::oauth_login_registers_then_logs_in_through_the_real_stack` (mock provider); inline `google_oauth.rs::token_response_splits_space_separated_scopes`, `github_oauth.rs::*` (scope split, error envelope, primary-email selection, name fallback, no-primary error).

### AUTH-008 OAuth security gates and provider-error mapping

- **Behavior:** After the exchange, every scope the deployment requests must appear in the granted set, checked before user info is fetched. After the fetch, the provider must report the email as verified. Provider failures map to fixed HTTP statuses.
- **Kind:** `guarantee`
- **Trigger:** `POST /auth/oauth`, `POST /auth/identities/oauth/complete`
- **Auth:** public / session respectively
- **Input & validation:** required scopes: Google `openid, email, profile`; GitHub `read:user, user:email`. A required scope matches a granted scope that equals it or ends in `.<scope>` or `/<scope>`. Google scopes are split on whitespace, GitHub on commas, with empty entries dropped.
- **Errors:**
  - missing scope → `PERMISSION_DENIED` 403 "Required scope 'X' not granted by user", details `{required_scope}`.
  - provider says email unverified → `PERMISSION_DENIED` 403 "Email not verified by OAuth provider", no details.
  - provider 4xx (including 429), or GitHub 200 with an `error` body → `AUTHENTICATION_FAILED` 401 "Invalid or expired OAuth token", details `{upstream_status}` when an HTTP status existed.
  - provider 5xx, network failure, timeout, unparseable body, or GitHub with no primary email → `SERVICE_UNAVAILABLE` 502 "OAuth provider 'X' temporarily unavailable", no details.
- **Guarantees:** gates are a free function that provider implementations cannot override; provider response bodies are never logged or returned.
- **Source:** `src/auth/oauth_provider.rs:126-145,380-400`, `src/errors/oauth_error.rs:282-313,373-389`, `src/errors/api_error.rs:804-866`, `src/auth/google_oauth.rs:49-51,76-80`, `src/auth/github_oauth.rs:39-45,175,249-255`
- **Tests:** inline `oauth_provider.rs::scope_coverage_*`, `orchestrator_*`; `api_error.rs` OAuth mapping tests (lines 1585-1680).

### AUTH-009 OAuth users are matched by (provider, subject), not by email

- **Behavior:** An existing `user_identities` row with `(method, provider_subject)` logs that user in, whatever email the provider now reports. The login resolves the workspace (AUTH-014) and writes audit `auth.login` `{auth_method:<provider>}`.
- **Kind:** `rule`
- **Trigger:** `POST /auth/oauth`
- **Auth:** public
- **Effects:** as AUTH-004 minus the password step. Retried on transient DB errors.
- **Source:** `src/services/public_auth_service.rs:977-1013`, `src/repositories/postgresql/public_user_repository.rs:255-292`, `migrations/0005_stored_procedures.sql:124-146`
- **Tests:** `tests/oauth_login_http.rs` (second call logs in the same user, no duplicate identity); `tests/oauth_registration_tx_live.rs::oauth_registration_persists_user_identity_and_preferences_then_matches_by_subject`; `tests/public_user_repository_live.rs::find_user_by_oauth_identity_is_scoped_to_method_and_subject`

### AUTH-010 OAuth login with a new identity registers a new user

- **Behavior:** With no identity row and no account holding the email, a user is created with `email_verified=true`, the name from the provider (or the email local part if none), no avatar yet, an identity row for the provider subject, and a preferences row. No workspace.
- **Kind:** `capability`
- **Trigger:** `POST /auth/oauth`
- **Auth:** public
- **Effects:** one transaction: user, identity (`password_hash` NULL), preferences (default workspace NULL, telemetry false), audit `auth.register` `{auth_method:<provider>}` written in the transaction. After commit: AUTH-012 avatar ingestion, metric `user_signup`. Retried on transient DB errors. The provider name and email are not length-checked or name-normalized.
- **Errors:** a unique-constraint failure inside the transaction rolls everything back and surfaces as 409 `RESOURCE_CONFLICT`.
- **Guarantees:** atomic registration; a subject collision leaves no orphan user.
- **Source:** `src/services/public_auth_service.rs:477-594,1014-1051`
- **Tests:** `tests/oauth_registration_tx_live.rs` (all 3 tests, including rollback on subject collision and deferred-FK ordering); `tests/oauth_login_http.rs`

### AUTH-011 OAuth sign-in cannot claim an email owned by another account

- **Behavior:** If no identity matches but the normalized provider email already belongs to any account, the login is refused. Accounts are never auto-merged; the user must sign in another way and link the provider (AUTH-046).
- **Kind:** `rule`
- **Trigger:** `POST /auth/oauth`
- **Auth:** public
- **Errors:** `AUTHENTICATION_FAILED` → 401 → "Authentication failed for this account".
- **Source:** `src/services/public_auth_service.rs:1014-1033`
- **Tests:** inline `public_auth_service.rs::authenticate_oauth_with_credentials_account_returns_invalid_credentials`

### AUTH-012 OAuth signup copies the provider avatar into object storage

- **Behavior:** After a new OAuth user commits, the server fetches the provider picture URL, stores the bytes as a public user file, and sets `users.avatar_file_id`. Any failure leaves the user with no avatar and does not fail the signup. The returned `avatar_file_id` reflects the outcome.
- **Kind:** `rule` (a side effect of signup, not something the client invokes)
- **Trigger:** `POST /auth/oauth` (new-user branch only)
- **Auth:** public
- **Input & validation:** skipped when storage is disabled. Requires an HTTP success status and a `Content-Type` starting with `image/` (parameters stripped). Rejected if the declared `Content-Length` or the streamed size exceeds `storage_max_upload_bytes`, or if the body is empty. Redirects are followed; same timeouts as AUTH-007.
- **Effects:** object put at `user/<user_id>/<file_id>`; then `sp_public_ingest_user_avatar` inserts a `user_files` row (visibility `public`, file name "avatar", encrypted) and sets `avatar_file_id` atomically. If the metadata write fails, the object is deleted. Each failure logs a `failing_open` warn.
- **Guarantees:** never strands a public object without metadata; never fails registration.
- **Source:** `src/services/public_auth_service.rs:198-354,578-584`, `migrations/0005_stored_procedures.sql:255-270`
- **Tests:** inline `public_auth_service.rs::ingest_oauth_avatar_*` (9 tests)

### AUTH-013 Provider access token is stored encrypted per session, for revocation only

- **Behavior:** After an OAuth login opens a session, the provider access token is stored AES-256-GCM-encrypted in `oauth_tokens`, one row per session, with `provider_token_expires_at = now + expires_in` (NULL when the provider gives no lifetime; GitHub is always NULL). Storage failure does not fail the login. No provider refresh token is captured.
- **Kind:** `guarantee`
- **Trigger:** `POST /auth/oauth`
- **Auth:** public
- **Effects:** `sp_public_upsert_oauth_tokens`; on a conflict on `session_id` it updates only the token, expiry and `updated_at`, not `oauth_provider`. Failure logs a `failing_open` warn (`provider_token_storage_failed`).
- **Guarantees:** the token is never sent to the client; plaintext exists only past the repository boundary.
- **Source:** `src/handlers/auth.rs:265-303`, `src/services/public_auth_service.rs:1329-1355`, `src/repositories/postgresql/public_oauth_token_repository.rs:75-96`, `migrations/0005_stored_procedures.sql:876-900`, `migrations/0002_tables.sql:144-158`
- **Tests:** `tests/oauth_token_repository_live.rs` (read paths)

### AUTH-014 Login lands in the stored default workspace, with fallback when access was lost

- **Behavior:** On credentials or OAuth login of an existing user:
  - No `default_workspace_id` stored → no workspace, even if the user has memberships.
  - Default stored and the user is still a member → that workspace, nothing written.
  - Default stored but no longer a member → the accessible workspace with the oldest `workspaces.created_at` becomes the new default and is returned. If none remain, the default is cleared and no workspace is returned.
- **Kind:** `rule`
- **Trigger:** `POST /auth/login`, `POST /auth/oauth` (existing user)
- **Auth:** public
- **Effects:** possible `user_preferences.default_workspace_id` update or clear, inside a no-RLS transaction (`sp_public_update_user_preferences`, `sp_public_clear_user_default_workspace`). New users (AUTH-001, AUTH-010) always get no workspace.
- **Source:** `src/services/public_auth_service.rs:702-749`, `migrations/0005_stored_procedures.sql:393-425,716-789`
- **Tests:** `tests/login_workspace_resolution_live.rs` (all 3 tests)

### AUTH-015 Refresh, validate-session and verify-email resolve the workspace without fallback

- **Behavior:** These endpoints return the stored default workspace only if the user is still a member. Otherwise they return no workspace, with no fallback and no preference write. Cleanup is left to the next login. `is_owner` is `workspace.owner_id == user.id`.
- **Kind:** `rule`
- **Trigger:** `POST /auth/refresh`, `POST /auth/validate-session`, `POST /auth/verify-email` (also USER's email-change confirm)
- **Auth:** varies
- **Source:** `src/handlers/auth.rs:175-199,543-554,610-622,751-761`
- **Tests:** none found

### AUTH-016 Session creation issues an opaque token pair and stores only HMACs

- **Behavior:** Every login and registration generates two independent 32-byte random tokens (`ring::SystemRandom`), base64url without padding (43 chars). They are not JWTs. Only `HMAC-SHA256(active key)` of each token (64 hex chars) is stored. Access tokens expire after `ACCESS_TOKEN_LIFETIME_SECS` (default 3600), refresh tokens after `REFRESH_TOKEN_LIFETIME_SECS` (default 2,592,000 = 30 days). Plaintext tokens are returned exactly once, in the response.
- **Kind:** `guarantee`
- **Trigger:** `POST /auth/register`, `/auth/login`, `/auth/oauth`
- **Auth:** public
- **Effects:** `sessions` row via `sp_public_upsert_session` (see AUTH-017): `access_token_hash`, `refresh_token_hash`, `previous_refresh_token_hash` NULL, `auth_method`, `device_fingerprint` (plaintext), `device_name` (encrypted), `created_at`, `expires_at` (access), `refresh_token_expires_at`. Then AUTH-018.
- **Guarantees:** a database dump does not reveal usable tokens (HMAC key needed). Both hash columns are UNIQUE. Lookups accept the hash under every key version (`hmac_for_search_all_versions`) so key rotation does not log users out.
- **Source:** `src/auth/token_generator.rs:38-104`, `src/services/public_auth_service.rs:1212-1309`, `src/repositories/postgresql/public_session_repository.rs:264-300`, `migrations/0002_tables.sql:91-133`
- **Tests:** inline `token_generator.rs::*` (5 tests)

### AUTH-017 One session per user per device; logging in again replaces it

- **Behavior:** `sessions` is unique on `(user_id, device_fingerprint)`. A new login from the same user and fingerprint overwrites the row in place: new `id`, new hashes, `previous_refresh_token_hash` reset to NULL, new `auth_method`, `device_name`, `created_at` and expiries. The old tokens and the old session id stop working immediately.
- **Kind:** `rule`
- **Trigger:** `POST /auth/register`, `/auth/login`, `/auth/oauth`
- **Auth:** public
- **Effects:** `ON CONFLICT ... DO UPDATE SET id = EXCLUDED.id, ...`. The `oauth_tokens.session_id` FK is `ON UPDATE CASCADE`, so an existing provider-token row follows the new id. No `session.revoked` event is emitted for the replaced id; only `session.created` (AUTH-018).
- **Source:** `migrations/0005_stored_procedures.sql:464-521`, `migrations/0002_tables.sql:121-123,654`
- **Tests:** `tests/oauth_login_http.rs` (same fingerprint twice, second token differs)

### AUTH-018 New sessions are announced to the user's other devices

- **Behavior:** After a session commits, a `session.created` user event is published with `{session_id, device_name, auth_method, created_at}`. Token refresh does not publish. Publish failure does not fail the login.
- **Kind:** `rule`
- **Trigger:** session creation in AUTH-001/004/007
- **Auth:** public
- **Effects:** SSE `session.created` (an `sse_events` row, which fans out to all instances via `pg_notify`, owned by RT). Skipped when SSE is disabled.
- **Source:** `src/services/public_auth_service.rs:1253-1301`, `src/services/sse_event_publisher.rs:93-110`
- **Tests:** `tests/auth_session_fanout_live.rs::registration_fans_out_session_created`, `registration_succeeds_when_sse_publish_fails`

### AUTH-019 Bearer-token session validation on every protected request

- **Behavior:** Protected routes require `Authorization: Bearer <access token>`. The token's HMAC under every key version is looked up in `sessions`. If found and `expires_at` has not passed, the request proceeds with that session. Validation hits Postgres on every request; there is no in-memory session cache.
- **Kind:** `guarantee`
- **Trigger:** every route in the verification-exempt, user-scoped, workspace-scoped and telemetry groups (`server.rs:608-687`)
- **Auth:** session
- **Input & validation:** the header must be ASCII-representable and start with exactly `Bearer ` (case-sensitive, one space). The token itself is not length-checked.
- **Errors:** all `AUTHENTICATION_FAILED` 401:
  - no header → "Missing Authorization header".
  - non-ASCII header → "Invalid Authorization header format".
  - no `Bearer ` prefix → "Invalid Authorization header - missing Bearer prefix".
  - unknown token → "Invalid or expired access token".
  - token found but expired → "Session expired", details `{session_id, expires_at}`.
  - session's user missing → "User not found".
- **Guarantees:** a session deleted on any instance is rejected by every instance at its next request. Expired-but-uncleaned rows are rejected by the expiry check.
- **Source:** `src/auth/middleware.rs:27-66,285-313`, `src/services/public_auth_service.rs:1417-1422`, `src/repositories/postgresql/public_session_repository.rs:97-130`, `src/errors/repository_error.rs:731-745`, `src/errors/api_error.rs:703-708`, `migrations/0005_stored_procedures.sql:432-457`
- **Tests:** `tests/session_repository_live.rs::find_by_access_token_rejects_expired_and_accepts_live`; `tests/router_composition.rs` (every protected group has an auth layer, auth outermost)

### AUTH-020 User-scoped AuthContext contents

- **Behavior:** For user-scoped, verification-exempt and telemetry routes the middleware builds an AuthContext from the session plus a fresh read of the user row: `user_id, user_email, user_name, avatar_file_id, email_verified`, `telemetry_consent` and `workspace_id` from the preferences row (missing row → false / None, and `workspace_id` is not checked against membership), empty `permissions`, `expires_at` (access expiry), `auth_method` (from the session) and `session_id`.
- **Kind:** `rule`
- **Trigger:** user-scoped route groups
- **Auth:** session
- **Source:** `src/auth/middleware.rs:137-185`, `src/models/auth.rs:14-53`
- **Tests:** indirectly via `tests/email_verification_middleware_http.rs`

### AUTH-021 Workspace-scoped AuthContext requires X-Workspace-Id and membership

- **Behavior:** For `/workspace/*` routes the middleware validates the session first, then requires an `X-Workspace-Id` UUID header naming a workspace where the user has a membership row. It loads that membership's permission strings into `permissions`. `workspace_id` is the header value; `telemetry_consent` is always false.
- **Kind:** `rule`
- **Trigger:** workspace-scoped route group
- **Auth:** workspace-scoped
- **Errors:**
  - header missing → `VALIDATION_ERROR` 400 "Missing X-Workspace-Id header".
  - not a string → 400 "Invalid X-Workspace-Id header format".
  - not a UUID → 400 "Invalid workspace ID format".
  - not a member → `PERMISSION_DENIED` 403 "Workspace access unavailable", details `{reason:"workspace_access_revoked", workspace_id}`.
  - unknown permission string → error from `Permission::from_strings` (WS).
  - A missing or invalid token yields 401 before any header check.
- **Source:** `src/auth/middleware.rs:193-278`, `src/errors/auth_error.rs:531-543`
- **Tests:** `tests/rate_limit_middleware_http.rs::workspace_scoped_route_rejects_malformed_workspace_id_header`, `workspace_scoped_route_rejects_missing_workspace_id_header`

### AUTH-022 Refresh tokens rotate on every use

- **Behavior:** Presenting the current refresh token issues a new access and refresh pair. The old pair stops working, the old refresh hash moves into `previous_refresh_token_hash`, and both expiries are set from now. Because the refresh expiry is renewed each time, the session lives as long as it is refreshed at least once per refresh lifetime.
- **Kind:** `capability`
- **Trigger:** `POST /auth/refresh`
- **Auth:** public (the refresh token is the credential); per-IP limit; public-scoped idempotency (a retry with the same `Idempotency-Key` and body replays the cached response, including its tokens, for 24 h).
- **Input & validation:** `refresh_token` trimmed, 1-200 chars.
- **Effects:** `sp_public_update_session_tokens` as a compare-and-swap on the old refresh hash. Then the user is re-read, preferences read, workspace resolved per AUTH-015, permissions loaded. No audit, no SSE. Response body: full `AuthResponse` with the new tokens and the session's `auth_method`.
- **Errors:** user deleted after rotation → `AUTHENTICATION_FAILED` 401 "User not found"; see also AUTH-023 to AUTH-025.
- **Source:** `src/handlers/auth.rs:584-638`, `src/services/public_auth_service.rs:764-816,1368-1383`, `src/repositories/postgresql/public_session_repository.rs:148-172,321-357`, `migrations/0005_stored_procedures.sql:538-595`
- **Tests:** inline `public_auth_service.rs::refresh_session_tokens_happy_path_rotates`; `tests/session_repository_live.rs::update_session_tokens_cas_rotates_captures_previous_and_rejects_stale`, `find_by_refresh_token_is_scoped_to_the_matching_hash`

### AUTH-023 Expired refresh tokens are rejected even before cleanup runs

- **Behavior:** A current refresh token whose `refresh_token_expires_at <= now` is rejected without rotating. The row is not deleted; cleanup (AUTH-035) removes it later.
- **Kind:** `rule`
- **Trigger:** `POST /auth/refresh`
- **Auth:** public
- **Errors:** `AUTHENTICATION_FAILED` 401 "Refresh token expired - re-authentication required".
- **Source:** `src/services/public_auth_service.rs:1378-1381`
- **Tests:** inline `public_auth_service.rs::refresh_session_tokens_expired_refresh_returns_expired`

### AUTH-024 Reusing the previous refresh token terminates the session

- **Behavior:** If the presented token matches no current refresh hash but does match some session's `previous_refresh_token_hash` (the token rotated out one step earlier), the session is deleted. Its provider tokens are revoked first, for non-credentials sessions. The request is denied. Tokens two or more rotations old match nothing and get a plain 401 without termination.
- **Kind:** `guarantee`
- **Trigger:** `POST /auth/refresh`
- **Auth:** public
- **Effects:** AUTH-033 per-session revocation (skipped when `auth_method == "credentials"`). `sp_public_terminate_session_for_reuse` DELETE, retried on transient DB errors; a DELETE that still fails is logged as error `session_termination_failed` and the 401 is returned anyway. Warn log "Refresh token reuse detected". Audit `session.reuse_detected` (resource = session id, `{auth_method}`), best-effort, recorded once even if the DELETE was retried. No SSE event.
- **Errors:** `AUTHENTICATION_FAILED` 401 "Invalid or expired refresh token" (same as an unknown token).
- **Source:** `src/services/public_auth_service.rs:850-951,1385-1400`, `src/repositories/postgresql/public_session_repository.rs:188-244`, `migrations/0005_stored_procedures.sql:613-664`, `migrations/0003_indexes.sql:43-45`
- **Tests:** inline `public_auth_service.rs::refresh_session_tokens_reuse_*` (7 tests), `refresh_session_tokens_unknown_token_returns_invalid`; `tests/session_repository_live.rs::find_by_previous_refresh_token_is_scoped_to_the_matching_hash`

### AUTH-025 A lost compare-and-swap during concurrent refresh is a 401, not a 500

- **Behavior:** If two refreshes with the same token race, the loser's update matches zero rows and gets the invalid-refresh 401. Its session is not terminated by that request.
- **Kind:** `hardening`
- **Trigger:** `POST /auth/refresh`
- **Auth:** public
- **Errors:** `AUTHENTICATION_FAILED` 401 "Invalid or expired refresh token".
- **Source:** `src/services/public_auth_service.rs:786-795`, `src/repositories/postgresql/public_session_repository.rs:349-354`
- **Tests:** inline `public_auth_service.rs::refresh_session_tokens_cas_miss_returns_invalid_refresh`

### AUTH-026 Validate the current session and get fresh context

- **Behavior:** Returns the current session context without issuing tokens: `{session_id, user{id,email,name,avatar_file_id,email_verified,telemetry_consent}, workspace|null, permissions, expires_at, auth_method}`. Reachable by unverified users.
- **Kind:** `capability`
- **Trigger:** `POST /auth/validate-session`
- **Auth:** session (verification-exempt group); per-user limit `user:<id>` (`RATE_LIMIT_PER_USER`, default 100 per 60 s); user-scoped idempotency.
- **Effects:** workspace per AUTH-015; permissions for that workspace.
- **Source:** `src/handlers/auth.rs:49-57,535-573`, `src/handlers/mod.rs:107`
- **Tests:** none found

### AUTH-027 Log out the current session

- **Behavior:** Deletes the caller's own session, revoking its provider token first, and returns empty success.
- **Kind:** `capability`
- **Trigger:** `POST /auth/logout`
- **Auth:** session (verification-exempt group); per-user limit; idempotency.
- **Effects:** identical to AUTH-029 with the session id taken from the AuthContext.
- **Source:** `src/handlers/auth.rs:502-514`
- **Tests:** `tests/email_verification_middleware_http.rs::unverified_user_reaches_verification_exempt_route_when_verification_required`

### AUTH-028 List my sessions

- **Behavior:** Returns the caller's sessions whose refresh token has not expired, cursor-paginated, sortable only by `created_at`. Each item is `{id, auth_method, device_name, created_at, expires_at, refresh_token_expires_at}`; token hashes and fingerprint are never included. Clients spot their own row by comparing `id` with their `session_id`.
- **Kind:** `capability`
- **Trigger:** `GET /users/me/sessions`
- **Auth:** session; email-verification gate applies (user-scoped group).
- **Input & validation:** standard pagination params; `sort_by` allowlist `["created_at"]`.
- **Effects:** RLS `sessions_user_access` plus an explicit `user_id` filter plus `refresh_token_expires_at > NOW()`; `device_name` decrypted (one bad ciphertext fails the page).
- **Source:** `src/handlers/sessions.rs:37-58,92-111`, `src/services/session_service.rs:197-209`, `src/repositories/postgresql/session_repository.rs:200-289`
- **Tests:** inline `session_service.rs::list_sessions_returns_repository_paginated_result`

### AUTH-029 Revoke one of my sessions by id

- **Behavior:** Deletes the named session if it belongs to the caller, including the caller's current one. Revokes its provider token first for non-credentials sessions. Returns empty success.
- **Kind:** `capability`
- **Trigger:** `DELETE /users/me/sessions/{session_id}`
- **Auth:** session; verification gate applies.
- **Input & validation:** `session_id` UUID path parameter.
- **Effects:**
  - Lookup under RLS (another user's session is invisible).
  - AUTH-033 per-session provider revocation.
  - `DELETE FROM sessions WHERE id=$1` (zero rows counts as success); the `oauth_tokens` row cascades.
  - Audit `session.revoke` (resource = session id), best-effort.
  - SSE `session.revoked` `{session_ids:[id], revoked_by_session_id: caller's session}`, best-effort.
- **Errors:** not found, including another user's id → `RESOURCE_NOT_FOUND` 404 with details `{entity_type:"session", entity_id}`. A DELETE failure propagates, and no audit or SSE fires.
- **Guarantees:** provider revocation is attempted before the DELETE; the DELETE is the load-bearing action.
- **Source:** `src/handlers/sessions.rs:137-148`, `src/services/session_service.rs:75-184`, `src/repositories/postgresql/session_repository.rs:110-187`
- **Tests:** inline `session_service.rs::revoke_session_*` (13 tests)

### AUTH-030 Sign out all my other sessions

- **Behavior:** Deletes every live session of the caller except the current one and returns `{revoked_count}`.
- **Kind:** `capability`
- **Trigger:** `DELETE /users/me/sessions/others`
- **Auth:** session; verification gate applies.
- **Effects:**
  - AUTH-033 bulk revocation of provider tokens on the user's live sessions other than the current one, run concurrently.
  - `DELETE ... WHERE user_id=$1 AND id<>$current AND refresh_token_expires_at>NOW() RETURNING id`.
  - One audit `session.revoke` per deleted id, with metadata `{via:"revoke_others", current_session_id}`, written concurrently, best-effort.
  - One SSE `session.revoked` `{session_ids:[...], revoked_by_session_id}`, only if any were deleted.
- **Guarantees:** the revoked set matches the deleted set (same liveness filter). Idempotent: zero others returns `revoked_count: 0` and no event.
- **Source:** `src/handlers/sessions.rs:60-69,170-182`, `src/services/session_service.rs:222-331`, `src/repositories/postgresql/session_repository.rs:351-404`, `src/handlers/mod.rs:225-233`
- **Tests:** inline `session_service.rs::revoke_other_sessions_*` (11 tests)

### AUTH-031 Resend the email-verification code

- **Behavior:** Generates a new 6-digit code for the caller, replacing any previous one and resetting its attempt counter, emails it, and returns `{expires_at}`. Works whenever the email backend is configured, even if verification is not required.
- **Kind:** `capability`
- **Trigger:** `POST /auth/resend-verification`
- **Auth:** session (verification-exempt group).
- **Input & validation:** none. Per-user limit `verification_resend:<user_id>` (`VERIFICATION_RESEND_RATE_LIMIT_MAX_ATTEMPTS` 3 per `..._WINDOW_SECS` 300), checked and consumed before any other check; limiter errors fail open.
- **Effects:** AUTH-036 code storage (purpose `email_verification`, expiry `VERIFICATION_CODE_EXPIRY_MINUTES` default 15, max attempts `VERIFICATION_CODE_MAX_ATTEMPTS` default 5). MAIL `email_verification` template sent synchronously.
- **Errors:**
  - over the limit → `RATE_LIMIT_EXCEEDED` 429 "Too many resend requests. Try again in N seconds".
  - user missing → `RESOURCE_NOT_FOUND` 404.
  - already verified → `VALIDATION_ERROR` 400 "Email is already verified".
  - email service disabled → `INTERNAL_SERVER_ERROR` 500.
  - send failure → 500/503 from MAIL.
- **Source:** `src/handlers/auth.rs:796-862`, `src/services/public_auth_service.rs:1638-1707`
- **Tests:** inline `public_auth_service.rs::send_verification_code_*` (3 tests)

### AUTH-032 Verify email with the 6-digit code

- **Behavior:** Validates the code (AUTH-037). On success it sets `email_verified=true`, deletes the code, and returns the session context with `email_verified: true`.
- **Kind:** `capability`
- **Trigger:** `POST /auth/verify-email`
- **Auth:** session (verification-exempt group). No dedicated rate limit beyond the per-user tier.
- **Input & validation:** `code` trimmed, exactly 6 characters, ASCII digits only (`models/auth.rs:522-530`, `validation/mod.rs:472-480`).
- **Effects:** `sp_public_complete_email_verification` (updates only if currently false; deletes the user's `email_verification` code). Audit `auth.email_verified` `{source:"verification_code", auth_method:"credentials"}`, best-effort, emitted whether or not the flag actually changed. Workspace per AUTH-015.
- **Errors:** per AUTH-037 (400).
- **Source:** `src/handlers/auth.rs:731-780`, `src/services/public_auth_service.rs:1720-1792`, `src/repositories/postgresql/public_verification_code_repository.rs:183-198`, `migrations/0005_stored_procedures.sql:1175-1194`
- **Tests:** inline `public_auth_service.rs::verify_email_code_*` (6 tests); `models/auth.rs::verify_email_request_enforces_six_character_code_length`; `tests/verification_code_repository_live.rs::complete_email_verification_flips_verified_and_deletes_only_the_email_code`

### AUTH-033 Provider tokens are revoked (RFC 7009) whenever their session is torn down

- **Behavior:** Before (or, for unlinks, right after) a session row is deleted, the stored provider access token is decrypted and revoked at the provider. Every failure is logged as a `failing_open` warn and swallowed; the session deletion always proceeds.
- **Kind:** `guarantee`
- **Trigger:** AUTH-024, AUTH-027, AUTH-029, AUTH-030, AUTH-035, AUTH-042, AUTH-051, AUTH-052; also USER's password change and account deletion, which call the same helpers.
- **Auth:** n/a
- **Effects:**
  - Google: `POST https://oauth2.googleapis.com/revoke` with form `token`.
  - GitHub: `DELETE https://api.github.com/applications/{client_id}/token` with JSON `{access_token}`, Basic auth `client_id:client_secret`, and the User-Agent.
  - Non-2xx → `RevocationFailed` with `upstream_status` (logged only).
  - Token sets by caller:
    - single session: `find_by_session`.
    - "others" and password reset: live sessions except one (`find_by_user_excluding_session`), with `Uuid::nil()` as the exclusion for reset.
    - unlinks: same fetch filtered to the reaped ids (and to the provider for OAuth unlink).
    - cleanup: `find_revocable_for_expired_sessions`.
    - account deletion: `find_all_by_user`.
  - Bulk paths run concurrently (`join_all`).
- **Guarantees:** the revoked set equals the deleted set (same liveness predicate); see Observations for the same-device-relogin gap.
- **Source:** `src/auth/revocation.rs:45-349`, `src/auth/google_oauth.rs:248-279`, `src/auth/github_oauth.rs:400-438`, `src/errors/oauth_error.rs:426-435`, `migrations/0005_stored_procedures.sql:911-1080`
- **Tests:** inline `revocation.rs::revoke_one_provider_token_swallows_unconfigured_provider`; `session_service.rs::*provider_revocation*`; `public_auth_service.rs::refresh_session_tokens_reuse_*`, `cleanup_expired_sessions_*`; `tests/oauth_token_repository_live.rs` (4 tests)

### AUTH-034 Revocation reaches other devices and other instances

- **Behavior:** Every path that deletes sessions publishes a single `session.revoked` user event `{session_ids, revoked_by_session_id}` after commit, but only when it deleted at least one session. `revoked_by_session_id` is the caller's session id, or `null` for password reset. Clients force-logout when their id is in the list. Independently, because tokens are checked against Postgres on every request (AUTH-019), a revoked session fails on any instance immediately.
- **Kind:** `guarantee`
- **Trigger:** AUTH-027, AUTH-029, AUTH-030, AUTH-042, AUTH-051, AUTH-052 (and USER password change); not emitted by AUTH-024 reuse termination or by AUTH-017 same-device replacement.
- **Auth:** n/a
- **Effects:** SSE `session.revoked` via an `sse_events` row and `pg_notify` (RT). Publish failure is a `failing_open` warn.
- **Guarantees:** best-effort notification; hard enforcement comes from the DB lookup. Open SSE streams are not closed server-side (see Observations).
- **Source:** `src/services/session_service.rs:158-177,301-323`, `src/services/public_auth_service.rs:2211-2234`, `src/services/identity_service.rs:231-264`
- **Tests:** inline `session_service.rs::revoke_session_publishes_session_revoked_event`, `revoke_other_sessions_publishes_single_event_with_all_revoked_ids`, `*_no_event_when_nothing_revoked`; `tests/auth_session_fanout_live.rs::reset_password_fans_out_session_revoked_and_audits`; `tests/identity_unlink_fanout_live.rs`

### AUTH-035 Expired sessions are reaped periodically, after revoking still-live provider tokens

- **Behavior:** Every `CLEANUP_INTERVAL_HOURS` (default 6), under a distributed lock, one cutoff instant is captured. Provider tokens of sessions with `refresh_token_expires_at < cutoff` whose provider token is still live (`provider_token_expires_at` NULL or later than the cutoff) are revoked. Then those sessions are deleted and their `oauth_tokens` rows cascade.
- **Kind:** `guarantee` (operational property: dead sessions and their provider grants do not linger)
- **Trigger:** background job `auth_cleanup` (also runs verification-code cleanup, AUTH-036; invitation and pending-email-change cleanup belong to WS and USER). Disabled by `ENABLE_BACKGROUND_CLEANUP=false`.
- **Auth:** system
- **Effects:** `sp_public_find_oauth_tokens_for_expired_sessions(cutoff)`, AUTH-033 bulk revoke (a fetch failure is a warn and the delete still runs), then `sp_cleanup_expired_sessions(cutoff)` returns the deleted count. The service call is wrapped in `retry_with_backoff` (3 attempts); the result is logged by `log_cleanup_result("sessions")`.
- **Source:** `src/services/public_auth_service.rs:1563-1605`, `src/server.rs:1574-1638`, `migrations/0006_background_tasks.sql:50-68`, `migrations/0005_stored_procedures.sql:1057-1080`
- **Tests:** inline `public_auth_service.rs::cleanup_expired_sessions_swallows_revocable_fetch_failure`, `cleanup_expired_sessions_revokes_fetched_provider_tokens`; `tests/oauth_token_repository_live.rs::find_revocable_for_expired_sessions_selects_expired_session_with_live_provider_token`

### AUTH-036 Verification codes: format, storage and lifecycle

- **Behavior:** Codes are exactly 6 ASCII digits, zero-padded, drawn uniformly from 000000-999999 with rejection sampling over a u32. Only `HMAC-SHA256(active key)` of the code is stored. There is one row per `(user_id, purpose)` with purpose in `email_verification | password_reset | email_change`. Issuing a new code replaces the old one (new id, hash, `attempts_used=0`, `max_attempts`, `created_at`, `expires_at`). Expired rows are deleted by the `auth_cleanup` job.
- **Kind:** `guarantee`
- **Trigger:** AUTH-003/031 (`email_verification`), AUTH-040 (`password_reset`); USER email change uses `email_change` through its own SP.
- **Auth:** n/a
- **Effects:** `sp_public_upsert_verification_code`; `sp_cleanup_expired_verification_codes` deletes `expires_at < NOW()`.
- **Guarantees:** the raw code is never persisted.
- **Source:** `src/auth/verification_code_generator.rs:7-33`, `src/models/verification_code.rs:13-85`, `src/repositories/postgresql/public_verification_code_repository.rs:64-87,405-420`, `migrations/0005_stored_procedures.sql:1087-1122`, `migrations/0006_background_tasks.sql:104-122`, `migrations/0002_tables.sql:166-187`
- **Tests:** inline `verification_code_generator.rs::*` (4), `verification_code.rs::*` (4); `tests/verification_code_repository_live.rs::upsert_verification_code_conflict_resets_the_brute_force_lockout`, `find_by_user_id_and_purpose_is_scoped_to_user_and_purpose`

### AUTH-037 Verification-code validation order and attempt limit

- **Behavior:** Shared validator used by email verification, password reset and USER's email change:
  1. No code row → reject.
  2. Expired (`now > expires_at`) → reject, attempt not counted.
  3. Atomically increment `attempts_used` only while it is below `max_attempts`; if already at the limit → reject.
  4. Compare the submitted code's HMAC under every key version with the stored hash; mismatch → reject with remaining-attempts info.
  5. On success, return the code id.
  A correct code still consumes an attempt.
- **Kind:** `rule`
- **Trigger:** AUTH-032, AUTH-042 (and USER email-change confirm)
- **Auth:** n/a
- **Errors:** all `VALIDATION_ERROR` 400:
  - no code → "No active verification code. Please request a new code."
  - expired → "Verification code has expired. Please request a new code."
  - at the limit → "Too many failed attempts. Please request a new code."
  - mismatch → "Invalid verification code. N attempt(s) remaining." (or "... No attempts remaining. Please request a new code."), details `{reason:"invalid_verification_code", remaining_attempts}`.
  - Password reset rewrites all of these to 401 (AUTH-043).
- **Guarantees:** the attempt limit is enforced in one conditional UPDATE with no check-then-act race.
- **Source:** `src/auth/verification_code_validator.rs:23-103`, `src/repositories/postgresql/public_verification_code_repository.rs:104-166`, `migrations/0005_stored_procedures.sql:1125-1171`
- **Tests:** inline `public_auth_service.rs::verify_email_code_no_active_code_returns_business_rule_error`, `_expired_code_rejects_before_incrementing`, `_max_attempts_atomic_guard_rejects`, `_hmac_mismatch_returns_remaining_count`; `tests/verification_code_repository_live.rs::increment_attempts_climbs_to_ceiling_then_returns_none`

### AUTH-038 Email-verification gate

- **Behavior:** When `REQUIRE_EMAIL_VERIFICATION=true`, user-scoped and workspace-scoped routes reject a request whose user has `email_verified=false` and whose session has `auth_method == "credentials"`. Sessions established via OAuth always pass. When the flag is false the gate does nothing.
- **Kind:** `rule`
- **Trigger:** every user-scoped and workspace-scoped route (it runs after auth and the rate limit, before idempotency, `server.rs:627-668`)
- **Auth:** session
- **Errors:** `PERMISSION_DENIED` 403 "Email verification required", details `{reason:"email_not_verified"}`. If the gate is reached without an AuthContext (wiring error) → 500 `INTERNAL_SERVER_ERROR`.
- **Guarantees:** the user's `email_verified` is read fresh on every request (AUTH-020), so verification takes effect immediately.
- **Source:** `src/auth/middleware.rs:85-123`, `src/errors/auth_error.rs:545-552`
- **Tests:** inline `middleware.rs::blocks_only_unverified_credentials_users`; `tests/email_verification_middleware_http.rs` (4 tests)

### AUTH-039 Verification-exempt routes

- **Behavior:** The routes an unverified user needs in order to recover never pass through the gate: `POST /auth/validate-session`, `/auth/logout`, `/auth/verify-email`, `/auth/resend-verification`, and (USER's) `/users/me/email`, `/users/me/email/confirm`, `/users/me/delete`. `/auth/identities*` and `/users/me/sessions*` are NOT exempt.
- **Kind:** `rule`
- **Trigger:** route registration
- **Auth:** session
- **Source:** `src/handlers/mod.rs:103-146,225-233,274-290`, `src/server.rs:608-622`
- **Tests:** `tests/email_verification_middleware_http.rs::unverified_user_reaches_verification_exempt_route_when_verification_required`; `tests/router_composition.rs`

### AUTH-040 Request a password-reset code (forgot password)

- **Behavior:** For an email belonging to an account that has a credentials identity, a 6-digit `password_reset` code is created (expiry `PASSWORD_RESET_CODE_EXPIRY_MINUTES` default 15, `PASSWORD_RESET_CODE_MAX_ATTEMPTS` default 5) and emailed. The response is identical in every case.
- **Kind:** `capability`
- **Trigger:** `POST /auth/forgot-password`
- **Auth:** public; per-IP limit; idempotency; per-email limit `password_reset:<HMAC(email)>` (`PASSWORD_RESET_RATE_LIMIT_MAX_ATTEMPTS` 3 per `..._WINDOW_SECS` 300), limiter errors fail open.
- **Input & validation:** `email` normalized, 1-254 chars, email format.
- **Effects:** code upsert (AUTH-036); MAIL `password_reset` template sent in a spawned task carrying the request's tracing span. Response body: `{message:"If an account exists with this email, a password reset code has been sent."}`.
- **Errors:** validation 400; over the limit → 429 "Too many password reset requests. Try again in N seconds".
- **Source:** `src/handlers/auth.rs:874-971`, `src/services/public_auth_service.rs:1854-1997`
- **Tests:** inline `public_auth_service.rs::request_password_reset_*` (8 tests)

### AUTH-041 Forgot-password never reveals whether the account exists

- **Behavior:** Email service disabled, unknown email, OAuth-only account, DB lookup error, code persist failure, template render failure and SMTP failure all return the same 200 body. SMTP runs off the request path.
- **Kind:** `guarantee`
- **Trigger:** `POST /auth/forgot-password`
- **Auth:** public
- **Effects:** each internal failure logs a `failing_open` warn (`email_service_not_configured`, `user_lookup_failed`, `verification_code_persist_failed`, `email_template_render_failed`, `email_send_failed`).
- **Source:** `src/services/public_auth_service.rs:1814-1997`, `src/handlers/auth.rs:961-970`
- **Tests:** inline `public_auth_service.rs::request_password_reset_*`

### AUTH-042 Reset the password with a code

- **Behavior:** With a valid code for a credentials account, the password hash is replaced, every live session of the user is deleted, any pending email change is cancelled, and `{password_reset: true}` is returned. The user is not signed in and `email_verified` is unchanged.
- **Kind:** `capability`
- **Trigger:** `POST /auth/reset-password`
- **Auth:** public; per-IP limit; idempotency; per-email limit `password_reset_verify:<HMAC(email)>` (`PASSWORD_RESET_VERIFY_RATE_LIMIT_MAX_ATTEMPTS` 10 per `..._WINDOW_SECS` 900), limiter errors fail open.
- **Input & validation:** `email` normalized, format checked; `code` exactly 6 digits; `new_password` with the same rules as AUTH-001.
- **Effects:**
  1. AUTH-037 validation (attempt counted).
  2. Argon2id hash.
  3. AUTH-033 revocation of every live provider token of the user (before the transaction).
  4. One transaction: `sp_public_complete_password_reset` (update the credentials hash; delete the `password_reset` code; delete `pending_email_changes` and the `email_change` code; delete sessions with `refresh_token_expires_at > NOW()` returning ids) plus audit `auth.password_reset` `{auth_method:"credentials"}` in the transaction.
  5. After commit: SSE `session.revoked` `{session_ids, revoked_by_session_id:null}` if any; MAIL `password_reset_confirmation` sent synchronously, best-effort.
- **Errors:** over the limit → 429 "Too many password reset attempts. Try again in N seconds"; failures per AUTH-043; infrastructure errors propagate (5xx).
- **Guarantees:** password change, session invalidation and audit are atomic.
- **Source:** `src/handlers/auth.rs:986-1074`, `src/services/public_auth_service.rs:2009-2275`, `src/repositories/postgresql/public_verification_code_repository.rs:224-247`, `migrations/0005_stored_procedures.sql:1225-1291`
- **Tests:** `tests/auth_transaction_flows.rs::reset_password_is_atomic_and_invalidates_all_sessions`, `reset_password_noops_for_user_without_credentials_identity`; `tests/auth_session_fanout_live.rs::reset_password_*` (3 tests)

### AUTH-043 Reset-password failures collapse to one 401

- **Behavior:** Unknown email, an account without a credentials identity, and every code failure (none, expired, attempts exhausted, wrong code) all return the same 401.
- **Kind:** `guarantee`
- **Trigger:** `POST /auth/reset-password`
- **Auth:** public
- **Errors:** `AUTHENTICATION_FAILED` 401 "Invalid email or verification code".
- **Source:** `src/services/public_auth_service.rs:2015-2080,2182-2186`
- **Tests:** inline `public_auth_service.rs::reset_password_unknown_email_returns_invalid_credentials`, `reset_password_oauth_user_returns_invalid_credentials`, `reset_password_business_rule_collapses_to_invalid_credentials`

### AUTH-044 List my linked sign-in methods

- **Behavior:** Returns `{identities:[{method, created_at}]}` ordered by `created_at` ascending. Provider subject and password hash are never exposed.
- **Kind:** `capability`
- **Trigger:** `GET /auth/identities`
- **Auth:** session; verification gate applies.
- **Effects:** RLS `user_identities_user_access` plus an explicit `user_id` filter.
- **Source:** `src/handlers/identities.rs:30-55`, `src/services/identity_service.rs:309-319`, `src/repositories/postgresql/identity_repository.rs:67-103`, `src/models/identity.rs:43-56`
- **Tests:** inline `identity_service.rs::list_identities_maps_repo_rows_to_summaries`

### AUTH-045 Add a password to my account

- **Behavior:** Attaches a credentials identity (Argon2id hash) to an account that has none and returns `{method:"credentials", created_at}`. No current-password challenge; no sessions are revoked.
- **Kind:** `capability`
- **Trigger:** `POST /auth/identities/credentials`
- **Auth:** session; verification gate applies; per-user limit only.
- **Input & validation:** `password` with the same rules as AUTH-001 (`models/identity.rs:64-75`).
- **Effects:** RLS insert into `user_identities`. Audit `auth.identity_linked` `new_value {method}`, best-effort. SSE `identity.linked` `{method, linked_by_session_id}`, best-effort.
- **Errors:** already linked (pre-check, or unique violation `user_identities_user_method_unique` in a race) → `RESOURCE_CONFLICT` 409 "Identity already linked for method 'credentials'", details `{reason:"identity_already_linked", method}`.
- **Source:** `src/handlers/identities.rs:70-80`, `src/services/identity_service.rs:275-294,331-387`, `src/repositories/postgresql/identity_repository.rs:118-165`
- **Tests:** inline `identity_service.rs::link_credentials_*` (5), `map_link_conflict_*` (4); `tests/identity_repository_live.rs` (2)

### AUTH-046 Link an OAuth provider to my account

- **Behavior:** Runs the same server-side exchange and gates as login (AUTH-007/008) with an authorization code, then binds `(method, subject)` to the caller's account. Returns `{method, created_at}`. The provider email may differ from the account email.
- **Kind:** `capability`
- **Trigger:** `POST /auth/identities/oauth/complete`
- **Auth:** session; verification gate applies; per-user limit only.
- **Input & validation:** `method` trimmed and lowercased, 1-50; `auth_code`, `code_verifier`, `redirect_uri` as AUTH-007; no device fields (`models/identity.rs:109-144`).
- **Effects:** AUTH-048 immediate token revocation; AUTH-047 guards; RLS insert; AUTH-049 email-verified upgrade; audit `auth.identity_linked` `{method}`; SSE `identity.linked` `{method, linked_by_session_id}`. Creates no session and no `oauth_tokens` row.
- **Errors:** unknown provider → 400 (AUTH-050); gate failures per AUTH-008; conflicts per AUTH-047.
- **Source:** `src/handlers/identities.rs:98-114`, `src/services/identity_service.rs:421-556`
- **Tests:** inline `identity_service.rs::link_oauth_complete_*` (11 tests)

### AUTH-047 OAuth-link takeover guards

- **Behavior:** Checked in order:
  1. If `(method, subject)` belongs to another account → conflict; if it already belongs to the caller → already-linked.
  2. If the normalized provider email belongs to another account → conflict.
  3. If the caller already has any identity for `method` → already-linked.
  4. A unique violation at insert is mapped the same way (`provider_subject_unique` → conflict, `user_method_unique` → already-linked).
- **Kind:** `rule`
- **Trigger:** `POST /auth/identities/oauth/complete`
- **Auth:** session
- **Errors:** both `RESOURCE_CONFLICT` 409:
  - conflict → "This <method> account is already associated with another user", details `{reason:"identity_conflict", method}`.
  - already linked → "Identity already linked for method '<method>'", details `{reason:"identity_already_linked", method}`.
- **Guarantees:** never auto-merges accounts; never names the other account.
- **Source:** `src/services/identity_service.rs:469-537,275-294`, `src/errors/auth_error.rs:576-598`
- **Tests:** inline `identity_service.rs::link_oauth_complete_rejects_*` (4), `link_oauth_complete_normalizes_provider_email_before_collision_lookup`

### AUTH-048 Provider token minted during linking is revoked immediately

- **Behavior:** Right after the exchange, before any guard, the provider access token obtained while linking is revoked at the provider, best-effort. This happens whether the link then succeeds or is rejected.
- **Kind:** `guarantee`
- **Trigger:** `POST /auth/identities/oauth/complete`
- **Auth:** session
- **Effects:** AUTH-033 provider calls via `revoke_unpersisted_provider_token`; failure is a `failing_open` warn.
- **Source:** `src/services/identity_service.rs:439-467`, `src/auth/revocation.rs:148-162`
- **Tests:** inline `identity_service.rs::link_oauth_complete_revokes_minted_token_on_success`, `link_oauth_complete_revokes_minted_token_even_when_rejected`

### AUTH-049 Linking an OAuth identity with the same verified email marks the account verified

- **Behavior:** After a successful OAuth link, if the caller is unverified and the normalized provider email equals the account email, `email_verified` is set to true. Audit `auth.email_verified` `{source:"oauth_link", auth_method:<method>}` is written only when this call actually flipped the flag. Failures are swallowed.
- **Kind:** `rule`
- **Trigger:** `POST /auth/identities/oauth/complete`
- **Auth:** session
- **Effects:** RLS `UPDATE users SET email_verified=true WHERE id=$1 AND email_verified=false`. No SSE event.
- **Source:** `src/services/identity_service.rs:542-543,652-722`, `src/repositories/postgresql/user_repository.rs:625-663`
- **Tests:** inline `identity_service.rs::upgrade_*` (6 tests)

### AUTH-050 OAuth providers are enabled by configuration

- **Behavior:** Google is enabled when `GOOGLE_CLIENT_ID` is set (no secret: public-client PKCE). GitHub is enabled when both `GITHUB_CLIENT_ID` and `GITHUB_CLIENT_SECRET` are set. Zero providers is valid. Naming a provider that is not enabled fails.
- **Kind:** `rule`
- **Trigger:** startup; `POST /auth/oauth`; `POST /auth/identities/oauth/complete`
- **Auth:** n/a
- **Errors:** `VALIDATION_ERROR` 400 "OAuth provider 'X' is not configured. Available providers: ...", details `{requested_provider, available_providers:[...]}` (an empty array when none). Startup fails if only one GitHub value is set, or if any value equals its `.env.example` placeholder.
- **Source:** `src/auth/provider_factory.rs:47-137`, `src/errors/auth_error.rs:505-521`, `src/config/environment.rs:1195-1258`
- **Tests:** inline `provider_factory.rs::*` (7 tests)

### AUTH-051 Remove my password

- **Behavior:** With the correct current password, deletes the credentials identity, provided another identity remains. Deletes every other live session (any method) and keeps the current one. Returns empty success.
- **Kind:** `capability`
- **Trigger:** `POST /auth/identities/credentials/unlink`
- **Auth:** session; verification gate applies.
- **Input & validation:** `current_password` 1-128 chars (`models/identity.rs:86-90`).
- **Effects:**
  1. Per-user limit `password_change:<user_id>` (`PASSWORD_CHANGE_RATE_LIMIT_MAX_ATTEMPTS` 5 per `..._WINDOW_SECS` 900, shared with USER's password change), checked before the identity lookup; limiter errors fail open.
  2. Argon2 verify, outside the transaction.
  3. Read the revocable tokens of other live sessions.
  4. One transaction: `sp_public_unlink_credentials` locks the user's identity rows `FOR UPDATE`, checks the identity exists and is not the last one, deletes it, and deletes other live sessions returning ids; plus audit `auth.identity_unlinked` `{method:"credentials", revoked_other_session_count}` in the transaction.
  5. After commit: revoke provider tokens of exactly the reaped sessions; SSE `session.revoked` (if any) and then SSE `identity.unlinked` `{method, unlinked_by_session_id}`, in that order.
- **Errors:**
  - over the limit → 429 "Too many credential-unlink attempts. Try again in N seconds".
  - no credentials identity, or wrong password → `AUTHENTICATION_FAILED` 401 "Invalid current password".
  - last identity → AUTH-053.
  - an identity that vanished concurrently → 200 no-op (no audit, no SSE).
- **Source:** `src/handlers/identities.rs:130-149`, `src/services/identity_service.rs:731-1016`, `src/repositories/postgresql/public_user_repository.rs:532-555,616-643`, `migrations/0005_stored_procedures.sql:1611-1690`
- **Tests:** inline `identity_service.rs::unlink_credentials_*` (5), `tokens_for_revoked_sessions_keeps_only_reaped_sessions`; `tests/auth_transaction_flows.rs::unlink_*` (3); `tests/identity_unlink_fanout_live.rs::unlink_credentials_fans_out_session_revoked_and_identity_unlinked_and_audits`

### AUTH-052 Disconnect an OAuth provider

- **Behavior:** Deletes the caller's identity for `{method}` if another identity remains. Deletes the caller's other live sessions that were established with that method, keeping the current session even when it used that method. No password challenge. Returns empty success, including when the method was never linked or is not a configured provider.
- **Kind:** `capability`
- **Trigger:** `DELETE /auth/identities/{method}`
- **Auth:** session; verification gate applies; per-user limit only.
- **Input & validation:** path `method` trimmed and lowercased; length 1-50 chars, otherwise `VALIDATION_ERROR` 400 "OAuth provider must be between 1 and 50 characters"; `credentials` → 400 "Use POST /auth/identities/credentials/unlink with a body to unlink the password method".
- **Effects:**
  1. Read revocable tokens of other live sessions, filtered to this provider.
  2. One transaction: `sp_public_unlink_oauth_identity` (same lock and guard as AUTH-051; deletes sessions with `auth_method=method AND id<>current AND refresh_token_expires_at>NOW()`) plus in-transaction audit `auth.identity_unlinked` `{method, revoked_other_session_count}`.
  3. After commit: revoke the reaped sessions' tokens; SSE `session.revoked` (if any), then `identity.unlinked`.
- **Errors:** last identity → AUTH-053.
- **Source:** `src/handlers/identities.rs:171-212`, `src/services/identity_service.rs:1079-1229`, `src/repositories/postgresql/public_user_repository.rs:578-603`, `migrations/0005_stored_procedures.sql:1753-1832`
- **Tests:** `tests/identities_unlink_http.rs::delete_oauth_identity_guards_reject_bad_method_and_credentials`; `tests/identity_unlink_fanout_live.rs` (2 OAuth tests); `tests/auth_transaction_flows.rs::unlink_deletes_target_identity_then_rejects_removing_the_last`, `unlink_keeps_the_callers_current_session_signed_in`

### AUTH-053 The last sign-in method cannot be removed

- **Behavior:** Unlinking the only remaining identity is refused with no side effects. The check is made under a row lock, so concurrent unlinks cannot both succeed and leave zero identities.
- **Kind:** `rule`
- **Trigger:** AUTH-051, AUTH-052
- **Auth:** session
- **Errors:** `RESOURCE_CONFLICT` 409 "Cannot remove '<method>' — it is the last sign-in method on this account", details `{reason:"last_identity_protected", method}`.
- **Guarantees:** no provider revocation, audit or SSE on refusal.
- **Source:** `migrations/0005_stored_procedures.sql:1629-1654,1772-1794`, `src/services/identity_service.rs:941-950,1184-1186`, `src/errors/auth_error.rs:602-611`
- **Tests:** `tests/auth_transaction_flows.rs::unlink_deletes_target_identity_then_rejects_removing_the_last`

### AUTH-054 Auth configuration discovery

- **Behavior:** Returns `{require_email_verification, oauth_providers:[{provider_id, provider_name, client_id, auth_url, scopes, icon_url?, additional_auth_params?}]}`, sorted by `provider_id`, so clients can render sign-in options and build authorization URLs. Values:
  - Google: `"google"`, "Google", `https://accounts.google.com/o/oauth2/v2/auth`, scopes `openid email profile`, a gstatic icon URL.
  - GitHub: `"github"`, "GitHub", `https://github.com/login/oauth/authorize`, scopes `read:user user:email`, a githubassets favicon.
  - `additional_auth_params` is omitted when empty (always, for both shipped providers).
- **Kind:** `capability`
- **Trigger:** `GET /auth/config`
- **Auth:** public; per-IP limit.
- **Source:** `src/handlers/auth.rs:640-704`, `src/auth/google_oauth.rs:202-228`, `src/auth/github_oauth.rs:356-380`, `src/auth/oauth_provider.rs:321-323`
- **Tests:** inline `provider_factory.rs::get_available_providers_is_sorted_by_provider_id`

### AUTH-055 Password hashing

- **Behavior:** Passwords are hashed with Argon2id using the `argon2` 0.5.3 crate defaults and a random 16-byte salt, stored as a PHC string (`$argon2id$v=19$...`). Verification parses the stored PHC parameters: mismatch returns false; a malformed or non-Argon2 hash is an error (500), not "wrong password". Old hashes are never rehashed on login.
- **Kind:** `guarantee`
- **Trigger:** AUTH-001, AUTH-004, AUTH-042, AUTH-045, AUTH-051 (and USER password change and deletion)
- **Auth:** n/a
- **Guarantees:** constant-time comparison inside the crate; the dummy hash for AUTH-002/005 is computed at startup with the same function.
- **Source:** `src/auth/password.rs:18-62`, `src/services/public_auth_service.rs:142-143`
- **Tests:** inline `password.rs::*` (8 tests)

## Data owned

- **`sessions`** (`0002_tables.sql:91-133`): `id`, `user_id` (FK `users` ON DELETE CASCADE, `0002:645`), `access_token_hash`/`refresh_token_hash` (VARCHAR(64), each UNIQUE), `previous_refresh_token_hash` (nullable, not unique), `auth_method` (`credentials` or a provider id), `device_fingerprint` (VARCHAR(64), plaintext), `device_name` (encrypted), `created_at`, `expires_at` (access), `refresh_token_expires_at`. UNIQUE `(user_id, device_fingerprint)`. Indexes: `idx_sessions_user`, `idx_sessions_expires`, `idx_sessions_refresh_token_expires_at`, `idx_sessions_device_fingerprint`, partial `idx_sessions_previous_refresh_token_hash` (`0003_indexes.sql:33-45`). RLS `sessions_user_access` FOR ALL on `user_id = app.current_user_id` (`0004:218-220`).
- **`user_identities`** (`0002:56-87`): `id`, `user_id` (FK deferred, CASCADE), `method`, `provider_subject`, `password_hash`, timestamps. UNIQUE `(user_id, method)` and `(method, provider_subject)`. CHECK `user_identities_shape`: credentials rows have a hash and no subject; other rows have a subject and no hash. RLS `user_identities_user_access` with a WITH CHECK (`0004:227-230`).
- **`oauth_tokens`** (`0002:144-158`): `id`, `user_id` (FK CASCADE), `session_id` (FK CASCADE, ON UPDATE CASCADE, UNIQUE), `oauth_provider`, `provider_access_token_encrypted`, `provider_token_expires_at` (nullable), timestamps. No refresh-token column. Index `idx_oauth_tokens_user`. RLS `oauth_tokens_user_access` (`0004:257-259`).
- **`verification_codes`** (`0002:166-187`, shared with USER for `email_change`): `purpose` VARCHAR(32), `code_hash`, `attempts_used`, `max_attempts`, `expires_at`; UNIQUE `(user_id, purpose)`; `idx_verification_codes_expires_at`; RLS `verification_codes_user_policy` (`0004:262-264`).
- **Touched, owned by USER:** `users` (created at registration; `email_hash` UNIQUE; `email_verified` written by AUTH-032/049); `user_preferences` (created at registration; `default_workspace_id` written by AUTH-014); `pending_email_changes` (deleted by AUTH-042).
- **Stored procedures (`0005_stored_procedures.sql`):**
  - Users and identities: `sp_public_find_user_by_email_hash` (54), `..._with_credentials_hash` (88), `sp_public_find_user_by_oauth_identity` (124), `sp_public_find_credentials_identity_by_user_id` (161), `sp_public_insert_identity` (194), `sp_public_create_user` (217), `sp_public_ingest_user_avatar` (255).
  - Preferences: `sp_public_create_user_preferences` (377), `sp_public_update_user_preferences` (393), `sp_public_clear_user_default_workspace` (410).
  - Sessions: `sp_public_find_session_by_token_hash` (432), `sp_public_upsert_session` (464), `sp_public_update_session_tokens` (538), `sp_public_find_session_by_refresh_token_hash` (570), `..._by_previous_refresh_token_hash` (613), `sp_public_terminate_session_for_reuse` (653).
  - Auth context: `sp_public_get_user_by_id` (674), `sp_public_get_user_preferences` (697), `sp_public_validate_workspace_access` (716), `sp_public_get_workspace_permissions` (733), `sp_public_get_workspace_by_id` (750), `sp_public_get_user_accessible_workspaces` (770, ordered by `created_at ASC`).
  - Provider tokens: `sp_public_upsert_oauth_tokens` (876), `sp_public_find_oauth_tokens_by_session` (911), `..._by_user_excluding_session` (958), `..._by_user` (1006), `..._for_expired_sessions` (1057).
  - Verification codes and reset: `sp_public_upsert_verification_code` (1087), `sp_public_find_verification_code` (1125), `sp_public_increment_verification_attempts` (1152), `sp_public_complete_email_verification` (1175), `sp_public_complete_password_reset` (1225).
  - Unlinks: `sp_public_unlink_credentials` (1611), `sp_public_unlink_oauth_identity` (1753).
  - Also `sp_system_re_encrypt_*` for sessions and oauth_tokens (DATA).
- **Cleanup procedures:** `sp_cleanup_expired_sessions(p_cutoff)` (`0006_background_tasks.sql:50-68`), `sp_cleanup_expired_verification_codes()` (`0006:104-122`).
- **Background task:** `auth_cleanup` (`server.rs:1574-1638`), with session and verification-code passes owned here.

## Configuration

| Variable | Default | Validation / notes |
|---|---|---|
| `ACCESS_TOKEN_LIFETIME_SECS` | 3600 | > 0 and < refresh lifetime (`environment.rs:666-684`) |
| `REFRESH_TOKEN_LIFETIME_SECS` | 2592000 | > 0 |
| `REQUIRE_EMAIL_VERIFICATION` | false | true requires `EMAIL_BACKEND=smtp` (`environment.rs:1262-1266`) |
| `VERIFICATION_CODE_EXPIRY_MINUTES` / `_MAX_ATTEMPTS` | 15 / 5 | when SMTP: expiry 1-60, attempts > 0 (`environment.rs:1273-1287`) |
| `PASSWORD_RESET_CODE_EXPIRY_MINUTES` / `_MAX_ATTEMPTS` | 15 / 5 | when SMTP: expiry 1-60, attempts > 0 (`environment.rs:1290-1306`) |
| `GOOGLE_CLIENT_ID` | unset | must not equal the placeholder |
| `GITHUB_CLIENT_ID`, `GITHUB_CLIENT_SECRET` | unset | both or neither; no placeholders |
| `OAUTH_PROVIDER_CONNECT_TIMEOUT_SECS` | 10 | 1-30 |
| `OAUTH_PROVIDER_TOTAL_TIMEOUT_SECS` | 30 | 5-120 and >= connect (`environment.rs:1170-1193`) |
| `APP_DISPLAY_NAME` | "App" | GitHub User-Agent |
| `CREDENTIAL_LOGIN_RATE_LIMIT_MAX_ATTEMPTS` / `_WINDOW_SECS` | 5 / 900 | > 0 |
| `VERIFICATION_RESEND_RATE_LIMIT_MAX_ATTEMPTS` / `_WINDOW_SECS` | 3 / 300 | > 0 |
| `PASSWORD_RESET_RATE_LIMIT_MAX_ATTEMPTS` / `_WINDOW_SECS` | 3 / 300 | > 0; warning above 20 |
| `PASSWORD_RESET_VERIFY_RATE_LIMIT_MAX_ATTEMPTS` / `_WINDOW_SECS` | 10 / 900 | > 0; warning above 50 |
| `PASSWORD_CHANGE_RATE_LIMIT_MAX_ATTEMPTS` / `_WINDOW_SECS` | 5 / 900 | shared bucket with USER |
| `RATE_LIMIT_PER_IP` / `RATE_LIMIT_PER_USER` / `RATE_LIMIT_WINDOW_SECS` | 20 / 100 / 60 | generic tiers (HTTP) applied to auth routes |
| `ENFORCE_RATE_LIMITS` | true | false makes every limit above observability-only |
| `CLEANUP_INTERVAL_HOURS` | 6 | > 0; warning above 168 |
| `ENABLE_BACKGROUND_CLEANUP` | true | disables `auth_cleanup` |
| `storage_max_upload_bytes` (FILE/OPS) | n/a | caps the OAuth avatar fetch |

Sources: `src/config/environment.rs:342-482,660-1310`.

## Cross-domain dependencies

- **Calls out:**
  - DATA: `DatabaseEncryption` (`encrypt_data`/`decrypt_data` AES-256-GCM, `hmac_for_search`, `hmac_for_search_all_versions`), `transaction::with_public_context` / `with_user_context`, `retry_database_operation`, background runner and lock.
  - AUDIT: `UserAuditLogService::log_event` / `log_event_in_tx` with actions `auth.register`, `auth.login`, `auth.email_verified`, `auth.password_reset`, `auth.identity_linked`, `auth.identity_unlinked`, `session.revoke`, `session.reuse_detected` (`models/audit_actions.rs:187-196`).
  - RT: `SseEventPublisher::publish_user_event` with `session.created`, `session.revoked`, `identity.linked`, `identity.unlinked`.
  - MAIL: `render_email_verification`, `render_password_reset`, `render_password_reset_confirmation`, `send_email`.
  - HTTP: `RateLimitService::check_rate_limit[_by_email]`, `ValidatedJson`, `Path`/`Query`, `PaginationQuery`, the error envelope.
  - FILE: `ObjectStorageRepository::put/delete`, `PublicUserFileRepository::ingest_avatar`.
  - WS: membership and permission lookups, `Permission::from_strings`.
  - OBS: `MetricsService::record_business_event("user_signup")`.
- **Called by:**
  - every protected route via the two auth middlewares and the verification gate.
  - USER: `change_password`, `delete_user`, `request/confirm_email_change` use `validate_verification_code`, `revocation::revoke_provider_tokens_for_user` / `revoke_all_provider_tokens_for_user`, `hash_password`/`verify_password`, the `password_change` bucket.
  - RT: the SSE stream handler uses `public_auth_service.get_user_accessible_workspaces`.
  - WS: switch-workspace returns `SessionContextResponse`.
  - DATA: re-encryption covers `sessions.device_name` and `oauth_tokens`.

## Observations

1. **Same-device re-login strands or mislabels provider tokens.** `sp_public_upsert_session` replaces the row and its `id` in place (`0005_stored_procedures.sql:503-513`). `oauth_tokens.session_id` is `ON UPDATE CASCADE` (`0002_tables.sql:654`), so the old provider-token row follows the new session. Consequences:
   - (a) An OAuth re-login overwrites the old provider token without revoking it (`0005:894-898`).
   - (b) The upsert never updates `oauth_provider`, so Google then GitHub on the same device stores a GitHub token labelled `google`, which would later be sent to Google's revoke endpoint.
   - (c) A credentials re-login leaves the old OAuth token attached to a `credentials` session, and `revoke_session` and reuse termination skip revocation for credentials sessions (`session_service.rs:100`, `public_auth_service.rs:857`).
   - (d) No `session.revoked` is emitted for the replaced id.
   This contradicts the "no non-terminal path ever strands a live provider token" claim (`revocation.rs:127-128,307-308`).
2. **A null default workspace means no workspace, even with memberships.** `resolve_user_workspace` falls back only when a default exists but is inaccessible (`public_auth_service.rs:712-748`). Because `default_workspace_id` is `ON DELETE SET NULL` (`0002_tables.sql:271`), deleting the default workspace drops the user into the zero-workspace state on every later login, even if they belong to other workspaces. Refresh, validate-session and verify-email never fall back or clean up (`handlers/auth.rs:175-199`), unlike login.
3. **Registration reveals whether an email exists.** An existing email gets 401 versus 200 for a new one; the dummy Argon2 (`public_auth_service.rs:1077-1086`) equalizes only timing. Concurrent duplicate registrations get a different answer again: 409 `RESOURCE_CONFLICT` (`api_error.rs:747-752`). The OAuth email-collision message "Authentication failed for this account" (`public_auth_service.rs:1030-1032`) differs from every other OAuth failure message, despite the comment claiming parity.
4. **The email-verification gate looks at the session's `auth_method`, not the account** (`middleware.rs:121-123`). An unverified credentials account that has linked an OAuth identity (possible while the gate was off) and signs in through that provider is never gated. `identity_service.rs:1039-1042` documents this only for the self-unlink case.
5. **Revocation does not close open SSE streams.** The stream is keyed to `user_id` and never re-checks the session (`handlers/sse_notifications.rs:88-139`). A revoked device keeps receiving the user's events until it disconnects or reacts to `session.revoked`.
6. **The expired-access-token response discloses the session id.** It returns `details {session_id, expires_at}` (`public_session_repository.rs:119-124`, `repository_error.rs:739-742`, `api_error.rs:703-708`); an unknown token returns no details. This is the only signal clients have to tell "expired" from "invalid", since the envelope code is `AUTHENTICATION_FAILED` for both.
7. **Reuse detection covers one step back only** (`0005:613-638`). A token two rotations old is a plain 401 and leaves the session alive. A concurrent-refresh loser (AUTH-025) that retries the same token without its `Idempotency-Key` hits the previous-hash branch and terminates the legitimate session.
8. **Sessions have no absolute lifetime.** Each rotation resets `refresh_token_expires_at` to now plus the lifetime (`public_auth_service.rs:768-784`, `0005:550-557`), so a session refreshed at least every 30 days lives indefinitely.
9. **Unlinking a password the account doesn't have returns 401, not 200.** It answers "Invalid current password" (`identity_service.rs:818-827`) before the SP's idempotent NotFound branch can run. That branch, and the comment justifying it (`0005:1634-1645`), is reachable only through a race; the OAuth unlink path does return 200 for an absent method.
10. **The ownership check in `revoke_session` is unreachable in production** (`session_service.rs:85-89`). `find_by_id` runs under RLS, so another user's session is simply not found (404) (`session_repository.rs:121-139`). Only the mock-based test `revoke_session_rejects_when_session_belongs_to_other_user` exercises the 403 branch.
11. **Issuing a new code resets the attempt counter** (`0005:1110-1116`). The effective guess budget is `max_attempts` times the issue limit: up to 15 guesses per 5 minutes for email verification (resend 3/300 s × 5) and for password reset (forgot 3/300 s × 5, further capped by the verify bucket at 10/900 s). `/auth/verify-email` has no dedicated limit beyond the per-user tier.
12. **The code-hash comparison is not constant-time.** `Vec<String>::contains` compares hex strings (`verification_code_validator.rs:73-74`); the session-token lookup is a DB equality. Neither is a constant-time comparison.
13. **Password reset proves email control but does not mark the email verified** (`0005:1225-1291`), and does not sign the user in.
14. **`complete_email_verification` ignores whether anything changed.** The SP's boolean "changed" result is discarded (`public_verification_code_repository.rs:183-198`), so `auth.email_verified` is audited even when the user was already verified, for example by AUTH-049 after a code was issued (`public_auth_service.rs:1732-1784`).
15. **Forgot-password still has a timing difference.** The credentials branch does a DB upsert and a template render before responding, while the other branches return early; only SMTP is moved off the request path (`public_auth_service.rs:1888-1988`). The handler acknowledges a residual timing channel only for reset-password (`handlers/auth.rs:990-995`).
16. **Reset-password revokes provider tokens before its transaction** (`public_auth_service.rs:2100-2107`). If the transaction then fails, for example on the audit insert, the tokens are revoked but the sessions survive, breaking the stated "revoked set == deleted set".
17. **`auth.login` is audited before the session exists.** It is written inside `handle_existing_user_login` (`public_auth_service.rs:432-456`), before `create_session` (`handlers/auth.rs:454-462`), so a failed session creation leaves a login audit with no session. Registration audits `auth.register` but never `auth.login`. Pre-auth audit contexts carry `session_id = nil` (`public_auth_service.rs:357-379`).
18. **Doc mismatch on token storage.** `token_generator.rs:18` says "Storage: AES-256-GCM encrypted token stored for return to client", but `sessions` stores only HMACs (`0002:96-97`) and `handlers/auth.rs:46-48` says encrypted token storage was eliminated. `environment.rs:1261` says registration sends the code in `handle_new_user_credentials`; it is actually `register_credentials_user` (`public_auth_service.rs:1112-1120`).
19. **Scope matching is suffix-based.** A required scope `email` is satisfied by any granted scope ending in `.email` or `/email` (`oauth_provider.rs:132-136`).
20. **OAuth-provided names and emails skip validation.** They are not length-checked or normalized like credentials registration (`public_auth_service.rs:487-488` versus `models/auth.rs:429-438`); GitHub's `login` fallback can be any length.
21. **Device fingerprints are case-sensitive in storage.** They are validated as hex in either case but stored and matched as-is (`validation/mod.rs:447`, `models/auth.rs:384-389`), so upper- and lower-case renderings of the same fingerprint count as different devices.
22. **GitHub token expiry is always NULL.** GitHub ignores any `expires_in` (the struct lacks the field, `github_oauth.rs:27-30,272-276`), so if expiring user tokens were enabled on the GitHub app, `provider_token_expires_at` would still be NULL and cleanup would try to revoke already-dead tokens.
23. **Public auth routes sit behind idempotency** (`server.rs:593-603`). A replay with the same key and body returns the cached token-bearing response (encrypted at rest) for 24 h, and deterministic 4xx responses (for example a 401 login) are cached too (`middleware/idempotency.rs:251-252`).
24. **Inconsistent fail-open logging.** A verification-email failure during registration is logged at debug level without the canonical `failing_open` shape (`public_auth_service.rs:1113-1119`), unlike every other fail-open site. Resend consumes the rate-limit budget even when the user is already verified (`handlers/auth.rs:805-857`).
25. **OAuth unlink doesn't validate the method.** Unlinking an unconfigured or unknown provider id returns 200 no-op (`handlers/identities.rs:191-210`), while linking one returns 400.
26. **OAuth self-unlink keeps the current session and its token.** It keeps a session whose `auth_method` names a now-unlinked provider, plus its `oauth_tokens` row; the token is revoked only when that session later ends (`identity_service.rs:1028-1061`, `0005:1819-1826`).
27. **Login rejects short passwords with 400.** The login password has a minimum of 8 chars (`models/auth.rs:475-480`), so a shorter guess gets 400 `VALIDATION_ERROR` rather than 401.

## Open questions

1. Is same-device re-login meant to replace the session silently? Should it revoke or relabel the previous provider token and emit `session.revoked` for the replaced id (Observation 1)?
2. With no stored default workspace, should login fall back to an accessible workspace, as it does when the default is merely inaccessible (Observation 2)? Should refresh and validate-session share login's fallback?
3. Is registration-based email enumeration accepted for the product? If so, is the dummy Argon2 on that path still wanted (Observation 3)?
4. Should the verification gate apply to the account (`email_verified`) regardless of how the session was established (Observation 4)?
5. Should revoking a session terminate its open SSE streams server-side (Observation 5)?
6. Is the sliding, unbounded session lifetime intended, or should there be an absolute maximum (Observation 8)?
7. Should the expired-token 401 keep returning `session_id`/`expires_at`, and should clients get a machine-readable code to tell expired from invalid (Observation 6)?
8. Should a successful password reset mark the email verified (Observation 13)?
9. Should unlinking an absent credentials identity be an idempotent 200 like the OAuth path (Observation 9)?
10. Is the code guess budget across re-issues (Observation 11) acceptable, and should `/auth/verify-email` get its own limit?
11. Should OAuth unlink reject methods that are not configured providers (Observation 25)?
12. Ownership for the spec: should the session and verification-code cleanup passes (AUTH-035) be specified under AUTH or DATA?
13. Should idempotent replay of token-bearing responses on `/auth/login`, `/auth/oauth` and `/auth/refresh` remain (Observation 23)? HTTP owns the mechanism, AUTH owns the consequence.
