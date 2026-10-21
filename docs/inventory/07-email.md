# Inventory: Email (`MAIL`)

Reference: `AntWorker-API` @ `0210895` (2026-10-20). This file records what the old API does. It does not prescribe what the new one should do.

## Scope

MAIL covers transactional email: backend selection and SMTP transport (lettre), TLS, credentials, sender identity, timeouts, failure classification, the single-attempt delivery model, delivery metrics, and template rendering (MiniJinja, HTML and plain-text variants, variables, escaping, branding). It also records, per sending flow, the email-dispatch contract: recipient, sync or background, and whether a failed email fails the request.

Boundary: the endpoints that trigger email (register, resend verification, forgot/reset password, password change, account deletion, email change, invitations) are owned by AUTH, USER and WS. Their primary behavior (codes, sessions, audit, SSE) is catalogued there. Here only the email side of each is recorded (MAIL-019 to MAIL-028). Code generation and storage (`verification_codes`, `pending_email_changes`) belong to AUTH/DATA. Per-flow rate-limit buckets are enforced by AUTH/USER handlers and services. They are listed here only because they are the only limits on sending. Metric instruments, dashboards and alerts belong to OBS. Alertmanager's own SMTP receiver (`alertmanager-config.yml:13-17`) is OBS monitoring and unrelated to app email. `alertmanager-secrets/` and `.env` exist and were not opened.

Files read: `src/services/email_service.rs`, `src/services/email_template_service.rs`, `src/config/email.rs`, `src/models/email.rs`, `templates/email/*` (18 files), `src/services/traits.rs:1108-1361`, `src/server.rs` (wiring), `src/config/environment.rs` (email parsing/validation/status), `src/config/validation.rs` (email warning/status), `src/services/public_auth_service.rs` (register, verification, password reset), `src/services/user_service.rs` (delete, password change, email change), `src/services/workspace_member_service.rs` (invite, resend), `src/handlers/auth.rs`, `src/handlers/mod.rs`, `src/handlers/workspace_members.rs`, `src/errors/{service_error,api_error,response_envelope}.rs` (error mapping), `src/services/otel_metrics_service.rs` (email instruments), `src/app_state.rs`, `Cargo.toml`, `.env.example`, `Dockerfile`, `prometheus-alerts.yml` (email group), `grafana/provisioning/dashboards/app-email.json` (descriptions), and tests: inline modules of the files above, `tests/auth_session_fanout_live.rs`, `tests/user_sse_fanout_live.rs`, the `email_service: None` harnesses in `tests/*_http.rs`, `tests/prometheus/alert-rules.test.yml`.

## Behaviors

### MAIL-001 Email backend selection (smtp or disabled)

- **Behavior:** At startup `EMAIL_BACKEND` selects `smtp` (one shared lettre `AsyncSmtpTransport` built and injected into the auth, user and workspace-member services) or `disabled` (no transport; those services receive `None`). Default is `disabled`. Parsing is case-insensitive. Any other value aborts startup.
- **Kind:** `capability` (an operator can turn transactional email on or off without code changes).
- **Trigger:** startup (`server::run`).
- **Auth:** n/a.
- **Input & validation:** `EMAIL_BACKEND` ∈ {`smtp`,`disabled`} (any case); unset → `disabled`; other → `ConfigLoadError` "EMAIL_BACKEND must be 'disabled' or 'smtp', got '…'".
- **Effects:** INFO log "Email service initialized (SMTP transport with connection pooling)" with host, port, tls_mode, from_email, or "Email service disabled by configuration". Feature status `email` (enabled, mode `smtp`, host/port/tls_mode/from_email/connection_timeout_secs) or `email` disabled. Dependent feature statuses: `password_reset` and `email_change` are reported enabled only when backend is smtp.
- **Errors:** startup abort (process exit) on invalid value.
- **Guarantees:** One transport instance per process, shared via `Arc`; lettre `pool` feature enabled, pool parameters not configured (lettre defaults). No SMTP connection is attempted at startup; reachability and credentials are first exercised on the first send. No readiness check covers SMTP.
- **Source:** `src/config/environment.rs:281-293`, `src/config/environment.rs:528`, `src/config/email.rs:20-53`, `src/server.rs:1379-1409`, `src/server.rs:257,299,340`, `src/config/environment.rs:1695-1708`, `src/config/environment.rs:1739-1758`, `src/config/validation.rs:291-313`, `Cargo.toml:92-98`.
- **Tests:** `src/config/environment.rs` `backend_parsers_accept_known_reject_unknown_and_default_when_unset`.

### MAIL-002 SMTP startup validation

- **Behavior:** When backend is `smtp`, startup aborts unless SMTP_HOST and SMTP_FROM_EMAIL are non-empty, no SMTP_* value equals its `.env.example` placeholder, SMTP_PORT > 0, and 1 ≤ SMTP_CONNECTION_TIMEOUT_SECS ≤ 60. Transport construction then rejects an unparseable SMTP_FROM_EMAIL and TLS relay setup failures. Checks run in `Config::validate()` before any service is built, and again inside `SmtpEmailService::new`.
- **Kind:** `rule`.
- **Trigger:** startup.
- **Auth:** n/a.
- **Input & validation:** Placeholders rejected: `smtp.example.com`, `your-smtp-username`, `your-smtp-password`, `noreply@yourdomain.com`. SMTP_PORT is parsed as `u16` and SMTP_CONNECTION_TIMEOUT_SECS as `u64` even when backend is disabled, so a non-numeric value aborts startup regardless of backend. With backend disabled, presence, placeholder and range checks are skipped.
- **Effects:** stderr "FATAL: Configuration validation error: …" and exit. Constructor errors read "SMTP email service: …".
- **Errors:** startup abort with messages such as "SMTP_HOST is required when EMAIL_BACKEND=smtp", "SMTP_CONNECTION_TIMEOUT_SECS exceeds 60 seconds …", "SMTP email service: Invalid SMTP_FROM_EMAIL '…': …".
- **Guarantees:** A running SMTP-enabled instance always has a host and a syntactically valid sender address.
- **Source:** `src/config/environment.rs:42-45`, `src/config/environment.rs:213-224`, `src/config/environment.rs:527-540`, `src/config/environment.rs:966-1037`, `src/server.rs:852-855`, `src/services/email_service.rs:69-94`, `src/services/email_service.rs:104-116`.
- **Tests:** `src/config/environment.rs` `config_rejects_placeholder_smtp_host`, `…_smtp_username`, `…_smtp_password`, `…_smtp_from_email`, `config_accepts_real_smtp_credentials`, `config_smtp_placeholder_guards_inactive_when_email_disabled`, `config_rejects_smtp_host_none_when_smtp_enabled`, `config_rejects_smtp_from_email_none_when_smtp_enabled`, `config_rejects_zero_for_every_floor_guarded_field` (SMTP_PORT, SMTP_CONNECTION_TIMEOUT_SECS), `placeholder_consts_match_env_example`. No test for the >60 timeout bound found.

### MAIL-003 Email-dependent auth settings validated only with SMTP

- **Behavior:** `REQUIRE_EMAIL_VERIFICATION=true` with a non-smtp backend aborts startup. The verification-code, password-reset-code and email-change-code expiry (1-60 min) and max-attempts (>0) settings are validated only when the backend is smtp.
- **Kind:** `rule`.
- **Trigger:** startup.
- **Auth:** n/a.
- **Input & validation:** see Configuration. Error "EMAIL_BACKEND=smtp is required when REQUIRE_EMAIL_VERIFICATION=true".
- **Errors:** startup abort.
- **Source:** `src/config/environment.rs:1261-1327`.
- **Tests:** `src/config/environment.rs` `config_rejects_require_email_verification_without_smtp`, `config_accepts_require_email_verification_with_smtp`, `config_rejects_verification_code_zero_when_smtp_enabled_without_require`, `config_verification_code_guards_inactive_when_email_disabled`, `config_rejects_password_reset_code_bounds_when_smtp_enabled`, `config_rejects_email_change_code_bounds_when_smtp_enabled`.

### MAIL-004 SMTP TLS modes

- **Behavior:** `SMTP_TLS_MODE` selects `starttls` (lettre `starttls_relay`, default), `tls` (implicit TLS, lettre `relay`) or `none` (plaintext, `builder_dangerous`). TLS uses rustls. The connection port is always `SMTP_PORT` (default 587) whatever the mode. `none` with smtp backend produces a startup warning.
- **Kind:** `guarantee` (transport confidentiality of credentials and content).
- **Trigger:** startup.
- **Auth:** n/a.
- **Input & validation:** case-insensitive {`starttls`,`tls`,`none`}; unset → `starttls`; other value aborts startup even when backend is disabled.
- **Effects:** warning `ConfigValidationWarning` category `email`, field `SMTP_TLS_MODE`, value `none`: "SMTP_TLS_MODE=none — SMTP connections are unencrypted (credentials and email content sent in plaintext). Use starttls or tls for production deployments".
- **Errors:** startup abort on invalid value or relay construction failure.
- **Source:** `src/config/environment.rs:309-322`, `src/config/email.rs:78-107`, `src/services/email_service.rs:103-121`, `src/config/environment.rs:1584-1588`, `src/config/validation.rs:147-154`, `Cargo.toml:95`.
- **Tests:** `src/config/environment.rs` `validate_warns_on_plaintext_smtp_tls`, `backend_parsers_accept_known_reject_unknown_and_default_when_unset`.

### MAIL-005 SMTP authentication only when both credentials are set

- **Behavior:** SMTP AUTH credentials are attached only when both SMTP_USERNAME and SMTP_PASSWORD are set. If either is missing the transport connects unauthenticated, with no warning.
- **Kind:** `rule`.
- **Trigger:** startup (transport build).
- **Auth:** n/a.
- **Source:** `src/services/email_service.rs:126-129`, `src/config/environment.rs:531-532`.
- **Tests:** none found.

### MAIL-006 Sender identity

- **Behavior:** Every email has `From: [SMTP_FROM_NAME] <SMTP_FROM_EMAIL>`. The display name is omitted when SMTP_FROM_NAME is unset. No Reply-To, CC, BCC or custom headers are set by app code.
- **Kind:** `rule`.
- **Trigger:** every send.
- **Auth:** n/a.
- **Input & validation:** SMTP_FROM_NAME is not validated.
- **Source:** `src/services/email_service.rs:96`, `src/services/email_service.rs:151`, `src/config/email.rs:170-180`.
- **Tests:** none found.

### MAIL-007 Message assembly (single recipient, multipart/alternative)

- **Behavior:** Each email has one `To` mailbox: the recipient address plus, when known, their display name. Subject is a fixed English string per template. When a plain-text body is present the body is `multipart/alternative` with `text/plain` first and `text/html` second, otherwise a single `text/html` part. Every production renderer sets a plain-text body, so every production email is multipart.
- **Kind:** `guarantee` (every email renders in text-only clients).
- **Trigger:** every send.
- **Auth:** n/a.
- **Effects:** none beyond the wire message.
- **Errors:** message-build failure → `INTERNAL_SERVER_ERROR` 500 (internal code `email_failure`, "Failed to build email message").
- **Source:** `src/services/email_service.rs:149-220`, `src/services/email_template_service.rs:271-276`, `src/services/email_template_service.rs:311-317` (and each renderer's `EmailMessage` literal), `src/models/email.rs:9-65`.
- **Tests:** `src/services/email_service.rs` `build_message_assembles_multipart_alternative_plain_then_html`, `build_message_html_only_when_no_plain_text`.

### MAIL-008 Malformed recipient address

- **Behavior:** If a recipient address fails lettre parsing, the send fails before any network I/O with `INTERNAL_SERVER_ERROR` 500 (internal code `email_failure`). The error message carries only the parse-error shape, never the address.
- **Kind:** `hardening`.
- **Trigger:** every send.
- **Auth:** n/a.
- **Effects:** no metric is recorded (the error returns before the timed send).
- **Errors:** `INTERNAL_SERVER_ERROR` → 500 (sync flows only; best-effort flows swallow it).
- **Guarantees:** no PII in the error message.
- **Source:** `src/services/email_service.rs:153-180`, `src/services/email_service.rs:244`.
- **Tests:** `src/services/email_service.rs` `malformed_recipient_maps_to_email_failure_without_leaking_the_address`.

### MAIL-009 SMTP failure classification

- **Behavior:** A lettre send error that is permanent (`is_permanent()`, SMTP 5xx) or TLS (`is_tls()`) becomes `ApiError::Internal` (internal code `email_failure`, wire `INTERNAL_SERVER_ERROR` 500). Every other error (4xx, timeout, connection) becomes `ApiError::ServiceUnavailable` (internal code `smtp_temporary_failure`, wire `SERVICE_UNAVAILABLE` 503). The wire messages are the generic "An unexpected error occurred" / "Service temporarily unavailable". The lettre error text appears only in logs.
- **Kind:** `guarantee` (clients of the synchronous flows can tell a retryable 503 from a 500).
- **Trigger:** every send.
- **Auth:** n/a.
- **Errors:** as above. Surfaced to the client only by MAIL-020 and MAIL-024.
- **Source:** `src/services/email_service.rs:248-297`, `src/errors/service_error.rs:84-92`, `src/errors/service_error.rs:118-120`, `src/errors/api_error.rs:1025-1038`, `src/errors/response_envelope.rs:243-244`.
- **Tests:** `src/services/email_service.rs` `unreachable_smtp_host_yields_transient_service_unavailable`. Permanent/TLS branch: none found.

### MAIL-010 Single-attempt, in-process, unqueued delivery

- **Behavior:** Each send is exactly one SMTP attempt made in the requesting process. There is no retry, no queue or outbox table, and no persistence of unsent mail. All sends are awaited inline on the request path except the password-reset code, which uses a detached `tokio::spawn` (MAIL-021). The only time bound is SMTP_CONNECTION_TIMEOUT_SECS, passed to lettre's builder `.timeout()`.
- **Kind:** `guarantee` (delivery is at most once; a crash or failure between commit and send loses the email).
- **Trigger:** every send.
- **Auth:** n/a.
- **Guarantees:** At most once. Not coordinated across instances, and needs no coordination. The spawned reset task is not registered with any shutdown/drain mechanism. Replays with an `Idempotency-Key` header are governed by the HTTP idempotency middleware on every route group (HTTP domain).
- **Source:** `src/services/email_service.rs:120-124`, `src/services/email_service.rs:252-254`, `src/services/public_auth_service.rs:1973-1988`, `src/services/traits.rs:1127-1146`, `src/server.rs:594-654`.
- **Tests:** none found specific to absence of retry.

### MAIL-011 Delivery metrics and tracing

- **Behavior:** A successful send increments `email_sent_total{email.type}` and records `email_send_duration_seconds{email.type}` (time around `transport.send()`, buckets 0.1-60 s). A transport failure increments `email_errors_total{email.type, error.type=permanent|transient}`. Each send runs in span `service_email_send` with `email_type` and `recipient_count`. Success logs at DEBUG only.
- **Kind:** `guarantee` (operational visibility of the SMTP dependency).
- **Trigger:** every send.
- **Auth:** n/a.
- **Effects:** metrics as above (no-op when OTel metrics are disabled). `email.type` values are the nine `email_type` strings in the Template catalog. Pre-transport failures (address parse, build, render) record no metric.
- **Source:** `src/services/email_service.rs:230-297`, `src/services/otel_metrics_service.rs:18-20`, `src/services/otel_metrics_service.rs:138-153`, `src/services/otel_metrics_service.rs:251-277`.
- **Tests:** `src/services/otel_metrics_service.rs` `latency_histograms_export_seconds_boundaries`. `tests/prometheus/alert-rules.test.yml` (HighEmailErrorRate, HighEmailSendLatency) covers the alert rules, not the code.

### MAIL-012 Templates embedded and parsed at startup

- **Behavior:** All 18 template files (9 templates × `.html`/`.txt`) are compiled into the binary with `include_str!` and parsed into one MiniJinja `Environment` when the app state is built. A syntax error aborts startup with "Email template service: Failed to load template '<name>': …". The template service is built even when the email backend is disabled.
- **Kind:** `guarantee` (single-binary deployment; no template can fail to load at request time).
- **Trigger:** startup (`build_app_state`).
- **Auth:** n/a.
- **Effects:** INFO log "Email template service initialized (MiniJinja with embedded templates)" with `template_count = 8`.
- **Source:** `src/services/email_template_service.rs:50-232`, `src/server.rs:199-208`, `Dockerfile:29-33`.
- **Tests:** `src/services/email_template_service.rs` `new_parses_all_embedded_templates`.

### MAIL-013 HTML auto-escaping, raw plain text

- **Behavior:** Variables are HTML-escaped in `.html` templates (MiniJinja extension-based autoescape) and inserted raw in `.txt` templates. User-controlled values (inviter display name, workspace name, recipient name, new email) therefore cannot inject markup into the HTML part.
- **Kind:** `guarantee` (XSS-safe HTML bodies).
- **Trigger:** every render.
- **Auth:** n/a.
- **Guarantees:** Escaping depends on the registered template names ending in `.html`. Subject lines are built in Rust (`format!`) and are not escaped by app code. Header encoding is left to lettre.
- **Source:** `src/services/email_template_service.rs:51`, `src/services/email_template_service.rs:71-229`, `src/services/email_template_service.rs:395`.
- **Tests:** `src/services/email_template_service.rs` `html_output_escapes_user_controlled_input_while_plain_text_preserves_raw`.

### MAIL-014 Branding global `app_name`

- **Behavior:** `APP_DISPLAY_NAME` (default "App") is registered as the global `{{ app_name }}` for every template. Only `account_deletion_confirmation` and `workspace_invitation` reference it. No subject includes it.
- **Kind:** `capability` (an operator can brand email copy).
- **Trigger:** startup / every render.
- **Auth:** n/a.
- **Input & validation:** no validation of APP_DISPLAY_NAME.
- **Source:** `src/config/environment.rs:72-77`, `src/config/environment.rs:344`, `src/services/email_template_service.rs:52`, `templates/email/account_deletion_confirmation.html:20,22`, `templates/email/workspace_invitation.html:22`.
- **Tests:** none found asserting `app_name` output.

### MAIL-015 Greeting and recipient personalization

- **Behavior:** Every template greets "Hi {{ recipient_name }}," when a name is supplied and is truthy, otherwise "Hi,". The same optional name becomes the `To` display name. All flows pass the account's display name, except invitations to an address with no account, which pass none.
- **Kind:** `rule`.
- **Trigger:** every render.
- **Auth:** n/a.
- **Source:** `src/services/email_template_service.rs:271-276`, `templates/email/*.html:14-18`, `templates/email/*.txt:3`, `src/services/workspace_member_service.rs:1037`.
- **Tests:** `src/services/email_template_service.rs` `absent_recipient_name_still_renders`.

### MAIL-016 Content model: in-app codes, no links, English only

- **Behavior:** No email contains a URL or action link. Verification, reset and email-change emails show a 6-digit code to type into the app. The invitation tells the user to open the desktop application. All copy and subjects are hard-coded English (`lang="en"`), with no localization, pluralization or support-contact variable. Expiry text is "expire in {N} minutes" / "expire in {N} days" using the configured value.
- **Kind:** `rule`.
- **Trigger:** every render.
- **Auth:** n/a.
- **Source:** `templates/email/*`, `src/services/email_template_service.rs:289-637`, `src/auth/verification_code_generator.rs:22-33`.
- **Tests:** `src/services/email_template_service.rs` `password_reset_embeds_code_and_pins_subject_and_type`, `email_verification_embeds_code_and_pins_subject_and_type`, `email_change_verification_embeds_code_and_pins_subject_and_type`, `email_change_notice_embeds_new_email_and_pins_subject_and_type`.

### MAIL-017 Fixed subjects and email_type labels

- **Behavior:** Each renderer sets a fixed subject and an `email_type` label (see Template catalog). The invitation subject interpolates the workspace name: "You've Been Invited to {workspace_name}".
- **Kind:** `rule`.
- **Trigger:** every render.
- **Auth:** n/a.
- **Source:** `src/services/email_template_service.rs:313,316,353,356,395,398,434,437,473,476,512,515,555,558,593,596,632,635`.
- **Tests:** `src/services/email_template_service.rs` `workspace_invitation_pins_subject_and_type`, `confirmation_emails_pin_subject_and_type`, and the four `*_embeds_*` tests.

### MAIL-018 Render failure handling

- **Behavior:** A template lookup/render error becomes `INTERNAL_SERVER_ERROR` 500 (internal code `email_failure`, "Failed to render email template '<name>': …"). Synchronous mandatory flows (MAIL-020, MAIL-024) return it. Best-effort flows log `error_type = "email_template_render_failed"`, `action = "failing_open"` at WARN and continue. Forgot-password swallows it silently (MAIL-021).
- **Kind:** `hardening`.
- **Trigger:** every render.
- **Auth:** n/a.
- **Source:** `src/services/email_template_service.rs:239-268`, call sites listed per flow.
- **Tests:** `src/services/public_auth_service.rs` `request_password_reset_template_render_failure_silently_succeeds`.

### MAIL-019 Verification code email at credentials registration

- **Behavior:** When `REQUIRE_EMAIL_VERIFICATION=true`, a successful credentials registration then sends the `email_verification` email (6-digit code, VERIFICATION_CODE_EXPIRY_MINUTES) to the new user's stored address, awaited inline before the response. Any failure (including a disabled backend) is logged at DEBUG and swallowed, and registration still succeeds. With the flag false, registration sends no email.
- **Kind:** `capability`.
- **Trigger:** `POST /auth/register`.
- **Auth:** public.
- **Input & validation:** registration validation is AUTH's. The email step has no email-specific rate limit; only the generic per-IP tier (HTTP) applies.
- **Effects:** verification code upserted (replacing any earlier EmailVerification code for the user), email `email_verification`, metrics. DEBUG log "Verification email send failed during registration (non-critical)".
- **Errors:** none from the email step.
- **Guarantees:** Registration success does not depend on SMTP. The SMTP round-trip is on the request path. The duplicate-email branch returns without it.
- **Source:** `src/services/public_auth_service.rs:1110-1120`, `src/services/public_auth_service.rs:1638-1707`, `src/handlers/mod.rs:66`.
- **Tests:** none found for the registration-time send. Integration harnesses force `require_email_verification = false`.

### MAIL-020 Resend verification code email

- **Behavior:** An authenticated, still-unverified user can request a new verification code. The code is generated, upserted (invalidating the previous one), rendered and sent synchronously. Success returns `{ expires_at }`. A send failure is returned to the client, and the new code stays stored.
- **Kind:** `capability`.
- **Trigger:** `POST /auth/resend-verification`.
- **Auth:** session (user-scoped, verification-exempt group).
- **Input & validation:** Per-user bucket `verification_resend:{user_id}`, default 3 per 300 s (VERIFICATION_RESEND_RATE_LIMIT_*). Enforced only when ENFORCE_RATE_LIMITS is on. Fails open on limiter error. Order: rate limit → user lookup → already-verified check → email-service presence → code → upsert → render → send.
- **Effects:** `verification_codes` upsert (purpose EmailVerification, max_attempts VERIFICATION_CODE_MAX_ATTEMPTS). Email `email_verification` to the stored address with the stored name. INFO "Verification code sent".
- **Errors:** `RATE_LIMIT_EXCEEDED` → 429 → bucket exhausted. `VALIDATION_ERROR` → 400 → "Email is already verified" (checked before backend presence). `RESOURCE_NOT_FOUND` → 404 → user missing. `INTERNAL_SERVER_ERROR` → 500 → backend disabled ("Email service not configured — cannot send verification code"), render failure, permanent SMTP failure, bad address. `SERVICE_UNAVAILABLE` → 503 → transient SMTP failure.
- **Guarantees:** The response is sent only after the SMTP server accepts the message, or an error is returned.
- **Source:** `src/handlers/auth.rs:796-862`, `src/handlers/mod.rs:113`, `src/services/public_auth_service.rs:1638-1707`, `src/config/environment.rs:446-451`.
- **Tests:** `src/services/public_auth_service.rs` `send_verification_code_already_verified_rejects`, `send_verification_code_no_email_service_returns_internal_error`, `send_verification_code_success_persists_and_sends`.

### MAIL-021 Password reset code email (background, anti-enumeration)

- **Behavior:** For an address belonging to a user with a credentials identity, a reset code (PASSWORD_RESET_CODE_EXPIRY_MINUTES) is upserted and the `password_reset` email is rendered on the request path, then sent from a detached task that inherits the request span. The response is always 200 with message "If an account exists with this email, a password reset code has been sent.", whatever happens. Backend disabled, unknown email, OAuth-only user, DB error, render failure and send failure all return the same 200 with no email.
- **Kind:** `capability`.
- **Trigger:** `POST /auth/forgot-password`.
- **Auth:** public.
- **Input & validation:** Per-email bucket `password_reset` (HMAC of email), default 3 per 300 s (PASSWORD_RESET_RATE_LIMIT_*). Enforced when ENFORCE_RATE_LIMITS is on. Fails open. The email goes to the address stored on the account.
- **Effects:** `verification_codes` upsert (purpose PasswordReset). Email `password_reset`. INFO "Password reset code dispatched" (logged before the send completes). WARN `failing_open` logs per failure: `email_service_not_configured`, `user_lookup_failed`, `verification_code_persist_failed`, `email_template_render_failed`, and `email_send_failed` (logged inside the task).
- **Errors:** `RATE_LIMIT_EXCEEDED` → 429 only. No email error ever reaches the client.
- **Guarantees:** Body is uniform across branches. SMTP latency is removed from the response. The DB upsert and render still run only on the credentials-user branch. The send is fire-and-forget with no completion tracking.
- **Source:** `src/handlers/auth.rs:899-971`, `src/handlers/mod.rs:70`, `src/services/public_auth_service.rs:1814-1997`, `src/config/environment.rs:454-459`.
- **Tests:** `src/services/public_auth_service.rs` `request_password_reset_no_email_service_silently_succeeds`, `request_password_reset_unknown_email_silently_succeeds`, `request_password_reset_oauth_user_silently_succeeds`, `request_password_reset_db_error_silently_succeeds`, `request_password_reset_code_persist_failure_silently_succeeds`, `request_password_reset_email_send_failure_silently_succeeds`, `request_password_reset_success_sends_one_email`, `request_password_reset_template_render_failure_silently_succeeds`.

### MAIL-022 Password reset confirmation email

- **Behavior:** After a successful reset (password updated, all sessions deleted, audit committed, `session.revoked` SSE attempted), the `password_reset_confirmation` email goes to the account's address. It is awaited inline, best-effort, and skipped when the backend is disabled. Failures are logged at WARN and the reset still returns `{ password_reset: true }`.
- **Kind:** `guarantee` (out-of-band notice of a credential change; chosen over `capability` because the user cannot request it independently).
- **Trigger:** `POST /auth/reset-password` (success path).
- **Auth:** public.
- **Effects:** email `password_reset_confirmation`. WARN `email_send_failed` / `email_template_render_failed` with `action = "failing_open"`.
- **Errors:** none from the email step.
- **Guarantees:** Sent strictly after commit. Never sent for a failed reset.
- **Source:** `src/services/public_auth_service.rs:2236-2267`, `src/handlers/mod.rs:71`.
- **Tests:** `tests/auth_session_fanout_live.rs` `reset_password_succeeds_when_confirmation_email_fails`.

### MAIL-023 Password change confirmation email

- **Behavior:** After a successful settings password change (other sessions revoked, audit committed, SSE attempted), the `password_change_confirmation` email goes to the address and name on the caller's `AuthContext`. It is awaited inline and best-effort, and the change still succeeds if the email fails.
- **Kind:** `guarantee` (out-of-band credential-change notice).
- **Trigger:** `PUT /users/me/password` (success path).
- **Auth:** session (user-scoped, verification enforced). Credentials identity required.
- **Effects:** email `password_change_confirmation`. WARN `failing_open` on failure.
- **Errors:** none from the email step.
- **Guarantees:** Sent after commit only.
- **Source:** `src/services/user_service.rs:1239-1267`, `src/handlers/mod.rs:184`.
- **Tests:** `tests/user_sse_fanout_live.rs` `change_password_succeeds_when_confirmation_email_fails`.

### MAIL-024 Account deletion confirmation email

- **Behavior:** After self-service account deletion commits (and post-commit audit and SSE fan-out), the `account_deletion_confirmation` email goes to the email and name read before the cascade. It is awaited inline and best-effort, and the deletion still succeeds if the email fails.
- **Kind:** `guarantee` (out-of-band notice of an irreversible action).
- **Trigger:** `POST /users/me/delete` (success path).
- **Auth:** session (verification-exempt group). Current password required for credentials accounts.
- **Effects:** email `account_deletion_confirmation`. WARN `failing_open` on failure. No email goes to co-members of cascade-deleted workspaces or to invitees of cascade-cancelled invitations (they receive SSE only).
- **Errors:** none from the email step.
- **Source:** `src/services/user_service.rs:675-684`, `src/services/user_service.rs:1006-1035`, `src/handlers/mod.rs:145`.
- **Tests:** none found (unit and integration harnesses pass `email_service: None` for deletion).

### MAIL-025 Email change verification code to the new address

- **Behavior:** Requesting an email change stages the encrypted new address and a 6-digit code (EMAIL_CHANGE_CODE_EXPIRY_MINUTES), then synchronously sends `email_change_verification` to the new, user-supplied address with the caller's display name. A send failure is returned to the client after the staging has already committed. Success returns `{ expires_at }`.
- **Kind:** `capability`.
- **Trigger:** `POST /users/me/email`.
- **Auth:** session (verification-exempt group). Current password required if the account has a credentials identity. Rejected if supplied for OAuth-only accounts.
- **Input & validation:** Per-user bucket `email_change:{user_id}`, default 3 per 300 s (EMAIL_CHANGE_RATE_LIMIT_*). Fails open. Order: rate limit → backend presence → same-as-current → credential gate → cross-account collision → stage → render → send.
- **Effects:** `initiate_email_change` (pending row + code, AUTH/DATA). Email `email_change_verification`. Then MAIL-026. INFO "Email change requested — verification code dispatched to new address".
- **Errors:** `RATE_LIMIT_EXCEEDED` → 429. `INTERNAL_SERVER_ERROR` → 500 → backend disabled ("Email service not configured — cannot change email", checked before every business rule), render failure, permanent SMTP failure. `SERVICE_UNAVAILABLE` → 503 → transient SMTP failure. `VALIDATION_ERROR` → 400 → new email equals current. `AUTHENTICATION_FAILED` → 401 → password missing, wrong or not applicable. `RESOURCE_CONFLICT` → 409 → address used by another account.
- **Guarantees:** The code reaches only the new address. The response waits for SMTP acceptance.
- **Source:** `src/handlers/auth.rs:1095-1116`, `src/handlers/mod.rs:123`, `src/services/user_service.rs:1289-1468`, `src/config/environment.rs:478-483`.
- **Tests:** `src/services/user_service.rs` `request_email_change_rejects_when_rate_limit_exceeded`, `request_email_change_no_email_service_returns_internal_error`, `request_email_change_oauth_only_with_password_rejected`, `request_email_change_credentials_without_password_rejected`, `request_email_change_credentials_wrong_password_rejected`, `request_email_change_same_email_rejected_before_lookup`, `request_email_change_credentials_success_persists_and_sends_both`, `request_email_change_rejects_address_owned_by_another_account`.

### MAIL-026 Email change security notice to the old address

- **Behavior:** After the verification email to the new address succeeds, `email_change_notice` (naming the requested new address) goes to the current address on the `AuthContext`. It is awaited inline and best-effort, and a failure does not fail the request.
- **Kind:** `guarantee` (anti-takeover signal to the current owner).
- **Trigger:** `POST /users/me/email` (after MAIL-025 succeeds).
- **Auth:** as MAIL-025.
- **Effects:** email `email_change_notice`. WARN `failing_open` on failure. Not sent if the verification send failed.
- **Errors:** none from this step.
- **Source:** `src/services/user_service.rs:1470-1498`.
- **Tests:** `src/services/user_service.rs` `request_email_change_credentials_success_persists_and_sends_both`.

### MAIL-027 Email change confirmation to the new address

- **Behavior:** After the change is committed (address promoted, audit written), `email_change_confirmation` goes to the new address with the caller's display name. It is awaited inline, best-effort, and skipped when the backend is disabled. No post-change notice goes to the old address.
- **Kind:** `guarantee` (post-action notice).
- **Trigger:** `POST /users/me/email/confirm` (success path).
- **Auth:** session (verification-exempt group).
- **Effects:** email `email_change_confirmation`. WARN `failing_open` on failure. It is sent before the `identity.email_changed` SSE publish.
- **Errors:** none from the email step.
- **Source:** `src/services/user_service.rs:1602-1632`, `src/handlers/mod.rs:124`.
- **Tests:** `tests/user_sse_fanout_live.rs` `confirm_email_change_succeeds_when_confirmation_email_fails`. `tests/email_change_confirm_http.rs` runs with `email_service: None` (disabled skip path).

### MAIL-028 Workspace invitation email (create and resend)

- **Behavior:** Creating an invitation or resending one sends `workspace_invitation` to the invited address, whether or not it has an account. The email names the inviter (caller's display name), the workspace, and INVITATION_EXPIRY_DAYS. It is personalized with the invitee's account name when an account exists. It is awaited inline and best-effort: render and send failures are logged at WARN and the invitation is still returned 200. Each resend sends another email.
- **Kind:** `capability`.
- **Trigger:** `POST /workspace/invitations`, `POST /workspace/invitations/{invitation_id}/resend`.
- **Auth:** workspace-scoped. `InviteMember` permission. Create also enforces the grant-subset rule.
- **Input & validation:** WS-owned (existing-member conflict, permission parsing). No email-specific or per-recipient rate limit. Only the generic per-user tier (HTTP) applies.
- **Effects:** invitee account lookup by email (only when email or SSE is enabled; lookup error → no name, DEBUG log). Email `workspace_invitation` sent after the invitation write and audit and before the `invitation.received` SSE.
- **Errors:** none from the email step.
- **Guarantees:** Invitation creation and resend never fail because of email.
- **Source:** `src/services/workspace_member_service.rs:1007-1066`, `src/services/workspace_member_service.rs:1487-1543`, `src/handlers/workspace_members.rs:239-250`, `src/handlers/workspace_members.rs:349-360`, `src/handlers/mod.rs:372-387`.
- **Tests:** `src/services/workspace_member_service.rs` `invite_member_success_audits_and_fans_out`, `invite_member_succeeds_when_email_send_fails`, `invite_member_skips_sse_when_invitee_has_no_account`, `resend_invitation_succeeds_when_email_send_fails`.

### MAIL-029 Disabled backend: per-flow outcome

- **Behavior:** With `EMAIL_BACKEND=disabled` no email is ever sent. Best-effort sends (MAIL-019, 022, 023, 024, 027, 028) are skipped silently. Forgot-password returns its normal 200 (WARN log `email_service_not_configured`). Resend-verification and email-change request return 500 `INTERNAL_SERVER_ERROR`. `GET /auth/config` exposes only `require_email_verification`, not email availability.
- **Kind:** `rule`.
- **Trigger:** each email-sending endpoint, with backend disabled.
- **Auth:** per endpoint.
- **Errors:** `INTERNAL_SERVER_ERROR` → 500 → `POST /auth/resend-verification` (unless already verified, which gives 400 first), `POST /users/me/email` (before any other check except the rate limit).
- **Source:** `src/services/public_auth_service.rs:1656-1661`, `src/services/public_auth_service.rs:1856-1866`, `src/services/user_service.rs:1343-1348`, `src/handlers/auth.rs:661-704`, and each best-effort `if let Some(ref email_service)` site listed above.
- **Tests:** `send_verification_code_no_email_service_returns_internal_error`, `request_password_reset_no_email_service_silently_succeeds`, `request_email_change_no_email_service_returns_internal_error`. The `tests/*_http.rs` harnesses run with `email_service: None`.

## Template catalog

All templates live in `templates/email/` as `<name>.html` + `<name>.txt`, receive the global `app_name`, and produce one `To` recipient. "Sync" means awaited on the request path.

| Template | `email_type` / subject | Sent by (`file:line`) | Recipient | Variables | When sent | Failure handling |
|---|---|---|---|---|---|---|
| `email_verification` | `email_verification` / "Verify Your Email Address" | `src/services/public_auth_service.rs:1691-1698` (`send_verification_code`), called from `public_auth_service.rs:1113` (register) and `src/handlers/auth.rs:856` (resend) | stored account email, account name | `recipient_name`, `verification_code`, `expires_in_minutes` | registration when REQUIRE_EMAIL_VERIFICATION=true; `POST /auth/resend-verification` | register: sync, swallowed (DEBUG). Resend: sync, error returned (500/503) |
| `password_reset` | `password_reset` / "Reset Your Password" | `src/services/public_auth_service.rs:1940-1988` | stored account email, account name | `recipient_name`, `reset_code`, `expires_in_minutes` | `POST /auth/forgot-password` for a user with a credentials identity | background `tokio::spawn`; always 200; WARN on failure |
| `password_reset_confirmation` | `password_reset_confirmation` / "Your Password Has Been Changed" | `src/services/public_auth_service.rs:2241-2267` | stored account email, account name | `recipient_name` | successful `POST /auth/reset-password` | sync after commit, best-effort (WARN) |
| `password_change_confirmation` | `password_change_confirmation` / "Your Password Has Been Changed" | `src/services/user_service.rs:1241-1267` | `AuthContext` email, name | `recipient_name` | successful `PUT /users/me/password` | sync after commit, best-effort (WARN) |
| `account_deletion_confirmation` | `account_deletion_confirmation` / "Your Account Has Been Deleted" | `src/services/user_service.rs:1009-1035` | pre-cascade account email, name | `recipient_name`, `app_name` (used) | successful `POST /users/me/delete` | sync after commit, best-effort (WARN) |
| `email_change_verification` | `email_change_verification` / "Confirm Your New Email Address" | `src/services/user_service.rs:1460-1468` | requested new address, caller name | `recipient_name`, `verification_code`, `expires_in_minutes` | `POST /users/me/email` | sync, error returned (500/503) after staging committed |
| `email_change_notice` | `email_change_notice` / "Email Change Requested" | `src/services/user_service.rs:1473-1498` | current (old) address, caller name | `recipient_name`, `new_email` | `POST /users/me/email`, after the verification send succeeds | sync, best-effort (WARN) |
| `email_change_confirmation` | `email_change_confirmation` / "Your Email Address Has Been Changed" | `src/services/user_service.rs:1606-1632` | new (now active) address, caller name | `recipient_name` | successful `POST /users/me/email/confirm` | sync after commit, best-effort (WARN) |
| `workspace_invitation` | `workspace_invitation` / "You've Been Invited to {workspace_name}" | `src/services/workspace_member_service.rs:1036-1066` (create), `:1513-1543` (resend) | invited address; invitee account name if one exists, else none | `recipient_name`, `workspace_name`, `inviter_name`, `expires_in_days`, `app_name` (used) | `POST /workspace/invitations`; `POST /workspace/invitations/{id}/resend` | sync, best-effort (WARN) |

Every template has a production sender. None exists without being sent.

Flows that send no email (factual, verified by absence of an email service in `IdentityServiceImpl`, `SessionServiceImpl`, `WorkspaceServiceImpl`, `UserInvitationServiceImpl` (`src/server.rs:308-352`) and by the call-site grep):
- OAuth registration and login, and credentials registration when REQUIRE_EMAIL_VERIFICATION=false (no welcome email).
- Successful email verification (`POST /auth/verify-email`).
- New sign-in or new device, and session revocation (`/users/me/sessions*`).
- Linking or unlinking credentials or OAuth identities (`/auth/identities/*`), including adding a password to an OAuth account.
- Completed email change: no notice to the old address (it is notified only at request time).
- Invitation updated (`PUT /workspace/invitations/{id}`) or cancelled (`DELETE …`). Invitation accepted or declined (no notice to the inviter).
- Member removed, member left, member permissions changed, workspace ownership transferred, workspace deleted.
- Account deletion cascades: co-members of deleted owned workspaces and invitees of cascade-cancelled invitations get SSE only.
- Profile changes (`PUT /users/me`).

## Data owned

- No tables, RLS policies, stored procedures or triggers. Codes and pending email changes that emails carry are AUTH/DATA-owned (`verification_codes`, `pending_email_changes`).
- Template assets: `templates/email/*.html|.txt` (18 files), embedded at compile time (`src/services/email_template_service.rs:70-229`). The Docker build copies them (`Dockerfile:33`).
- Background work: none scheduled. One ad-hoc detached task per forgot-password request (`src/services/public_auth_service.rs:1975-1988`).

## Configuration

| Variable | Default | Validation |
|---|---|---|
| `EMAIL_BACKEND` | `disabled` | case-insensitive `smtp`/`disabled`; else startup abort (`src/config/environment.rs:281-293`) |
| `SMTP_HOST` | unset | required non-empty and ≠ `smtp.example.com` when smtp (`environment.rs:968-996`) |
| `SMTP_PORT` | 587 (`src/config/email.rs:191-193`) | `u16` parse always; > 0 when smtp (`environment.rs:1021-1025`) |
| `SMTP_USERNAME` | unset | ≠ `your-smtp-username` when smtp. Used only if `SMTP_PASSWORD` is also set |
| `SMTP_PASSWORD` | unset | ≠ `your-smtp-password` when smtp. Used only if `SMTP_USERNAME` is also set |
| `SMTP_TLS_MODE` | `starttls` | case-insensitive `starttls`/`tls`/`none`, parsed always; `none` warns when smtp (`environment.rs:309-322,1584-1588`) |
| `SMTP_FROM_EMAIL` | unset | required non-empty, ≠ `noreply@yourdomain.com`, must parse as an address at transport build (`environment.rs:974-1018`, `email_service.rs:86-94`) |
| `SMTP_FROM_NAME` | unset (address only) | none |
| `SMTP_CONNECTION_TIMEOUT_SECS` | 10 (`src/config/email.rs:197-199`) | `u64` parse always; 1-60 when smtp (`environment.rs:1028-1036`) |
| `APP_DISPLAY_NAME` | `App` | none (`environment.rs:344`) |

Read by email flows but owned by other domains: `REQUIRE_EMAIL_VERIFICATION` (false; needs smtp), `VERIFICATION_CODE_EXPIRY_MINUTES` (15), `PASSWORD_RESET_CODE_EXPIRY_MINUTES` (15), `EMAIL_CHANGE_CODE_EXPIRY_MINUTES` (15) (each 1-60, checked only when smtp), the matching `*_MAX_ATTEMPTS` (5), `INVITATION_EXPIRY_DAYS` (7, 1-90), `VERIFICATION_RESEND_RATE_LIMIT_MAX_ATTEMPTS/WINDOW_SECS` (3/300), `PASSWORD_RESET_RATE_LIMIT_*` (3/300), `EMAIL_CHANGE_RATE_LIMIT_*` (3/300), `ENFORCE_RATE_LIMITS` (`src/config/environment.rs:363-401`, `:446-483`).

## Cross-domain dependencies

- Calls out to OBS: `MetricsService::record_email_sent` / `record_email_error` (`src/services/email_service.rs:260-261,292-293`). Tracing spans `service_email_send` and `service_email_template_render_*`. lettre's own `tracing` feature (`Cargo.toml:97`). Consumed by `prometheus-alerts.yml:527-613` (HighEmailErrorRate, HighEmailSendLatency) and `grafana/provisioning/dashboards/app-email.json`.
- Depends on OPS: `Config::from_env` / `Config::validate` (`src/server.rs:842-855`). Startup wiring in `src/server.rs:199-208,1379-1409`. `AppResources.email_service` injection for tests (`src/server.rs:44`).
- Depends on HTTP: error envelope mapping of `ApiError::Internal` / `ServiceUnavailable` (`src/errors/api_error.rs:1025-1038`). Idempotency and rate-limit middleware on all route groups.
- Called by AUTH: `PublicAuthServiceImpl::send_verification_code`, `register_credentials_user`, `request_password_reset`, `reset_password` (`src/services/public_auth_service.rs`).
- Called by USER: `UserServiceImpl::delete_user`, `change_password`, `request_email_change`, `confirm_email_change` (`src/services/user_service.rs`).
- Called by WS: `WorkspaceMemberServiceImpl::invite_member`, `resend_invitation` (`src/services/workspace_member_service.rs`).
- Uses AUTH: `generate_verification_code` (6-digit, `src/auth/verification_code_generator.rs:22-33`). Per-flow rate-limit buckets via `RateLimitService`.

## Observations

1. **Startup log undercounts templates.** `template_count = 8` is logged, but 9 templates (18 files) are registered (`src/server.rs:206` vs `src/services/email_template_service.rs:70-229`). The log misreports what is loaded.
2. **Metrics comment undercounts email types.** It says `email.type` is "bounded by template count (4 types)", but 9 values exist (`src/services/otel_metrics_service.rs:136`). This matters for cardinality expectations.
3. **Trait doc contradicts call sites.** It says to "Await result for critical emails (password reset, email verification)" and use "`.ok()` for best-effort notifications (workspace invitations)" (`src/services/traits.rs:1138-1141`). In code, password reset is spawned fire-and-forget (`src/services/public_auth_service.rs:1975`) and best-effort sites use `if let Err` + WARN, not `.ok()`.
4. **No email field in AppState.** Docs say the EmailService "is None in AppState" (`src/config/email.rs:36`, `src/app_state.rs:72`), but `AppState` has no email field (`src/app_state.rs:22-113`). The service is injected into three services only.
5. **SMTP credentials documented as required but not enforced.** `SMTP_USERNAME`/`SMTP_PASSWORD` are documented "Required when EMAIL_BACKEND=smtp" (`src/config/email.rs:149,158`, `.env.example:836,845`). `validate()` does not require them (`src/config/environment.rs:966-1037`), and the transport silently connects unauthenticated unless both are set (`src/services/email_service.rs:127`).
6. **No development or file transport.** A `Cargo.toml` comment claims "file-transport for development (saves .eml files without SMTP server)" (`Cargo.toml:90`), but the feature is not enabled (`Cargo.toml:92-98`) and no file or log backend exists (`src/config/email.rs:20-40`). The only dev path is `SMTP_TLS_MODE=none` against a local SMTP catcher. `docker-compose.yml` ships none.
7. **Port does not follow TLS mode.** The default port is 587 for every mode. `SMTP_TLS_MODE=tls` without `SMTP_PORT` makes an implicit-TLS connection to 587 (`src/config/email.rs:191`, `src/services/email_service.rs:111,121`), which contradicts the "Port 465" guidance in `src/config/email.rs:74,83`.
8. **SMTP parse errors abort startup even when email is disabled.** `SMTP_PORT`, `SMTP_CONNECTION_TIMEOUT_SECS` and `SMTP_TLS_MODE` are parsed unconditionally (`src/config/environment.rs:530,533,536-539`), so a malformed value aborts startup even with `EMAIL_BACKEND=disabled`. The comment "When EMAIL_BACKEND=disabled: All SMTP fields ignored" (`src/config/email.rs:114`) is not accurate.
9. **Pre-send failures leave no metric.** Address-parse, message-build and render failures record no `email_errors_total` (`src/services/email_service.rs:244`, `src/services/email_template_service.rs:239-268`). The Prometheus error-rate alert and the dashboard therefore cannot see them.
10. **Reset timing is only partly uniform.** The forgot-password docstring claims response time is "independent of which branch was taken" (`src/services/public_auth_service.rs:1823-1831`). Only SMTP is moved off-path. The credentials-user branch still runs an extra identity lookup, a DB upsert and a render (`:1888-1957`) that the other branches skip.
11. **Registration timing and logging differ from the reset flow.** With `REQUIRE_EMAIL_VERIFICATION=true`, registration awaits SMTP inline (`src/services/public_auth_service.rs:1112-1120`), while the duplicate-email branch returns right after a dummy Argon2 (`:1077-1086`). This is a wall-clock difference of the kind the reset flow was redesigned to avoid. Its failure log is DEBUG, while every other email failure site logs WARN `failing_open`.
12. **The spawned reset send is untracked.** It is not awaited or registered with shutdown (`src/services/public_auth_service.rs:1975`), so an in-flight reset email can be dropped on shutdown. "Password reset code dispatched" is logged at INFO before the send completes (`:1990-1994`).
13. **Email-change request writes before it sends.** It stages the pending change and code before the verification send (`src/services/user_service.rs:1443-1468`). A 500 or 503 send failure leaves a valid staged code the user never received, and the request has consumed one `email_change` rate-limit attempt. Resend-verification does the same: it upserts first (`src/services/public_auth_service.rs:1685-1698`), invalidating the previous code before the new one is delivered.
14. **Validation order differs between flows.** The backend-presence check precedes all business rules in `request_email_change` (`src/services/user_service.rs:1343` before `:1353`), while `send_verification_code` returns 400 "already verified" before its presence check (`src/services/public_auth_service.rs:1648` before `:1656`). A disabled backend surfaces as a 500, not a 4xx or 503.
15. **Long inline sends.** Best-effort sends are on the request path, so invite, resend, password change, reset, delete and email-change confirm each wait up to `SMTP_CONNECTION_TIMEOUT_SECS` (≤60 s) on a slow server. Email-change request sends two emails sequentially (`src/services/user_service.rs:1468,1479`).
16. **Copy inconsistencies.** The invitation HTML `<title>` is "Workspace Invitation" and its `<h1>` is "You're Invited", while the text heading is "You're Invited to {{ workspace_name }}" and the subject is "You've Been Invited to {workspace_name}" (`templates/email/workspace_invitation.html:6,12`, `.txt:1`, `src/services/email_template_service.rs:395`). The reset and change confirmations share the subject "Your Password Has Been Changed" (`email_template_service.rs:434,473`).
17. **No pluralization.** Expiry copy reads "1 minutes" or "1 days" at the allowed minimums (1 minute, 1 day) (`templates/email/*verification*.html:30`, `password_reset.html:30`, `workspace_invitation.html:24`, `src/config/environment.rs:1275-1336`).
18. **"Contact support" with no contact.** Five templates say "please contact support immediately", but no support address or link is configured or passed (`templates/email/password_reset_confirmation.html:22`, `password_change_confirmation.html:24`, `account_deletion_confirmation.html:24`, `email_change_notice.html:26`, `email_change_confirmation.html:22`, and their `.txt` twins).
19. **Invitation copy is tied to the desktop client.** It hard-codes "Open the {{ app_name }} desktop application" (`templates/email/workspace_invitation.html:22`, `.txt:7`), so the template assumes that client.
20. **Branding is mostly absent.** `APP_DISPLAY_NAME` is documented as the product name in email copy (`.env.example:8-9`), but only 2 of 9 templates and no subject use it.
21. **Test and doc labels differ from production.** `email_type = "verification"` is used in tests and the alert-rule tests (`src/services/email_service.rs:332,344`, `src/services/otel_metrics_service.rs:339`, `tests/prometheus/alert-rules.test.yml:970-1017`) and in the model doc (`src/models/email.rs:59-60`). The production value is `email_verification` (`src/services/email_template_service.rs:356`). The tests exercise a label production never emits.
22. **Alert annotation overstates resilience.** It says "Primary operations (registration, password reset) still succeed" and "Users can resend verification codes" (`prometheus-alerts.yml:560-563`). Resend-verification and email-change requests do fail with 500/503 on SMTP errors (MAIL-020, MAIL-025).
23. **Sending to arbitrary addresses.** Any member with `InviteMember` can send invitation emails to arbitrary addresses, with an attacker-chosen workspace name in the subject and their own display name in the body, and can resend without an email-specific limit (`src/services/workspace_member_service.rs:1395-1543`). Email change lets any session send a code email to an arbitrary address, capped at 3 per 5 minutes per user (`src/services/user_service.rs:1299-1339,1460-1468`).
24. **Clients cannot see whether email works.** `GET /auth/config` exposes `require_email_verification` but not whether email, password reset or email change are available (`src/handlers/auth.rs:661-667`). With email disabled, forgot-password still answers "a password reset code has been sent" (`src/handlers/auth.rs:967-970`).
25. **Unused generality.** `EmailMessage.to` is a `Vec` and `EmailAddress` derives `Serialize`/`Deserialize` (`src/models/email.rs:8,48`), but every renderer produces exactly one recipient and nothing serializes these types.

## Open questions

1. Should best-effort notifications stay on the request path (Observation 15: latency up to the SMTP timeout), or move to background dispatch like password reset, or to a durable queue or outbox? Today delivery is at-most-once and lost on crash, shutdown or transient failure, with no retry (MAIL-010).
2. Is a 500 `INTERNAL_SERVER_ERROR` the intended client contract when the backend is disabled for resend-verification and email change (MAIL-029), and should clients be told via `/auth/config` whether email-dependent features exist?
3. Is it intended that forgot-password reports success when email is disabled, so users wait for an email that will never come (MAIL-021, Observation 24)?
4. Are the missing notifications intended scope or gaps: no old-address notice after a completed email change, no welcome email, no inviter notice on accept or decline, none on member removal, ownership transfer, workspace deletion, identity link or unlink, or new sign-in (Template catalog, last list)?
5. Should invitation emails, including unlimited resends, have a per-sender or per-recipient send limit (Observation 23)?
6. Should partial SMTP credentials (MAIL-005) and a TLS-mode/port mismatch (Observation 7) be startup errors or warnings rather than silently accepted?
7. Must the rewrite reproduce the exact subjects, `email_type` labels and body copy (dashboards and alerts key on `email_type`), or is copy free to change, including the inconsistencies in Observations 16-20?
8. Should SMTP_* parse errors abort startup when email is disabled (Observation 8)?
9. Is the staging-before-send order in email change and resend-verification intended (Observation 13), given a failed send leaves an undelivered valid code and consumes a rate-limit attempt?
10. Is localization, a support-contact variable, or in-email links (any base URL) part of the intended template contract? Today there are none (MAIL-016).
