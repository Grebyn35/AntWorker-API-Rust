# Spec: `auth`

## Purpose

`auth` proves who a caller is. It owns sign-in methods and sessions, and it implements the `SessionVerifier` that `platform::authn` uses on every protected request. It serves both kinds of client (D61): one that sets its own headers, such as the desktop client, holds its tokens and sends the Bearer header, and a browser holds them in cookies it can't read. This spec covers registration and sign-in with a password, refreshing, reading and ending the current session, and authenticating requests. Email verification, password reset and change, OAuth, identity linking and second factors arrive with the full module.

## Owns

| Table | Holds |
|---|---|
| `identities` | A user's sign-in methods. `id`, `user_id`, `method`, `created_at`, `updated_at` and `version`. For a password: `method = 'password'`, and `password_hash` in the string format of the Password Hashing Competition (PHC). `(user_id, method)` is unique. |
| `sessions` | `id`, `user_id`, `device_id` (optional), `access_token_hash`, `refresh_token_hash`, `previous_refresh_token_hash`, `created_at`, `updated_at`, `version`, `access_token_expires_at`, `refresh_token_expires_at`, `expires_at`. Each hash is a SHA-256 of a token and has a unique index. `(user_id, device_id)` is unique where `device_id` is set. |

## Interface

- `SessionVerifier`, implemented for `platform::authn`.
- The domain type `Password` (D25).
- `Principal` is built here only, after a successful credential check or a registration: clippy bans its constructor, and `auth` alone allows it, through `#[expect]`.

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `POST /v1/users` | public | AUTH-001 to AUTH-003, AUTH-007, AUTH-008, AUTH-015, AUTH-017 |
| `POST /v1/sessions` | public | AUTH-004 to AUTH-008, AUTH-015, AUTH-017 |
| `POST /v1/sessions/refresh` | public, with the refresh token as the credential | AUTH-010 to AUTH-012, AUTH-015 |
| `GET /v1/users/me/sessions/current` | user | AUTH-016 |
| `DELETE /v1/users/me/sessions/current` | user | AUTH-013 |

No route takes an `Idempotency-Key` (API-12).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| AUTH-001 | Registering with an email, a password and a name creates the user, a password identity and a session in one unit of work, and answers 201 with `Location: /v1/users/me/sessions/current` and the session, as AUTH-016 reads it, with its tokens (AUTH-015). The audit entry is `user.registered`. | API-04, ARCH-11, DATA-27 | `auth_001_` |
| AUTH-002 | Registering an email already in use answers 409 `auth.email_in_use`, and writes nothing. | D7, SEC-05 | `auth_002_` |
| AUTH-003 | A password must be 8 to 128 characters, of any characters. It is refused if it is on the breached-password list, contains a word of the context list, is one repeated character, or contains a run of 8 or more ascending or descending digits, each compared without regard to case. It isn't trimmed or changed. | D25, API-10 | `auth_003_` |
| AUTH-004 | Signing in with a correct email and password creates a session, and answers 201 with `Location: /v1/users/me/sessions/current` and the session, as AUTH-016 reads it, with its tokens (AUTH-015). The audit entry is `session.created`. | API-04, DATA-27 | `auth_004_` |
| AUTH-005 | An unknown email, an account without a password, and a wrong password all answer the same 401 `auth.invalid_credentials`, after the same password-hashing work. A wrong password for an existing account records `sign_in.failed` in that account's log. | SEC-05, DATA-27, PLATFORM-006 | `auth_005_` |
| AUTH-006 | Each email allows 5 sign-in attempts per 15 minutes, both settings. Beyond that, a sign-in answers 429 `request.rate_limited` before any lookup. Every attempt is counted, whether or not the email exists. | SEC-12, API-15, PLATFORM-026 | `auth_006_` |
| AUTH-007 | A session holds two independent 256-bit random tokens, returned once and stored only as SHA-256 hashes. An access token lasts 1 hour, and a refresh token 30 days. No session outlives its absolute lifetime of 90 days. All three durations are settings. | SEC-03, SEC-06, D5 | `auth_007_` |
| AUTH-008 | When a sign-in carries a device ID that already has a session for this user, the old session ends through the full sign-out path (AUTH-013) in the same unit of work. The sign-ins of one user take turns, so concurrent ones on one device leave it one session. Without a device ID, each sign-in creates a new session. | D5, D18, DATA-25 | `auth_008_` |
| AUTH-009 | A protected request is authenticated by its access token: from the Bearer header, or, without an `Authorization` header, from the access cookie (AUTH-015). A missing or unknown token answers 401 `request.unauthenticated`. An expired one answers 401 `request.access_token_expired`, so the client knows to refresh. | SEC-03, D18, D61 | `auth_009_` |
| AUTH-010 | Refreshing with the current refresh token, from the body or else from the refresh cookie, issues a new pair, delivered as AUTH-015 says, and answers 200 with the session. The old pair stops working, and the old refresh hash is kept as the previous one. The refresh expiry slides, but never past the absolute lifetime. The audit entry is `session.refreshed`. | SEC-06, D5, DATA-27 | `auth_010_` |
| AUTH-011 | An expired refresh token, or a session past its absolute lifetime, answers 401 `auth.session_expired` and rotates nothing. | SEC-06 | `auth_011_` |
| AUTH-012 | Presenting the previous refresh token ends the session: it is deleted, an access-change signal is sent, and `session.reuse_detected` is recorded. The response is 401 `auth.invalid_refresh_token`, the same as for an unknown or missing refresh token. | SEC-06, DATA-27 | `auth_012_` |
| AUTH-013 | Signing out deletes the current session, sends an access-change signal that closes its streams, records `session.ended`, clears the session's cookies, and answers 204. | SEC-06, DATA-27, D35 | `auth_013_` |
| AUTH-014 | Passwords are hashed with Argon2id, at the minimum that the Open Worldwide Application Security Project (OWASP) recommends: 19 MiB of memory, 2 iterations and 1 degree of parallelism, in PHC format, off the async runtime. A successful sign-in rehashes a password stored with other parameters, and records `password.rehashed`. | SEC-04, CODE-05, DATA-27 | `auth_014_` |
| AUTH-015 | A registration, sign-in or refresh from a browser, a request that carries `Origin`, leaves the tokens out of the body and sets them as cookies: the access token in `__Host-access_token`, and the refresh token in `__Host-refresh_token`. Each is `Secure`, `HttpOnly` and `SameSite=Strict`, with `Path=/` and no `Domain`, and expires with its token. | SEC-19, D61 | `auth_015_` |
| AUTH-016 | `GET /v1/users/me/sessions/current` answers with the session that authenticated the request: its ID, its user's ID, its device ID, its creation time and its three expiry times, and no token. | API-02, DATA-22 | `auth_016_` |
| AUTH-017 | A device ID is a string the client chooses, of 1 to 64 letters, digits, underscores and hyphens, rather than a UUID the API issues (API-11). Any other answers 400 with `invalid_device_id` at `/device_id`. | API-10, API-11 | `auth_017_` |

## Design

### Registration

1. The password is hashed on a blocking thread, and the service opens the unit of work.
2. `accounts::create` inserts the user, or reports that the email is in use: the unit of work rolls back, and the answer is AUTH-002.
3. The password identity and the session are inserted, the `Principal` of the new session is built, and `user.registered` is recorded with its audit scope.
4. The unit of work commits, and the route delivers the new session's tokens (AUTH-015).

### Sign-in

1. The email's rate-limit bucket is counted in its own short transaction (AUTH-006, PLATFORM-026). Its key is a hash-based message authentication code (HMAC) of the normalized email (ACCOUNTS-002), so every spelling of an address shares one bucket.
2. The service opens the unit of work. `accounts::find_by_email` returns the user's ID, if any, and the password identity is read by that user's ID and locked until the unit of work ends, so the sign-ins of one user take turns (DATA-25): of two on one device, the second ends the session the first started (AUTH-008).
3. The password is verified on a blocking thread. When there is no user or no password identity, a dummy hash computed at startup with the same parameters is verified instead (AUTH-005).
4. On a wrong password for an existing user, `sign_in.failed` is recorded in that account's log through `AuditScope::attempt`, and the unit of work commits and answers 401 (ARCH-11). With no user, the unit of work rolls back, and the answer is the same 401.
5. On success, a session with the same device ID ends (AUTH-008), the new session is created, `session.created` is recorded, the password is rehashed if needed, which records `password.rehashed` (AUTH-014), and the unit of work commits.

### Refresh

The presented token is hashed with SHA-256, and the session is read by `refresh_token_hash`. If no session matches, it is read by `previous_refresh_token_hash`, and a match there is reuse (AUTH-012). Rotation updates the session's hashes with `WHERE refresh_token_hash = $presented`, so two concurrent refreshes can't both succeed. A refresh that reads its token from the cookie is a request the cookie authenticates, so the cross-site protection of PLATFORM-053 applies to it.

### Verifying access tokens

Every authenticated request reads its session by its access token's hash, with no cache, so a session that ends, by signing out or by a refresh token's reuse, refuses its token at once (D5, AUTH-013). A cache in each replica would need the access-change signals to drop what it holds, which the hub's listener receives from step 2.4 on. `just load-test me` measures what verifying costs (OPS-18).

### Token delivery

One function delivers a session's tokens for registration, sign-in and refresh (D18), in the body or as cookies. A request that carries `Origin` comes from a browser, since only a browser sends it (PLATFORM-053), and gets cookies, since a token in the body would be readable by any script on its page, while a cookie that is `HttpOnly` isn't. Any other client gets the tokens in the body. A refresh decides the same way, wherever its old token came from, so a browser never reads a token, even one it sends in the body. `__Host-` makes a browser refuse the cookie unless it is `Secure`, has `Path=/` and names no `Domain`, so only the API's own host can set it, and the browser sends it to that host alone. The refresh cookie therefore goes with every request to the API too, as the access cookie does, rather than only to the refresh route: requirement 3.3.3 of the Application Security Verification Standard (ASVS) asks for `__Host-` on every cookie that no other host shares, and `Path` can't narrow such a cookie. Only the refresh route reads it, and neither the API nor its proxy logs a cookie. Signing out sets both cookies again, empty and expired, which removes them from the browser.

A browser sends a cookie that is `SameSite=Strict` only with requests from the cookie's own site, so a web client is served from the same site as the API, such as `app.example.com` beside `api.example.com`, and its origin is listed in `CORS_ALLOWED_ORIGINS` (PLATFORM-052).

### Passwords

The breached-password list holds at least the 3,000 most common passwords of 8 to 128 characters, drawn from passwords that breaches revealed, as requirements 6.2.4 and 6.2.12 of ASVS ask. The context list holds the words a password of this product mustn't contain, such as the product's name, which `just rename` rewrites (ARCH-15); a fork adds its own, as requirements 6.1.2 and 6.2.11 ask. Both lists are compiled into the binary, so checking a password reaches no other system. No rule asks for a kind of character (6.2.5), and the password is verified exactly as the user typed it (6.2.8).

### Device IDs

A client generates its device ID once per installation, such as from random bytes in base64url, and sends it with every sign-in, so a sign-in on the same installation replaces its session (AUTH-008, AUTH-017).

### Second factors

Every user may add a second factor, and every superadmin must (D63). The factors arrive with the full module, and their kinds are chosen then. Sign-in is built in the first slice so that they add to it without changing what it answers a user who has none: a user who has one gets a challenge to complete, rather than the session's tokens.

## Open design questions

| Question | Settled by |
|---|---|
| How the per-email sign-in limit (AUTH-006) keeps an attacker who sends 5 attempts every 15 minutes from shutting an account's owner out of signing in with their password, as requirement 6.1.1 of ASVS asks the documentation to explain: such as by counting attempts per email and client IP, beside a looser count per email | the threat model, in step 3 |
