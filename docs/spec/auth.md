# Spec: `auth`

## Purpose

`auth` proves who a caller is. It owns sign-in methods and sessions, and it implements the `SessionVerifier` that `platform::authn` uses on every protected request. This spec covers registration and sign-in with a password, refreshing, signing out, and authenticating requests. Email verification, password reset and change, OAuth and identity linking arrive with the full module.

## Owns

| Table | Holds |
|---|---|
| `identities` | A user's sign-in methods. For a password: `user_id`, `method = 'password'`, and `password_hash` in the string format of the Password Hashing Competition (PHC). `(user_id, method)` is unique. |
| `sessions` | `id`, `user_id`, `device_id` (optional), `access_token_hash`, `refresh_token_hash`, `previous_refresh_token_hash`, `created_at`, `access_expires_at`, `refresh_expires_at`, `absolute_expires_at`. Each hash is a SHA-256 of a token and has a unique index. `(user_id, device_id)` is unique where `device_id` is set. |

## Interface

- `SessionVerifier`, implemented for `platform::authn`.
- The domain type `Password` (D25).
- `Principal` is built here only after a successful credential check or a registration.

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `POST /v1/users` | public | AUTH-001 to AUTH-003, AUTH-007, AUTH-008 |
| `POST /v1/sessions` | public | AUTH-004 to AUTH-008 |
| `POST /v1/sessions/refresh` | public, with the refresh token as the credential | AUTH-010 to AUTH-012 |
| `DELETE /v1/sessions/current` | user | AUTH-013 |

All four routes take no `Idempotency-Key` (API-12).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| AUTH-001 | Registering with an email, a password and a name creates the user, a password identity and a session in one unit of work, and answers 201 with the user and the session's tokens. The audit entry is `user.registered`. | API-04, ARCH-11, DATA-27 | `auth_001_` |
| AUTH-002 | Registering an email already in use answers 409 `auth.email_in_use`, and writes nothing. | D7, SEC-05 | `auth_002_` |
| AUTH-003 | A password must be 8 to 128 characters. It is refused if it is on the common-password list (case-insensitive), is one repeated character, or contains a run of 8 or more ascending or descending digits. It isn't trimmed. | D25, API-10 | `auth_003_` |
| AUTH-004 | Signing in with a correct email and password creates a session, and answers 200 with the user and the session's tokens. The audit entry is `session.created`. | DATA-27 | `auth_004_` |
| AUTH-005 | An unknown email, an account without a password, and a wrong password all answer the same 401 `auth.invalid_credentials`, after the same password-hashing work. A wrong password for an existing account records `auth.sign_in_failed`. | SEC-05, DATA-27, PLATFORM-006 | `auth_005_` |
| AUTH-006 | Each email allows 5 sign-in attempts per 15 minutes, both configurable. Beyond that, a sign-in answers 429 before any lookup. Every attempt is counted, whether or not the email exists. | SEC-12, API-15, PLATFORM-026 | `auth_006_` |
| AUTH-007 | A session holds two independent 256-bit random tokens, returned once and stored only as SHA-256 hashes. An access token lasts 1 hour, and a refresh token 30 days. No session outlives its absolute lifetime of 90 days. All three durations are configurable. | SEC-03, SEC-06, D5 | `auth_007_` |
| AUTH-008 | When a sign-in carries a device ID that already has a session for this user, the old session ends through the full sign-out path (AUTH-013) in the same unit of work. Without a device ID, each sign-in creates a new session. | D5, D18 | `auth_008_` |
| AUTH-009 | A protected request is authenticated by its Bearer access token. A missing or unknown token answers 401 `request.unauthenticated`. An expired one answers 401 `request.access_token_expired`, so the client knows to refresh. | SEC-03, D18, D8 | `auth_009_` |
| AUTH-010 | Refreshing with the current refresh token issues a new pair. The old pair stops working, and the old refresh hash is kept as the previous one. The refresh expiry slides, but never past the absolute lifetime. | SEC-06, D5 | `auth_010_` |
| AUTH-011 | An expired refresh token, or a session past its absolute lifetime, answers 401 `auth.session_expired` and rotates nothing. | SEC-06 | `auth_011_` |
| AUTH-012 | Presenting the previous refresh token ends the session: it is deleted, an access-change signal is sent, and `session.reuse_detected` is recorded. The response is 401 `auth.invalid_refresh_token`, the same as for an unknown refresh token. | SEC-06, DATA-27 | `auth_012_` |
| AUTH-013 | Signing out deletes the current session, sends an access-change signal that closes its streams, records `session.ended`, and answers 204. | SEC-06, DATA-27, D35 | `auth_013_` |
| AUTH-014 | Passwords are hashed with Argon2id at the configured parameters that the Open Worldwide Application Security Project (OWASP) recommends, in PHC format, off the async runtime. A successful sign-in rehashes a password stored with older parameters. | SEC-04, CODE-05 | `auth_014_` |

## Design

### Registration

1. The unit of work opens for the public route.
2. `accounts::create` inserts the user, or reports `accounts.email_in_use`, which becomes AUTH-002.
3. The password identity and the session are inserted, and `user.registered` is recorded.
4. The unit of work commits, and the response carries the new user's tokens.

### Sign-in

1. The email's rate-limit bucket is counted in its own short transaction (AUTH-006, PLATFORM-026).
2. The unit of work opens. `accounts::find_by_email` returns the user's ID, if any, and the password identity is read by that user's ID.
3. The password is verified on a blocking thread. When there is no user or no password identity, a dummy hash computed at startup with the same parameters is verified instead (AUTH-005).
4. On a wrong password for an existing user, `auth.sign_in_failed` is recorded, and the unit of work commits and answers 401 (ARCH-11). With no user, the unit of work rolls back, and the answer is the same 401.
5. On success, a session with the same device ID ends (AUTH-008), the new session is created, `session.created` is recorded, the password is rehashed if needed (AUTH-014), and the unit of work commits.

### Refresh

The presented token is hashed with SHA-256, and the session is read by `refresh_token_hash`. If no session matches, it is read by `previous_refresh_token_hash`, and a match there is reuse (AUTH-012). Rotation updates the session's hashes with `WHERE refresh_token_hash = $presented`, so two concurrent refreshes can't both succeed.

### Device IDs

A device ID is an opaque string the client chooses, 1 to 64 characters from `[A-Za-z0-9_-]`. The desktop client generates one per installation.

## Open design questions

| Question | Settled by |
|---|---|
| Whether verifying the access token on every request needs a short in-process cache to keep database load acceptable | `auth_009_` under the slice's load test (OPS-18) |
