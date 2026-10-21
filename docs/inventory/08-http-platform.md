# Inventory: HTTP platform: application wiring and the request pipeline (`HTTP`)

Reference: `AntWorker-API` @ `0210895` (2026-10-20). This file records what the old API does. It does not prescribe what the new one should do.

## Scope

**Covers:** the process entry point and command-line dispatch; the startup initialization order and its fail-fast conditions; the HTTP side of graceful shutdown; the complete route table and every route group's middleware stack; the global middleware tier (body limit, CORS, request context, request logging, the metrics layer, panic catching); the response envelope for success, error and paginated responses; the error model (`ApiError` and the nested `RepositoryError`, `AuthError`, `OAuthError`, `StorageError`, plus `ServiceErrorCode`) and how it maps to HTTP; the custom extractors (JSON, query, path, raw upload body) and the validation framework; input-normalizing deserializers and the shared validator library; keyset pagination; rate limiting; idempotency; health probes.

**Boundary:**
- AUTH owns the authentication middlewares (`user_scoped_auth_middleware`, `workspace_scoped_auth_middleware`, AUTH-019/020/021), the email-verification gate (AUTH-038), the verification-exempt group's purpose (AUTH-039), and the handler-level rate-limit buckets on auth endpoints (AUTH-006 etc.). This file only records where those layers sit in each stack.
- FILE owns upload limits per route (FILE-005), the multipart image-update form contract (FILE-026; also USER-004), and download headers (FILE-021). They are referenced, not repeated.
- RT owns the process shutdown sequence after the HTTP drain (RT-033, RT-034), the per-process instance id (RT-027), the background-task spawn matrix and distributed locks (RT-028 to RT-032), and the SSE stream (RT-001, RT-017).
- OBS owns log format, span fields, metric instruments, the OTel providers and the telemetry proxy's own behavior. Only the existence and placement of the logging and metrics layers are recorded here.
- DATA owns the schema in general, `DatabaseEncryption`, transactions, `database_retry`, and pool sizing. This file owns the `rate_limits` and `idempotency_cache` tables and their stored procedures.
- OPS owns `Config::from_env`/`validate` as a whole, Docker, Caddy and CI. Only the variables this domain reads are listed.

**What does not exist** (checked so the spec does not assume them): no request-ID header or generator (the W3C trace id in `meta.trace_id` is the only correlation id); no response compression; no request timeout layer (only DB acquire timeout and outbound client timeouts elsewhere); no `fallback` handler; no `TraceLayer` (the `tower-http` `trace` feature is compiled but unused, `Cargo.toml:12`); no PATCH routes.

**Files read in full:** `src/{server,app_state,lib,main,cli,shutdown,background}.rs`, `src/handlers/{mod,health}.rs`, `src/middleware/*` (all 8), `src/extractors/*` (all 6), `src/validation/mod.rs`, `src/errors/*` (all 9; test modules skimmed), `src/services/{idempotency_service,rate_limit_service}.rs`, `src/repositories/{idempotency_repository,rate_limit_repository}.rs` and their `postgresql/` versions, `src/repositories/postgresql/keyset.rs`, `src/utils/{pagination,request,deserializers,retry,trace,mod}.rs`, `src/auth/middleware.rs`, `src/config/database.rs`. **Read in part:** `src/config/environment.rs` (server/auth parsing and validation), every handler signature and return in `src/handlers/*.rs`, `src/handlers/telemetry.rs:60-185`, `src/repositories/postgresql/{session_repository,public_session_repository,workspace_member_repository,workspace_repository}.rs` (pagination and error sites), `migrations/0002,0003,0004,0006,0008` (rate-limit and idempotency parts), `Caddyfile`, `docker-compose.yml` (api service), `.env.example` (HTTP section), `Cargo.toml`. **Tests:** `tests/{http_smoke,router_composition,idempotency_middleware_http,idempotency_panic_http,idempotency_repository_live,rate_limit_middleware_http,rate_limit_repository_live,panic_response_http,health_probes_http,repository_error_classification}.rs` plus the inline test modules of every file above. **Docs:** `README.md`, `documentation/{MULTI_INSTANCE,LOGGING}.md` (relevant parts). `.env` and `alertmanager-secrets/` exist and were not opened.

**Conventions below:** "global tier" = the layers every request passes (HTTP-007). Paths inside route groups are seen by route-level middleware without the `/v1` prefix (axum `nest` strips it, `server.rs:693-698`, `rate_limit.rs:59-61`); full paths are used everywhere else.

## Route table

Every route is wrapped by the global tier: `DefaultBodyLimit(MAX_REQUEST_BODY_BYTES)` → CORS → request_context → logging → metrics → catch_panic (`server.rs:711-741`). Route groups then add these route-level layers, outermost first:

| Group | Builder | Route-level layers, outer → inner | Wiring |
|---|---|---|---|
| I infra | `create_infrastructure_routes` | none (not under `/v1`) | `handlers/mod.rs:43-47`, `server.rs:692` |
| P public | `create_public_routes` | rate_limit (IP tier) → idempotency | `handlers/mod.rs:52-87`, `server.rs:593-603` |
| X verification-exempt | `create_verification_exempt_routes` | user_scoped_auth → rate_limit (user tier) → idempotency | `handlers/mod.rs:103-146`, `server.rs:608-622` |
| U user-scoped | `create_user_scoped_routes` | user_scoped_auth → rate_limit (user tier) → email gate → idempotency | `handlers/mod.rs:162-291`, `server.rs:627-645` |
| W workspace-scoped | `create_workspace_scoped_routes` | workspace_scoped_auth → rate_limit (user tier) → email gate → idempotency | `handlers/mod.rs:343-408`, `server.rs:650-668` |
| T telemetry | `create_telemetry_routes` | user_scoped_auth → rate_limit (telemetry tier); group body limit `TELEMETRY_PROXY_MAX_BODY_BYTES` | `handlers/mod.rs:321-327`, `server.rs:677-687` |

Column legend. **Auth:** `public`; `session` (bearer session, AUTH-019/020); `workspace` (session plus `X-Workspace-Id` membership, AUTH-021; per-route permissions are in WS). **Rate:** `ip` = `ip:<client_ip>` bucket; `user` = `user:<user_id>`; `telem` = `telemetry_proxy:user:<user_id>`; `exempt` = the layer is mounted but skips the path; `none` = no layer (HTTP-030). **Gate:** the email-verification layer is mounted (it only acts when `REQUIRE_EMAIL_VERIFICATION=true`, default false; AUTH-038). **Idem:** `yes` = method is POST/PUT/DELETE and the group mounts idempotency (HTTP-039); `n/a` = GET (skipped); `no` = not mounted. **Body:** what the handler reads, with the effective limit. **200 shape:** every handler success is HTTP 200; `object` = `{data:{…},meta}`, `{}` = `{data:{},meta}`, `paginated` = HTTP-029 shape (`+tc` = `total_count` present).

| # | Method | Path | Handler | Grp | Auth | Rate | Gate | Idem | Body (limit) | 200 shape |
|---|---|---|---|---|---|---|---|---|---|---|
| 1 | GET | `/health/live` | `health::health_live` (`health.rs:37`) | I | public | none | no | no | none | object `{status:"alive",version}` |
| 2 | GET | `/health/ready` | `health::health_ready` (`health.rs:74`) | I | public | none | no | no | none | object `{status:"ready",version}`; 503 when draining |
| 3 | GET | `/v1/permissions/schema` | `permissions::get_permission_schema` (`permissions.rs:81`) | P | public | exempt | no | n/a | none | object |
| 4 | GET | `/v1/audit-actions/schema` | `audit_actions::get_audit_action_schema` (`audit_actions.rs:78`) | P | public | exempt | no | n/a | none | object |
| 5 | GET | `/v1/auth/config` | `auth::get_auth_config` (`auth.rs:682`) | P | public | ip | no | n/a | none | object |
| 6 | POST | `/v1/auth/oauth` | `auth::oauth_login` (`auth.rs:216`) | P | public | ip | no | yes (public scope) | JSON (2 MiB) | object |
| 7 | POST | `/v1/auth/register` | `auth::credentials_register` (`auth.rs:335`) | P | public | ip | no | yes (public) | JSON (2 MiB) | object |
| 8 | POST | `/v1/auth/login` | `auth::credentials_login` (`auth.rs:387`) | P | public | ip | no | yes (public) | JSON (2 MiB) | object |
| 9 | POST | `/v1/auth/refresh` | `auth::token_refresh` (`auth.rs:584`) | P | public | ip | no | yes (public) | JSON (2 MiB) | object |
| 10 | POST | `/v1/auth/forgot-password` | `auth::forgot_password` (`auth.rs:899`) | P | public | ip | no | yes (public) | JSON (2 MiB) | object |
| 11 | POST | `/v1/auth/reset-password` | `auth::reset_password` (`auth.rs:986`) | P | public | ip | no | yes (public) | JSON (2 MiB) | object |
| 12 | GET | `/v1/files/public/user/{file_id}` | `files::download_public_user_file` (`files.rs:452`) | P | public | ip | no | n/a | none | raw bytes (FILE-020) |
| 13 | GET | `/v1/files/public/workspace/{file_id}` | `files::download_public_workspace_file` (`files.rs:480`) | P | public | ip | no | n/a | none | raw bytes |
| 14 | POST | `/v1/auth/validate-session` | `auth::validate_session` (`auth.rs:535`) | X | session | user | no | yes | none | object |
| 15 | POST | `/v1/auth/logout` | `auth::logout` (`auth.rs:502`) | X | session | user | no | yes | none | `{}` |
| 16 | POST | `/v1/auth/verify-email` | `auth::verify_email` (`auth.rs:731`) | X | session | user | no | yes | JSON (2 MiB) | object |
| 17 | POST | `/v1/auth/resend-verification` | `auth::resend_verification` (`auth.rs:796`) | X | session | user | no | yes | none | object |
| 18 | POST | `/v1/users/me/email` | `auth::request_email_change` (`auth.rs:1095`) | X | session | user | no | yes | JSON (2 MiB) | object |
| 19 | POST | `/v1/users/me/email/confirm` | `auth::confirm_email_change` (`auth.rs:1134`) | X | session | user | no | yes | JSON (2 MiB) | object |
| 20 | POST | `/v1/users/me/delete` | `users::delete_current_user` (`users.rs:150`) | X | session | user | no | yes | JSON (2 MiB) | `{}` |
| 21 | GET | `/v1/users/me` | `users::get_current_user` (`users.rs:36`) | U | session | user | yes | n/a | none | object |
| 22 | PUT | `/v1/users/me` | `users::update_current_user` (`users.rs:90`) | U | session | user | yes | yes | multipart (10 MiB, FILE-005) | object |
| 23 | PUT | `/v1/users/me/password` | `users::change_password` (`users.rs:189`) | U | session | user | yes | yes | JSON (2 MiB) | `{}` |
| 24 | PUT | `/v1/users/me/telemetry-consent` | `users::set_telemetry_consent` (`users.rs:227`) | U | session | user | yes | yes | JSON (2 MiB) | `{}` |
| 25 | GET | `/v1/users/me/workspaces` | `workspaces::list_my_workspaces` (`workspaces.rs:49`) | U | session | user | yes | n/a | query | paginated +tc |
| 26 | POST | `/v1/users/me/workspaces` | `workspaces::create_my_workspace` (`workspaces.rs:91`) | U | session | user | yes | yes | JSON (2 MiB) | object |
| 27 | POST | `/v1/users/me/workspaces/{id}/switch` | `workspaces::switch_my_workspace` (`workspaces.rs:149`) | U | session | user | yes | yes | none | object |
| 28 | GET | `/v1/users/me/invitations/pending` | `user_invitations::get_pending_invitations` (`user_invitations.rs:42`) | U | session | user | yes | n/a | query | paginated +tc |
| 29 | POST | `/v1/users/me/invitations/{invitation_id}/accept` | `user_invitations::accept_invitation` (`user_invitations.rs:84`) | U | session | user | yes | yes | none | `{}` |
| 30 | POST | `/v1/users/me/invitations/{invitation_id}/decline` | `user_invitations::decline_invitation` (`user_invitations.rs:121`) | U | session | user | yes | yes | none | `{}` |
| 31 | GET | `/v1/users/me/audit-logs` | `user_audit_logs::get_user_audit_logs` (`user_audit_logs.rs:36`) | U | session | user | yes | n/a | query | paginated |
| 32 | GET | `/v1/users/me/sessions` | `sessions::list_sessions` (`sessions.rs:92`) | U | session | user | yes | n/a | query | paginated |
| 33 | DELETE | `/v1/users/me/sessions/others` | `sessions::revoke_other_sessions` (`sessions.rs:170`) | U | session | user | yes | yes | none | object `{revoked_count}` |
| 34 | DELETE | `/v1/users/me/sessions/{session_id}` | `sessions::revoke_session` (`sessions.rs:137`) | U | session | user | yes | yes | none | `{}` |
| 35 | GET | `/v1/users/me/events` | `sse_notifications::events_stream` (`sse_notifications.rs:88`) | U | session | exempt | yes | n/a | none | `text/event-stream` (RT-001) |
| 36 | POST | `/v1/users/me/files` | `files::upload_user_file` (`files.rs:177`) | U | session | user | yes | yes | raw bytes + `?visibility` + `Content-Type`/`Content-Disposition` (10 MiB) | object |
| 37 | GET | `/v1/users/me/files` | `files::list_user_files` (`files.rs:215`) | U | session | user | yes | n/a | query | paginated |
| 38 | DELETE | `/v1/users/me/files/{file_id}` | `files::delete_user_file` (`files.rs:245`) | U | session | user | yes | yes | none | `{}` |
| 39 | GET | `/v1/files/user/{file_id}` | `files::download_user_file` (`files.rs:272`) | U | session | user | yes | n/a | none | raw bytes |
| 40 | GET | `/v1/files/workspace/{file_id}` | `files::download_workspace_file` (`files.rs:410`) | U | session | user | yes | n/a | none | raw bytes |
| 41 | GET | `/v1/auth/identities` | `identities::list_identities` (`identities.rs:46`) | U | session | user | yes | n/a | none | object `{identities:[…]}` (not paginated) |
| 42 | POST | `/v1/auth/identities/credentials` | `identities::link_credentials` (`identities.rs:70`) | U | session | user | yes | yes | JSON (2 MiB) | object |
| 43 | POST | `/v1/auth/identities/credentials/unlink` | `identities::unlink_credentials` (`identities.rs:130`) | U | session | user | yes | yes | JSON (2 MiB) | `{}` |
| 44 | POST | `/v1/auth/identities/oauth/complete` | `identities::link_oauth_complete` (`identities.rs:98`) | U | session | user | yes | yes | JSON (2 MiB) | object |
| 45 | DELETE | `/v1/auth/identities/{method}` | `identities::unlink_oauth_identity` (`identities.rs:171`) | U | session | user | yes | yes | none | `{}` |
| 46 | POST | `/v1/telemetry/traces` | `telemetry::proxy_traces` (`telemetry.rs:73`) | T | session | telem | no | no | raw OTLP bytes (1 MiB) | collector status and body passed through, not enveloped |
| 47 | POST | `/v1/telemetry/metrics` | `telemetry::proxy_metrics` (`telemetry.rs:103`) | T | session | telem | no | no | raw OTLP bytes (1 MiB) | passthrough |
| 48 | POST | `/v1/telemetry/logs` | `telemetry::proxy_logs` (`telemetry.rs:133`) | T | session | telem | no | no | raw OTLP bytes (1 MiB) | passthrough |
| 49 | GET | `/v1/workspace` | `workspaces::get_workspace` (`workspaces.rs:224`) | W | workspace | user | yes | n/a | none | object |
| 50 | PUT | `/v1/workspace` | `workspaces::update_workspace` (`workspaces.rs:268`) | W | workspace | user | yes | yes | multipart (10 MiB, FILE-005) | object |
| 51 | DELETE | `/v1/workspace` | `workspaces::delete_workspace` (`workspaces.rs:316`) | W | workspace | user | yes | yes | none | `{}` |
| 52 | POST | `/v1/workspace/transfer` | `workspaces::transfer_workspace` (`workspaces.rs:356`) | W | workspace | user | yes | yes | JSON (2 MiB) | object |
| 53 | GET | `/v1/workspace/members` | `workspace_members::get_workspace_members` (`workspace_members.rs:43`) | W | workspace | user | yes | n/a | query | paginated |
| 54 | PUT | `/v1/workspace/members/{member_id}/permissions` | `workspace_members::update_member_permissions` (`workspace_members.rs:83`) | W | workspace | user | yes | yes | JSON (2 MiB) | object |
| 55 | DELETE | `/v1/workspace/members/{member_id}` | `workspace_members::remove_member` (`workspace_members.rs:120`) | W | workspace | user | yes | yes | none | `{}` |
| 56 | POST | `/v1/workspace/leave` | `workspace_members::leave_workspace` (`workspace_members.rs:161`) | W | workspace | user | yes | yes | none | `{}` |
| 57 | GET | `/v1/workspace/invitations` | `workspace_members::get_workspace_invitations` (`workspace_members.rs:198`) | W | workspace | user | yes | n/a | query | paginated +tc |
| 58 | POST | `/v1/workspace/invitations` | `workspace_members::create_invitation` (`workspace_members.rs:239`) | W | workspace | user | yes | yes | JSON (2 MiB) | object |
| 59 | PUT | `/v1/workspace/invitations/{invitation_id}` | `workspace_members::update_invitation` (`workspace_members.rs:275`) | W | workspace | user | yes | yes | JSON (2 MiB) | object |
| 60 | DELETE | `/v1/workspace/invitations/{invitation_id}` | `workspace_members::cancel_invitation` (`workspace_members.rs:313`) | W | workspace | user | yes | yes | none | `{}` |
| 61 | POST | `/v1/workspace/invitations/{invitation_id}/resend` | `workspace_members::resend_invitation` (`workspace_members.rs:349`) | W | workspace | user | yes | yes | none | object |
| 62 | GET | `/v1/workspace/audit-logs` | `workspace_audit_logs::get_workspace_audit_logs` (`workspace_audit_logs.rs:49`) | W | workspace | user | yes | n/a | query | paginated |
| 63 | POST | `/v1/workspace/files` | `files::upload_workspace_file` (`files.rs:312`) | W | workspace | user | yes | yes | raw bytes + `?visibility` + headers (10 MiB) | object |
| 64 | GET | `/v1/workspace/files` | `files::list_workspace_files` (`files.rs:351`) | W | workspace | user | yes | n/a | query | paginated |
| 65 | DELETE | `/v1/workspace/files/{file_id}` | `files::delete_workspace_file` (`files.rs:382`) | W | workspace | user | yes | yes | none | `{}` |

Totals: 65 routes (2 infra, 11 public, 7 verification-exempt, 25 user-scoped, 3 telemetry, 17 workspace-scoped). Route registrations: `handlers/mod.rs:45-46,55-86,107-145,168-290,323-326,346-407`. Path parameters are `Uuid` except `{method}` (`String`, `identities.rs:174`). "query" bodies are `Query<PaginationParams>` plus, on lists 25, 28, 31, 53, 57, 62, a `ValidatedQuery<…Filters>` (HTTP-023, HTTP-024); lists 32, 37 and 64 take pagination only.

## Behaviors

### HTTP-001 Command-line dispatch

- **Behavior:** The binary `app-api` runs the API server when given no subcommand and runs the embedded schema migrations then exits when given `migrate`. Any other argument is rejected by clap before anything starts. Before dispatch, the process installs aws-lc-rs as the default rustls crypto provider.
- **Kind:** `capability`
- **Trigger:** Process start.
- **Input & validation:** `app-api` → server; `app-api migrate` → `migrate::run` (DATA); `app-api migrat`, `app-api migrate run`, `app-api --source …` → clap error (usage message, clap's exit code). `--help`/`--version` are clap built-ins.
- **Effects:** Server: `server::run` (HTTP-002); exit code 0 when it returns `Ok`. A returned error is printed by `main` as `Error: …` with exit code 1. `migrate` returns its own exit code.
- **Source:** `src/main.rs:5-8`, `src/cli.rs:8-29`, `src/lib.rs:55,61-64`
- **Tests:** `src/cli.rs::tests::{definition_is_valid, no_argument_runs_the_server, migrate_selects_the_migrate_command, unknown_arguments_are_rejected}`; `tests/migrate_command.rs` for the migrate path.

### HTTP-002 Startup initialization order and fail-fast conditions

- **Behavior:** `server::run` initializes in this order, and stops at the first failure:
  1. Load `.env` if present (errors ignored).
  2. `Config::from_env()`; on error print `FATAL: Configuration error: …` to stderr and exit.
  3. `config.validate()`; hard errors print `FATAL: Configuration validation error: …` and exit; warnings are held for later logging.
  4. Generate the per-process `instance_id` (RT-027).
  5. Build the observability repository (OTLP when `OBSERVABILITY_BACKEND` selects it, else Local) and print one line to stderr; initialize the OTel providers.
  6. Install the tracing subscriber: `RUST_LOG` or the environment's default filter, a JSON console layer, and the OTel trace and log layers (OBS).
  7. Log OTel init errors (warn), config warnings (warn), feature statuses (info) and "Configuration validation successful".
  8. Create the DB pool (eager connect; failure aborts startup).
  9. Load the encryption key ring (`expect`: failure panics the process).
  10. Build the metrics service (OTel or no-op) and register the pool gauge.
  11. Create the shutdown coordinator.
  12. Build SSE services and spawn the shared listener (RT), the email service (MAIL; SMTP construction error aborts) and object storage (FILE; S3 client error aborts).
  13. `build_app_state` wires all repositories and services (email template parse error aborts).
  14. Plan and spawn background tasks (RT-028).
  15. `build_router`, bind a TCP listener on `SERVER_HOST:SERVER_PORT` (bind error aborts), log "API server listening", and serve with `ConnectInfo<SocketAddr>` (the TCP peer address feeds HTTP-011) and graceful shutdown (HTTP-004).
- **Kind:** `guarantee`
- **Trigger:** Startup (no subcommand).
- **Input & validation:** `SERVER_HOST` default `0.0.0.0`, `SERVER_PORT` default 8080 (u16 parse). Config parse and validation rules are OPS; the HTTP-relevant ones are in Configuration below.
- **Effects:** No request is served until every step succeeds; migrations are never run by the server (HTTP-001).
- **Guarantees:** Configuration errors are reported before tracing is up, so they go to stderr only. Readiness (HTTP-006) does not depend on any of the dependencies above after startup.
- **Source:** `src/server.rs:834-2163`, `src/config/database.rs:6-25`, `src/config/environment.rs:404-405`
- **Tests:** `src/server.rs::tests::{default_filters_route_sdk_diagnostics_to_the_console_only, otel_logs_filter_holds_under_a_permissive_rust_log}` (log filters only). No test covers the ordering.

### HTTP-003 The router and service graph are built by pure composition functions

- **Behavior:** `build_app_state(config, AppResources)` builds every repository and service with no I/O of its own beyond the email-template parse, and `build_router(app_state, config)` returns the routes wrapped by `apply_global_middleware`. Production calls both from `server::run`; the integration tests call the same functions with disabled backends and an optional injected OAuth provider factory.
- **Kind:** `guarantee`
- **Trigger:** Startup and test harnesses.
- **Effects:** The idempotency service is always constructed (`Some`), the rate-limit service always exists, the telemetry-proxy service always exists (its HTTP client uses `TELEMETRY_PROXY_UPSTREAM_TIMEOUT_SECS`).
- **Guarantees:** The served router is exactly the tested composition (the last expression of `build_router` is `apply_global_middleware(...)`).
- **Source:** `src/server.rs:39-57,68-480,490-701,711-741`, `src/app_state.rs:19-204`
- **Tests:** `tests/router_composition.rs::build_router_returns_its_routes_wrapped_in_the_global_middleware_tier`; `tests/http_smoke.rs::signup_login_and_auth_reject_through_the_real_stack`

### HTTP-004 Graceful HTTP shutdown

- **Behavior:** On SIGINT (Ctrl+C, all platforms) or SIGTERM (Unix only), the coordinator logs the signal, sets the shutdown flag (readiness turns 503, HTTP-006) and sends one broadcast (SSE streams and background tasks stop, RT-016/RT-033). Axum then stops accepting connections and waits for in-flight requests with no time limit. The rest of the sequence (background tasks, pool close, OTel flush) is RT-034. The process exits 0.
- **Kind:** `guarantee`
- **Trigger:** OS signal.
- **Effects:** Info log "Received shutdown signal, initiating graceful shutdown" with `signal`; then "HTTP server shutdown complete, draining background tasks" (`event=shutdown_complete`).
- **Errors:** If `axum::serve` itself returns an error, it is logged ("Server error occurred") and `run` returns `Err("Server error: …")` immediately, skipping background-task shutdown, pool close and the OTel flush (exit code 1).
- **Guarantees:** The flag and the broadcast are set together, at the moment the listener stops accepting. The broadcast channel has capacity 16; a send with no receivers is ignored. Failure to install a signal handler panics.
- **Source:** `src/shutdown.rs:47-53,110-168`, `src/app_state.rs:191-203`, `src/server.rs:2151-2181`
- **Tests:** `src/shutdown.rs::tests::initiate_shutdown_sets_flag_and_notifies_subscribers`

### HTTP-005 Liveness probe

- **Behavior:** `GET /health/live` always returns 200 with `{data:{status:"alive", version:<CARGO_PKG_VERSION>}, meta}`, including during graceful shutdown.
- **Kind:** `capability`
- **Trigger:** `GET /health/live`
- **Auth:** public; no rate limit, no idempotency, not under `/v1`.
- **Effects:** None. Not counted by the metrics layer (HTTP-013); not logged (2xx).
- **Guarantees:** No dependency is consulted, so it cannot fail while the process serves HTTP.
- **Source:** `src/handlers/health.rs:16-19,37-44`, `src/handlers/mod.rs:45`
- **Tests:** `tests/health_probes_http.rs::readiness_drains_to_503_on_shutdown_while_liveness_stays_200`

### HTTP-006 Readiness probe

- **Behavior:** `GET /health/ready` returns 200 `{data:{status:"ready", version}, meta}` until the shutdown flag is set, then 503 for the rest of the process life. It does not check the database or any other dependency.
- **Kind:** `capability`
- **Trigger:** `GET /health/ready`
- **Auth:** public; no rate limit, no idempotency, not under `/v1`.
- **Errors:** `SERVICE_UNAVAILABLE` → 503 → shutdown flag set. Wire message is the generic "Service temporarily unavailable"; the log context carries `error_code=dependency_unavailable`, "Service is shutting down".
- **Effects:** Each 503 is logged at ERROR by the logging layer (HTTP-012). Not counted by metrics.
- **Guarantees:** The Compose healthcheck polls this path (`docker-compose.yml:51-57`); Caddy does not (RT-034).
- **Source:** `src/handlers/health.rs:74-89`, `src/app_state.rs:191-193`, `src/errors/api_error.rs:1033-1038`
- **Tests:** `tests/health_probes_http.rs::readiness_drains_to_503_on_shutdown_while_liveness_stays_200`

### HTTP-007 Global middleware order

- **Behavior:** Every request, matched or not, passes these layers outermost first: `DefaultBodyLimit(MAX_REQUEST_BODY_BYTES)` → CORS → request_context → logging → metrics → catch_panic → route-level layers → handler. The order means: CORS preflights are answered before any logging or metrics; the request context exists before logging and rate limiting; the logging span encloses metrics; a panic anywhere inside catch_panic becomes a response that metrics and logging record.
- **Kind:** `guarantee`
- **Trigger:** Every request.
- **Guarantees:** Global layers do not depend on `AuthContext`; anything that does (rate limiting, idempotency, the email gate) is route-level (HTTP-008). A panic inside a global layer itself is not caught.
- **Source:** `src/server.rs:711-741`, `src/server.rs:498-519` (documented invariants)
- **Tests:** `tests/panic_response_http.rs::handler_panic_is_answered_counted_and_logged_once`; `tests/router_composition.rs::build_router_returns_its_routes_wrapped_in_the_global_middleware_tier`

### HTTP-008 Route-group layer stacks

- **Behavior:** Route groups carry the route-level stacks in the Route table. Authentication is always the outermost route layer, so `AuthContext` exists before rate limiting and idempotency; rate limiting runs before the email gate and idempotency, so rate-limited and gate-rejected requests are never cached. The public and infra groups carry no auth layer; the verification-exempt and telemetry groups carry no email gate; the telemetry group carries no idempotency.
- **Kind:** `guarantee`
- **Trigger:** Every request that matches a route.
- **Errors:** If the IP-tier limiter runs without a request context, or the email gate runs without an `AuthContext`, the response is `INTERNAL_SERVER_ERROR` 500 (`middleware_misconfiguration`); unreachable with the shipped wiring.
- **Guarantees:** Route layers run only for matched routes (axum `route_layer`). Because auth runs first, requests that fail authentication never reach the limiter (see Observations).
- **Source:** `src/server.rs:578-687`, `src/middleware/rate_limit.rs:199-209`, `src/auth/middleware.rs:85-114`
- **Tests:** `tests/router_composition.rs::{every_protected_route_group_has_an_auth_layer, public_route_group_carries_no_auth_layer, auth_is_the_outermost_layer_on_protected_groups, verification_exempt_group_carries_no_email_verification_gate}`

### HTTP-009 Versioned prefix, unversioned health, framework defaults for misses

- **Behavior:** All business routes live under `/{API_VERSION}` = `/v1`; the prefix comes from the same constant as `meta.api_version`. Health routes are at the root. No fallback is registered: an unmatched path gets axum's default 404 with an empty body (no envelope, no `meta.trace_id`), and a known path with an unregistered method gets axum's default 405. GET routes also answer HEAD (axum default).
- **Kind:** `rule`
- **Trigger:** Any request.
- **Effects:** Misses are logged at WARN as "Request failed without structured error context" with `http_path="/<unmatched>"` and the real `url_path`, and counted under the `/<unmatched>` route label.
- **Source:** `src/errors/response_envelope.rs:20`, `src/server.rs:689-698`, `src/middleware/logging.rs:234-250,515-533`, `src/middleware/metrics.rs:132-150`
- **Tests:** `src/middleware/mod.rs::tests::telemetry_path_prefix_is_versioned`; `tests/health_probes_http.rs` (root paths). None for 404/405 shape.

### HTTP-010 CORS policy

- **Behavior:**
  - Development mode (`ENVIRONMENT` exactly `development`, which is the default): allowed origins are the fixed baseline `http://localhost:1420`, `tauri://localhost`, `http://127.0.0.1:1420` plus every `CORS_ORIGINS` entry.
  - Any other `ENVIRONMENT` value: only `CORS_ORIGINS`; if it is empty, only `tauri://localhost` (with a startup warning).
  - Entries equal to `*` or unparsable as a header value are dropped with a warning. Matching origins are echoed back.
  - Allowed methods: GET, POST, PUT, DELETE, OPTIONS. Allowed request headers: `Authorization`, `Content-Type`, `Accept`. Credentials allowed. No exposed response headers, no max-age.
- **Kind:** `guarantee`
- **Trigger:** Any request carrying `Origin`, including preflight `OPTIONS`.
- **Input & validation:** `CORS_ORIGINS` is split on commas, trimmed, blanks dropped.
- **Effects:** Startup info log with the origin count; preflights are answered inside the CORS layer, outside logging and metrics.
- **Guarantees:** A wildcard can never reach the allow-list (it would panic tower-http with credentials).
- **Source:** `src/middleware/cors.rs:13-17,32-131,140-182`, `src/config/environment.rs:332-340`, `src/config/observability.rs:152-154`, `src/server.rs:717-720,733`
- **Tests:** `src/middleware/cors.rs::tests::{collect_allowed_origins_drops_wildcard_keeps_valid, collect_allowed_origins_drops_malformed_origins}`

### HTTP-011 Request context: client IP and User-Agent

- **Behavior:** For every request the server derives `client_ip` as: the first comma-separated entry of `X-Forwarded-For` (trimmed, if non-empty), else `X-Real-IP` (trimmed, if non-empty), else the TCP peer IP, else the literal `unknown`. `user_agent` is the raw `User-Agent` value when it is valid UTF-8 (an empty header is kept as an empty string). Both are held in a task-local for the whole request.
- **Kind:** `rule`
- **Trigger:** Every request (global tier).
- **Input & validation:** Values are not validated as IP addresses; any string is accepted.
- **Effects:** Consumed by the IP rate-limit key (HTTP-030) and audit enrichment (AUDIT-012).
- **Guarantees:** Spoofing resistance depends on the proxy: Caddy's `reverse_proxy` sets `X-Forwarded-For` itself (OPS).
- **Source:** `src/middleware/request_context.rs:31-92`, `src/utils/request.rs:48-111`
- **Tests:** `src/utils/request.rs::tests::{extract_client_ip_forwarded_for, extract_client_ip_real_ip, extract_client_ip_priority, extract_client_ip_empty_real_ip_falls_through, extract_client_ip_whitespace_real_ip_falls_through, extract_client_ip_connect_info, extract_client_ip_headers_take_priority_over_connect_info, extract_client_ip_none, extract_client_ip_empty_header, extract_client_ip_whitespace_only, extract_user_agent_present, extract_user_agent_missing, extract_user_agent_empty, extract_client_ip_forwarded_for_multiple, extract_client_ip_forwarded_for_with_spaces}`; `tests/rate_limit_middleware_http.rs::rate_limit_ip_tier_enforced_and_keyed_per_ip`

### HTTP-012 Request logging: one log line per failed request

- **Behavior:** Each request runs inside one `http_request` span (continuing an incoming W3C `traceparent` when valid, HTTP-016). After the response:
  - 5xx → one ERROR event; 4xx → one WARN event, with the error context the response carries (`error_type`, `error_code`, `error_message`, `trace_id`, origin `timestamp`, `error_details`) plus `http_status`, `http_method`, `http_path` (route template or `/<unmatched>`), `url_path`, `duration_ms`.
  - 4xx/5xx without error context (framework 404/405, the telemetry body-limit 413, upstream passthrough statuses) → the same level with HTTP fields only.
  - 1xx–3xx → no event.
  Code that builds an `ApiError` does not log it; this layer is the single sink. Field format is OBS.
- **Kind:** `guarantee`
- **Trigger:** Every request.
- **Guarantees:** At most one request-outcome log per request. Error details (including inputs echoed in validation messages) reach the logs.
- **Source:** `src/middleware/logging.rs:215-546`, `src/errors/mod.rs:47-58`, `src/errors/api_error.rs:1086-1110`
- **Tests:** `tests/panic_response_http.rs::handler_panic_is_answered_counted_and_logged_once`

### HTTP-013 Request metrics layer

- **Behavior:** A metrics layer records request count, duration and error count (client_error/server_error) per method, route template (or `/<unmatched>`) and status for every request except paths starting with `/health` or `/v1/telemetry/`. With the Local observability backend it is a no-op. Instruments and labels are OBS.
- **Kind:** `guarantee`
- **Trigger:** Every request.
- **Source:** `src/middleware/metrics.rs:77-209`, `src/middleware/mod.rs:29,41-44`
- **Tests:** `tests/panic_response_http.rs::handler_panic_is_answered_counted_and_logged_once`

### HTTP-014 Handler panics become a 500 envelope

- **Behavior:** A panic in a handler or any route-level middleware is answered with `INTERNAL_SERVER_ERROR` 500, message "An unexpected error occurred", no details. The panic payload goes only to the log context: `error_code=handler_panic`, `error_message="Request handler panicked: <payload>"` when the payload is a string, else "Request handler panicked".
- **Kind:** `guarantee` (client-facing: the connection is answered instead of dropped)
- **Trigger:** A panic inside the catch_panic layer.
- **Effects:** Logged once at ERROR, counted as server_error. A keyed request's idempotency lock is released first (HTTP-045).
- **Source:** `src/middleware/catch_panic.rs:18-28`, `src/server.rs:726`
- **Tests:** `src/middleware/catch_panic.rs::tests::{str_payload_reaches_the_log_context_but_not_the_wire, string_payload_reaches_the_log_context, non_text_payload_still_yields_the_500_envelope}`; `tests/panic_response_http.rs::handler_panic_is_answered_counted_and_logged_once`; `tests/idempotency_panic_http.rs::handler_panic_releases_the_idempotency_key_for_a_retry`

### HTTP-015 Request body size limits per route class

- **Behavior:** A body limit applies when a handler reads the body:
  - Default: `MAX_REQUEST_BODY_BYTES` (2,097,152) on every route.
  - `PUT /users/me`, `PUT /workspace`, `POST /users/me/files`, `POST /workspace/files`: `STORAGE_MAX_UPLOAD_BYTES` (10,485,760), set per method-router (FILE-005).
  - `/v1/telemetry/*`: `TELEMETRY_PROXY_MAX_BODY_BYTES` (1,048,576) for the group.
  The outcome of an oversized body depends on the extractor:
  - JSON routes → `VALIDATION_ERROR` 400 "Failed to read request body" (`invalid_request_body`).
  - Upload and multipart routes → `PAYLOAD_TOO_LARGE` 413 (FILE-005).
  - Telemetry routes (raw `Bytes`) → axum's default plain-text 413, no envelope.
  - A keyed request (`Idempotency-Key`) whose declared `Content-Length` exceeds the idempotency ceiling → `PAYLOAD_TOO_LARGE` 413 before any read (HTTP-046).
- **Kind:** `rule`
- **Trigger:** Any request whose handler or middleware reads the body.
- **Guarantees:** The limit bounds what an extractor buffers. It does not reject a request before middleware runs, and routes whose handler reads no body are unaffected.
- **Source:** `src/server.rs:734-739`, `src/handlers/mod.rs:171,244,326,349,401`, `src/extractors/json.rs:147-150`, `src/extractors/body.rs:53-78`, `src/handlers/telemetry.rs:73-78`, `src/config/environment.rs:500-513`
- **Tests:** `src/extractors/body.rs::tests::{within_limit_extracts_ok, over_limit_returns_413, body_read_failure_returns_400_not_413}`; `tests/idempotency_middleware_http.rs::idempotency_rejects_oversized_declared_content_length_before_buffering`. None for the JSON or telemetry outcome.

### HTTP-016 Response envelope: success

- **Behavior:** Enveloped successes are `{"data": <payload>, "meta": {"api_version": "v1", "trace_id": "<32 lowercase hex>", "timestamp": "<RFC 3339 UTC>"}}` with `Content-Type: application/json`; `error` is omitted. Every handler returns HTTP 200 (creates included). Operations with no payload return `data: {}`. `meta.trace_id` is the trace id of the request span: the incoming `traceparent` trace id when valid, else a new one; if no span is active the W3C invalid id `000…0` (32 zeros) is used and an error is logged.
- **Kind:** `guarantee`
- **Trigger:** Every successful enveloped handler (all routes except downloads, SSE and telemetry).
- **Effects:** Incoming W3C baggage `user_id`/`workspace_id` are recorded on the span only (OBS).
- **Source:** `src/errors/response_envelope.rs:20,47-59,81-129,158-186,202-217`, `src/utils/trace.rs:49-126`, `src/middleware/logging.rs:99-174,351-371`
- **Tests:** `src/errors/response_envelope.rs::tests::{success_populates_data_omits_error_and_injects_meta, success_with_status_applies_custom_status_code, empty_success_serializes_data_as_empty_object, response_meta_from_context_uses_compile_time_api_version}`

### HTTP-017 Response envelope: error

- **Behavior:** Every `ApiError` response is `{"error": {"code": "<SCREAMING_SNAKE_CASE>", "message": "<text>", "details": <json>}, "meta": {…}}`; `data` is omitted and `details` is omitted when absent. `code` is one of ten wire codes (Error code catalog). The response also carries an internal `ErrorLogContext` extension for the logging layer (not serialized).
- **Kind:** `guarantee`
- **Trigger:** Any handler, extractor or middleware returning `ApiError`.
- **Guarantees:** The error constructor is private to the errors module, so every enveloped error response passes through `ApiError::into_response` and is logged.
- **Source:** `src/errors/response_envelope.rs:242-350`, `src/errors/api_error.rs:657-1112`
- **Tests:** `src/errors/response_envelope.rs::tests::{error_constructor_populates_error_omits_data_and_respects_status, error_constructor_omits_details_when_none, error_code_serializes_to_screaming_snake_case_for_every_variant}`

### HTTP-018 Error-to-HTTP mapping and hiding of internal detail

- **Behavior:** `ApiError` variants map to status, wire code, wire message and wire details as in the Error code catalog. Internal detail is hidden as follows:
  - All 500s and 503s, and 502 provider faults, return a fixed generic message and no details.
  - Every 409 (DB constraint or service conflict) returns "Resource already exists" with no details, except the four AuthError 409s, which keep their message and `details.reason`/`method`.
  - Storage 404 returns "Object not found" with no details.
  - The full message, code and details go to the log context only.
  - 400 (`Validation`), 413, service 404 (`ApiError::NotFound`, with `{entity_type, entity_id}`) and AuthError messages are passed through.
- **Kind:** `guarantee` (security)
- **Trigger:** Any `ApiError` response.
- **Source:** `src/errors/api_error.rs:685-1093`, `src/errors/auth_error.rs:365-432`
- **Tests:** `src/errors/api_error.rs::tests::into_response_*` (28 tests: repo, OAuth, auth, storage, validation, internal, service-unavailable, payload, rate-limit, not-found, conflict arms)

### HTTP-019 Database error classification

- **Behavior:** `sqlx` errors become `RepositoryError` as follows:
  - SQLSTATE 23505 and 23503 → `ConstraintViolation` → 409.
  - 40001 and 40P01 → `TransientError` → 500 after retries.
  - Any other SQLSTATE, or none → `QueryFailed` → 500.
  - `RowNotFound` → `NotFound` (`database_entity`/`unknown`) → 404.
  - `PoolTimedOut`, `WorkerCrashed`, and I/O errors of kind ConnectionReset, ConnectionAborted, BrokenPipe, TimedOut, NotConnected, Interrupted, UnexpectedEof → `ConnectionFailed` → 503 "Database connection unavailable".
  - Other I/O kinds and every other sqlx error (encode, decode) → `QueryFailed` → 500.
  `TransientError` and `ConnectionFailed` are the only kinds that `is_transient()` reports as retryable; every other kind is permanent. `with_operation_context` adds `operation`, `entity_type` and `entity_id` to the details of QueryFailed, NotFound, TransientError, ConstraintViolation and ConnectionFailed, and leaves the other variants unchanged.
- **Kind:** `rule`
- **Trigger:** Any DB call that fails.
- **Effects:** Retry helpers key on `is_transient()` (DATA `database_retry`; background `retry_with_backoff`: up to N attempts, sleeping about 2 s then 4 s ±500 ms between them, aborting at once on a permanent error).
- **Source:** `src/errors/repository_error.rs:224-246,365-407,831-960`, `src/utils/retry.rs:76-217`
- **Tests:** `src/errors/repository_error.rs::tests::{from_sqlx_*, *_is_transient, *_is_not_transient, with_operation_context_*}`; `tests/repository_error_classification.rs::{real_unique_violation_classifies_as_constraint_violation, real_foreign_key_violation_classifies_as_constraint_violation}`; `src/utils/retry.rs::tests::*`

### HTTP-020 Repository-origin 400/401/404 errors expose their message and details

- **Behavior:** Unlike 5xx and 409, these repository variants send their origin message and details to the client:
  - `RepositoryError::NotFound` → 404 with its message (for example "database_entity not found: unknown") and details `{entity_type, entity_id, operation?}`.
  - `ValidationError` → 400 with `{field, value_type, error}`.
  - `InvalidAuthContext` → 401 with `{reason, user_id, workspace_id}`.
  - `SessionExpired` → 401 with the fixed message "Session expired" and `{session_id, expires_at}`. An expired access token yields this shape, while an unknown token yields AuthError `invalid_access_token` with no details.
  - `OAuthError::InvalidToken` → 401 "Invalid or expired OAuth token" with `{upstream_status}`; `InsufficientScope` → 403 with its message and `{required_scope}`.
- **Kind:** `rule`
- **Trigger:** Those errors reaching the HTTP boundary.
- **Source:** `src/errors/api_error.rs:694-723,810-824`, `src/errors/repository_error.rs:624-639,698-784`, `src/repositories/postgresql/public_session_repository.rs:117-124`, `src/errors/oauth_error.rs:282-292,373-389`
- **Tests:** `src/errors/api_error.rs::tests::{into_response_repo_not_found_returns_404_with_details, into_response_repo_session_expired_returns_401_with_sanitized_message, into_response_repo_invalid_auth_context_returns_401, into_response_repo_validation_error_returns_400_with_details, into_response_oauth_invalid_token_returns_401, into_response_oauth_insufficient_scope_returns_403_with_details}`

### HTTP-021 Service errors keep their identity through repository-typed closures

- **Behavior:** When a non-repository `ApiError` (validation, auth, not-found, conflict and so on) is raised inside a closure that must return `RepositoryError` (transactions, retries), it is boxed as `RepositoryError::ServiceBridge` and unboxed back to the original `ApiError` at the boundary. Status, wire code, message, details and log context are therefore the original's, never a collapsed 500. A bridged error is never retried. `ApiError::Repository(inner)` is unwrapped to `inner` instead, so a transient inner fault stays retryable.
- **Kind:** `hardening`
- **Trigger:** Errors crossing transaction/retry helpers.
- **Source:** `src/errors/api_error.rs:143-150,271-321,514-516,666-671`, `src/errors/repository_error.rs:166-171,244,816-828`
- **Tests:** `src/errors/api_error.rs::tests::{into_repository_error_*, from_repository_unwraps_service_bridge_to_inner_api_error, from_repository_wraps_non_bridge_variants_as_repository, end_to_end_bridged_*, defensive_arm_manually_constructed_repository_service_bridge_delegates}`

### HTTP-022 JSON request bodies

- **Behavior:** JSON bodies are read through `ValidatedJson<T>`: axum's JSON extraction, then the model's `validator` rules. Extraction failures are all `VALIDATION_ERROR` 400 with internal code `invalid_request_body`:
  - Missing or non-JSON `Content-Type` → "Missing 'Content-Type: application/json' header" (400, not 415).
  - Malformed JSON → "Invalid JSON syntax: <serde message>", or "Invalid JSON syntax" if the text cannot be parsed.
  - Wrong type or missing field → "Invalid request body: <serde message without the ' at line …' suffix>", or "Invalid request body format".
  - Body read failure or over the body limit → "Failed to read request body".
  - Any other rejection → "Invalid request format".
  Validation failures follow HTTP-023. Unknown fields are ignored unless a model opts out.
- **Kind:** `rule`
- **Trigger:** Every route with a JSON body (Route table).
- **Source:** `src/extractors/json.rs:77-179,250-273`
- **Tests:** `src/extractors/json.rs::tests::{valid_json, missing_content_type, malformed_json, wrong_type, validated_json_valid_input_returns_200, validated_json_per_field_length_failure_returns_400, validated_json_custom_validator_failure_returns_400, validated_json_schema_level_validator_failure_returns_400, validated_json_wire_format_failure_returns_400}`

### HTTP-023 Field-validation error format

- **Behavior:** A `validator` failure becomes `VALIDATION_ERROR` 400 (internal code `field_validation`):
  - `details` is an object mapping each failing field to an array of message strings. Cross-field (schema-level) errors are listed under the key `_schema`; the crate's `__all__` key never appears.
  - A rule without a custom message yields "Validation failed: <rule code>" (for example "Validation failed: range").
  - The top-level message is "Validation failed: N field(s) invalid", where N counts the failing fields plus 1 if any schema error exists; when only schema errors exist it is "Validation failed: cross-field constraint violated".
  The same conversion is used by `ValidatedJson`, by `ValidatedQuery` (whose filter types validate in `from_raw`), by the multipart extractors, and by pagination limit checks.
- **Kind:** `rule`
- **Trigger:** Any validated request model.
- **Source:** `src/errors/api_error.rs:428-495`, `src/extractors/query.rs:221-295`
- **Tests:** `src/errors/api_error.rs::tests::{from_validation_errors_field_only_produces_field_keyed_details, from_validation_errors_schema_only_produces_schema_keyed_details, from_validation_errors_mixed_field_and_schema_produces_both_keys, from_validation_errors_multiple_fields_counts_correctly, from_validation_errors_fallback_message_when_no_custom_message}`; `src/extractors/query.rs::tests::validated_query_*`

### HTTP-024 Query-string parameters

- **Behavior:** Query strings are parsed with `axum_extra`'s query extractor, so a repeated key fills a `Vec` (`?action=a&action=b`). A parse failure (missing required field, wrong type, bad enum value such as `sort_direction=up`) is `VALIDATION_ERROR` 400 with message "Invalid query parameter: <serde message>" and internal code `invalid_query_parameter`. Any other rejection gives "Invalid query parameters". Unknown keys are ignored. On list routes the pagination struct and the filter struct are parsed independently from the same query string.
- **Kind:** `rule`
- **Trigger:** Every route that reads query parameters.
- **Source:** `src/extractors/query.rs:100-162,274-295`
- **Tests:** `src/extractors/query.rs::tests::{valid_query_params, missing_required_param, wrong_type_param, empty_query_string, vec_query_param_single_value, vec_query_param_multiple_values, vec_query_param_mixed_with_other_params, validated_query_vec_with_per_element_custom_validator_failure_returns_400}`

### HTTP-025 Path parameters

- **Behavior:** A path segment that does not parse into the expected type (for example a malformed UUID) is `VALIDATION_ERROR` 400 with message "Invalid path parameter: <axum message without its 'Invalid URL: ' prefix>" and internal code `invalid_path_parameter`. Routing-side failures (missing parameters, wrong number of parameters, unsupported type) are `INTERNAL_SERVER_ERROR` 500 (`middleware_misconfiguration`). Any other rejection gives 400 "Invalid path parameter".
- **Kind:** `rule`
- **Trigger:** Every route with `{…}` segments.
- **Source:** `src/extractors/path.rs:87-175`
- **Tests:** `src/extractors/path.rs::tests::{valid_uuid_path, malformed_uuid_returns_400, missing_path_param_is_a_500_server_error, non_numeric_for_numeric_path_returns_400}`

### HTTP-026 Input normalization at deserialization

- **Behavior:** Request models normalize fields while deserializing, before validation, using these shared helpers:
  - `normalized_email`: trim, then lowercase the whole address.
  - `normalized_string` and `normalized_optional_string`: trim; collapse every run of Unicode whitespace (tabs and newlines included) to one space; Unicode NFC.
  - `normalized_lowercase_string`: trim and lowercase (provider and method names).
  - `normalized_email_optional_vec`: `normalized_email` applied to each element.
  - `trimmed_string`: trim only (tokens, fingerprints).
  The multipart `name` part uses `normalize_string` too, and email-keyed rate-limit buckets use `normalize_email` (HTTP-037). Which fields use which helper is the owning domain's contract.
- **Kind:** `rule`
- **Trigger:** Deserialization of any model field that opts in.
- **Source:** `src/utils/deserializers.rs:48-315`
- **Tests:** `src/utils/deserializers.rs::tests::{collapses_whitespace, normalizes_string, normalized_email_deserializer, normalized_string_deserializer, normalized_optional_string_deserializer, normalized_email_optional_vec_deserializer_present_multiple, normalized_email_optional_vec_deserializer_empty_vec, normalized_email_optional_vec_deserializer_absent, normalizes_lowercase, normalized_lowercase_string_deserializer, trimmed_string_deserializer}`

### HTTP-027 Shared validator rules

- **Behavior:** `src/validation` provides the custom rules request models attach. Each yields one error per field with the message shown:
  - `validate_time_range_business_rules(from, to)`: both bounds or neither ("Time range must specify both 'from' and 'to', or neither. Half-bounded ranges are not supported."); `from < to` strictly ("Time range 'from' must be before 'to'"); span at most 365 whole days ("Time range cannot exceed 365 days").
  - `validate_email_filter_list`: each element at most 254 bytes ("Email filter at index i exceeds 254 characters") and a valid email ("Invalid email format in filter at index i"). The value is not echoed.
  - `validate_not_only_whitespace`: "Field cannot be empty or contain only whitespace".
  - `validate_future_datetime`: strictly after now ("Date must be in the future").
  - `validate_permission_list`, `validate_{user,workspace}_audit_action_list`, `validate_{user,workspace}_audit_resource_type_list`: each value must be in the enum's set; the message echoes the bad value and lists all valid ones.
  - `validate_device_fingerprint`: exactly 64 ASCII hex chars (separate length and charset messages).
  - `validate_verification_code`: ASCII digits only (length is the model's).
  - `validate_password_not_common`: rejects members of the embedded blocklist (case-insensitive), single-repeated-character passwords, and all-digit strictly ascending or descending runs of length ≥ 8.
  - `validate_idempotency_key`: HTTP-040.
  Consumers: AUTH, USER, WS, AUDIT request and filter models.
- **Kind:** `rule`
- **Trigger:** Model validation.
- **Source:** `src/validation/mod.rs:37-645`
- **Tests:** `src/validation/mod.rs::tests::*` (44 tests: `validate_time_range_business_rules_*`, `validate_email_filter_list_*`, `validates_*`, `validate_*_audit_*`, `validate_verification_code_*`, `validate_idempotency_key_*`, `validate_password_not_common_*`)

### HTTP-028 Pagination parameters

- **Behavior:** List endpoints accept:
  - `limit`: integer 1–100, default 25.
  - `sort_by`: default `created_at`.
  - `sort_direction`: `asc` or `desc` (lowercase), default `desc`.
  - `after` or `before`: an opaque cursor.
  Validation runs in a fixed order and stops at the first failure:
  1. `limit` out of range → `VALIDATION_ERROR` 400, `field_validation`, details `{"limit":["Validation failed: range"]}`. A non-integer `limit` fails earlier, as a query parse error (HTTP-024).
  2. `sort_by` not in the endpoint allow-list → 400 "Invalid sort field: '<x>'. Allowed fields are: created_at" (code `invalid_cursor`). Every endpoint allows only `created_at`.
  3. Both `after` and `before` given → 400 "Cannot specify both 'after' and 'before' cursors. Use 'after' for forward pagination or 'before' for backward pagination." (`invalid_cursor`).
  4. Cursor decoding and the sort-consistency check (HTTP-029).
- **Kind:** `rule`
- **Trigger:** The 9 paginated lists (Route table rows 25, 28, 31, 32, 37, 53, 57, 62, 64).
- **Source:** `src/utils/pagination.rs:165-239,353-415`, `src/handlers/files.rs:220,356`, `src/handlers/sessions.rs:97`, `src/handlers/user_audit_logs.rs:42`, `src/handlers/user_invitations.rs:50`, `src/handlers/workspaces.rs:55`, `src/handlers/workspace_audit_logs.rs:57`, `src/handlers/workspace_members.rs:49,206`
- **Tests:** `src/utils/pagination.rs::tests::{pagination_params_validation_*, pagination_params_serde_*, pagination_query_first_page, pagination_query_rejects_invalid_limit, pagination_query_rejects_invalid_sort_field, pagination_query_rejects_both_cursors}`

### HTTP-029 Cursors and paginated responses

- **Behavior:**
  - **Cursor format:** base64url without padding of the JSON `{"t": <unix ms>, "i": "<uuid>", "s": "<sort_by>", "d": "asc"|"desc"}`. `t` is the row's sort column (`created_at`, or `joined_at` for members, `invited_at` for invitations) truncated to milliseconds.
  - **Decode errors** (all 400, code `invalid_cursor`): "Invalid cursor format" (not base64url), "Invalid cursor encoding" (not UTF-8), "Invalid cursor structure" (bad JSON), "Invalid cursor: timestamp out of valid range".
  - **Sort consistency:** a cursor whose `s`/`d` differ from the request's `sort_by`/`sort_direction` → 400 "Cursor was created with sort 's:d', but request uses 'x:y'. When changing sort parameters, omit the cursor to start from the beginning."
  - **Page semantics:** order is `(sort column, id)` in the requested direction. `after` returns rows after the cursor in that order (older rows for `desc`); `before` returns the rows immediately before it, still displayed in the requested order.
    - First page: `has_next` is true when more rows exist; `has_previous` is false.
    - `after` page: `has_previous` is always true; `has_next` is true when more rows exist.
    - `before` page: `has_next` is always true; `has_previous` is true when more rows exist.
    - `next_cursor` is built from the last item only when `has_next` and the page is non-empty; `previous_cursor` from the first item only when `has_previous` and non-empty.
  - **Response shape:** `{data:[…], meta, pagination:{limit, has_next, has_previous, next_cursor?, previous_cursor?, sort_by, sort_direction, total_count?}}`. `total_count` (the total before slicing) is present only on the three lists paginated in memory (my workspaces, workspace invitations, pending invitations) and omitted elsewhere.
- **Kind:** `rule`
- **Trigger:** Paginated list requests.
- **Guarantees:**
  - **DB-level pages:** one query with `(sort_col, id) </> ($ts, $id)` and `LIMIT limit+1`; backward pages are fetched in reverse order, then flipped.
  - Cursors are not signed. A forged cursor only moves the window inside the caller's RLS scope.
- **Source:** `src/utils/pagination.rs:417-462,490-631,734-932`, `src/repositories/postgresql/keyset.rs:39-94`, `src/errors/response_envelope.rs:372-503`, `src/repositories/postgresql/workspace_member_repository.rs:300,878-880,1026-1028`, `src/repositories/postgresql/workspace_repository.rs:724`
- **Tests:** `src/utils/pagination.rs::tests::{cursor_*, pagination_query_decodes_*, pagination_query_rejects_cursor_*, pagination_query_rejects_invalid_cursor_string, pagination_query_build_meta, paginate_in_memory_*, assemble_*}`; `src/repositories/postgresql/keyset.rs::tests::*`; `src/errors/response_envelope.rs::tests::{pagination_meta_omits_optional_fields_when_none, pagination_meta_includes_optional_fields_when_some, paginated_success_populates_data_pagination_and_meta}`; `tests/workspace_member_pagination_live.rs`

### HTTP-030 Rate-limit tiers, keys and exemptions

- **Behavior:** The rate-limit layer picks one bucket per request:
  - Requests carrying an `AuthContext` on `/telemetry/traces`, `/telemetry/metrics`, `/telemetry/logs` (exact paths) → `telemetry_proxy:user:<user_id>`, limit `RATE_LIMIT_TELEMETRY_PROXY_PER_USER` (600).
  - Any other authenticated request → `user:<user_id>`, limit `RATE_LIMIT_PER_USER` (100). The verification-exempt, user-scoped and workspace-scoped groups share this one bucket.
  - Public-group requests → `ip:<client_ip>` (HTTP-011, possibly `ip:unknown`), limit `RATE_LIMIT_PER_IP` (20). Every public route shares it, downloads and `/auth/config` included.
  All tiers use the window `RATE_LIMIT_WINDOW_SECS` (60). Skipped without counting or headers: paths starting with `/permissions/schema` or `/audit-actions/schema`, paths ending with `/users/me/events`, and paths starting with `/health` (never mounted).
- **Kind:** `rule`
- **Trigger:** Requests on groups P, X, U, W, T.
- **Effects:** One `sp_ratelimit_increment` call per counted request (HTTP-031).
- **Source:** `src/middleware/rate_limit.rs:73-92,124-214`, `src/config/environment.rs:429-436`
- **Tests:** `src/middleware/rate_limit.rs::tests::{telemetry_paths_use_dedicated_bucket_and_limit, other_authenticated_paths_use_default_bucket_and_limit, telemetry_match_is_exact_path}`; `tests/rate_limit_middleware_http.rs::{rate_limit_exempt_sse_path_bypasses_exhausted_bucket, rate_limit_ip_tier_enforced_and_keyed_per_ip}`

### HTTP-031 Fixed-window counter

- **Behavior:** Each check adds `amount` (1 per request; a size for quotas such as the telemetry KiB budget) to the key's counter in one atomic upsert, then compares:
  - A new key, or a key whose `window_start + window_secs <= NOW()`, starts a new window at `NOW()` with count = `amount`. Otherwise the count grows within the current window.
  - `allowed = count <= limit`; `remaining = max(limit - count, 0)`; `reset_at = window_start + window_secs`.
  Rejected requests are counted too. Windows are anchored at the first request after expiry, not aligned to the clock, and time comes from the database clock.
- **Kind:** `rule`
- **Trigger:** Every counted request (HTTP-030) and every service-level `check_rate_limit`, `check_quota` or `check_rate_limit_by_email` call.
- **Input & validation:** `amount` above `i32::MAX` is clamped; `window_secs` is cast to `i32`.
- **Effects:** Upsert on `rate_limits (key, count, window_start, window_secs)`; `window_secs` is overwritten with the caller's value on every call, but the expiry test uses the stored value.
- **Guarantees:** The upsert is atomic and race-free per key (`INSERT … ON CONFLICT DO UPDATE`).
- **Source:** `src/services/rate_limit_service.rs:187-279`, `src/repositories/postgresql/rate_limit_repository.rs:62-101`, `migrations/0006_background_tasks.sql:314-344`
- **Tests:** `src/services/rate_limit_service.rs::tests::{check_rate_limit_allowed, check_rate_limit_exceeded, check_quota_consumes_amount, check_rate_limit_multiple_keys}`; `tests/rate_limit_repository_live.rs::{increment_counts_within_window_and_resets_after_expiry, increment_adds_amount_and_resets_to_it}`

### HTTP-032 Rate-limit rejection (429)

- **Behavior:** When a middleware bucket is exceeded and enforcement is on, the request stops at the limiter with HTTP 429:
  - Envelope `RATE_LIMIT_EXCEEDED`, message "Rate limit exceeded. Try again in N seconds", no details.
  - Headers `X-RateLimit-Limit: <limit>`, `X-RateLimit-Remaining: 0`, `X-RateLimit-Reset: <reset_at, unix seconds>` and `Retry-After: N`, where N = whole seconds until `reset_at` (truncated, at least 0).
  Handler-level buckets (AUTH-006, USER-015, USER-021 and others) return the same envelope and the same four headers for their own bucket, but the outer limiter then overwrites the three `X-RateLimit-*` headers (HTTP-033).
- **Kind:** `rule`
- **Trigger:** Count over limit with `ENFORCE_RATE_LIMITS=true`.
- **Effects:** The handler does not run. Logged at WARN with `{limit, remaining, reset_at}` in the details. Never reaches idempotency, so it is never cached.
- **Source:** `src/middleware/rate_limit.rs:31-50,222-260`, `src/errors/api_error.rs:363-368,1042-1092`
- **Tests:** `tests/rate_limit_middleware_http.rs::{rate_limit_enforced_returns_429_with_headers, rate_limit_ip_tier_enforced_and_keyed_per_ip}`; `src/errors/api_error.rs::tests::into_response_rate_limit_exceeded_returns_429_with_all_headers`

### HTTP-033 Rate-limit headers on responses that pass the limiter

- **Behavior:** Every response returned through the limiter, whether a success, a handler error or an idempotent replay, gets `X-RateLimit-Limit`, `X-RateLimit-Remaining` and `X-RateLimit-Reset` for the middleware bucket, replacing any same-named headers set further in. `Retry-After` is not added. No rate-limit headers are added on:
  - exempt paths and health;
  - requests rejected by authentication (the limiter never runs);
  - requests where the limiter's DB call failed (HTTP-035).
- **Kind:** `rule`
- **Trigger:** Counted requests that are not rejected by the limiter.
- **Source:** `src/middleware/rate_limit.rs:281-294`
- **Tests:** `tests/rate_limit_middleware_http.rs::rate_limit_enforced_returns_429_with_headers` (header budget on the allowed request)

### HTTP-034 Observability-only rate limiting

- **Behavior:** With `ENFORCE_RATE_LIMITS=false`, over-limit requests are allowed. They still get the `X-RateLimit-*` headers (with `Remaining: 0`), and each one logs a WARN "Rate limit exceeded (observability-only mode)" with `enforcement_status=not_enforced`. Handler- and service-level buckets honor the same flag (each checks it before returning 429); the telemetry byte quota does not (OBS).
- **Kind:** `rule`
- **Trigger:** Over-limit request with enforcement off.
- **Input & validation:** `ENFORCE_RATE_LIMITS` must be `true` or `false` (case-insensitive), default `true`.
- **Source:** `src/middleware/rate_limit.rs:261-279`, `src/handlers/auth.rs:414`, `src/services/user_service.rs:608`, `src/services/identity_service.rs:784`
- **Tests:** `tests/rate_limit_middleware_http.rs::rate_limit_observability_only_allows_over_limit`

### HTTP-035 Rate limiter failures fail open

- **Behavior:** If the counter update fails (any DB error), the request proceeds unthrottled with no rate-limit headers, and a WARN log `rate_limit_check_failed` with `action=failing_open` is emitted.
- **Kind:** `hardening`
- **Trigger:** DB error in `sp_ratelimit_increment`.
- **Source:** `src/middleware/rate_limit.rs:296-313`
- **Tests:** `tests/rate_limit_middleware_http.rs::rate_limit_fails_open_when_the_limiter_errors`

### HTTP-036 Rate limits hold across instances

- **Behavior:** All counters live in the shared `rate_limits` table, so a key's limit applies to the sum of its traffic across every API replica, not per replica.
- **Kind:** `guarantee`
- **Trigger:** Multi-replica deployments.
- **Guarantees:** Atomic upsert per key; window expiry is evaluated with the database clock, so instance clock skew does not matter.
- **Source:** `src/repositories/postgresql/rate_limit_repository.rs:1-32,62-101`, `migrations/0002_tables.sql:545-574`
- **Tests:** None found for the multi-instance aspect.

### HTTP-037 Email-keyed buckets never store the email

- **Behavior:** `check_rate_limit_by_email(prefix, email, …)` normalizes the email (trim, lowercase), HMAC-SHA256 hashes it with the active key and uses the key `<prefix>:<64-hex hmac>`, so syntactically different spellings share one counter and the raw email never reaches `rate_limits`. Callers: `credential_login`, `password_reset`, `password_reset_verify` (AUTH).
- **Kind:** `guarantee` (privacy)
- **Trigger:** Service-level email-keyed checks.
- **Source:** `src/services/rate_limit_service.rs:293-315`, `src/handlers/auth.rs:400-401,913-914,1003-1004`
- **Tests:** `src/services/rate_limit_service.rs::tests::{check_rate_limit_by_email_hashes_email, check_rate_limit_by_email_normalizes_case_and_whitespace, check_rate_limit_by_email_isolates_prefixes, check_rate_limit_by_email_isolates_users}`; `tests/rate_limit_middleware_http.rs::credential_login_email_lockout_returns_429_after_max_attempts`

### HTTP-038 Expired rate-limit windows are purged

- **Behavior:** The `rate_limit_cleanup` background job deletes every `rate_limits` row with `window_start + window_secs < NOW()` every `RATE_LIMIT_CLEANUP_INTERVAL_HOURS` (default 1), with up to 3 attempts. Expired rows that have not been purged yet do not change any decision, because the upsert resets expired windows itself.
- **Kind:** `hardening`
- **Trigger:** Background job `rate_limit_cleanup` (scheduling per RT-028; skipped when `ENABLE_BACKGROUND_CLEANUP=false`).
- **Effects:** Info log with the removed count when > 0.
- **Source:** `src/server.rs:1518-1565`, `src/services/rate_limit_service.rs:326-336`, `src/repositories/postgresql/rate_limit_repository.rs:140-163`, `migrations/0006_background_tasks.sql:279-292`
- **Tests:** `tests/rate_limit_repository_live.rs::cleanup_deletes_only_windows_past_expiry`

### HTTP-039 Opt-in idempotent retries

- **Behavior:** A POST, PUT or DELETE on the public, verification-exempt, user-scoped or workspace-scoped groups that carries an `Idempotency-Key` header is executed at most once per key. The first request runs and its response is stored; a retry with the same key and the same body gets the stored response back (HTTP-044). Without the header, or for other methods, the layer does nothing. The telemetry and health routes have no idempotency layer, so the header is ignored there.
- **Kind:** `capability`
- **Trigger:** POST/PUT/DELETE with `Idempotency-Key` on groups P, X, U, W.
- **Effects:** Rows in `idempotency_cache` (HTTP-047).
- **Guarantees:** Rate limiting and the email gate run before this layer, so their rejections are never stored.
- **Source:** `src/middleware/idempotency.rs:286-337`, `src/server.rs:593-668`
- **Tests:** `tests/http_smoke.rs::signup_login_and_auth_reject_through_the_real_stack` (steps 8–10)

### HTTP-040 Idempotency-Key format

- **Behavior:** The header must be:
  - valid UTF-8, else 400 "Invalid Idempotency-Key header format";
  - non-empty, else "Idempotency key cannot be empty";
  - at most 100 bytes, else "Idempotency key must be less than 100 characters";
  - parseable by the `uuid` crate, else "Idempotency key must be valid UUID v4 format (e.g., '550e8400-e29b-41d4-a716-446655440000')";
  - version 4, else "Idempotency key must be UUID v4 format, got version N. Example: '550e8400-e29b-41d4-a716-446655440000'".
  Every failure is `VALIDATION_ERROR` 400 with internal code `invalid_header` and is returned before any storage access. The key is used as sent: hyphenated, simple, braced, `urn:uuid:` and upper-case spellings of one UUID are distinct keys.
- **Kind:** `rule`
- **Trigger:** Keyed POST/PUT/DELETE on idempotent groups.
- **Source:** `src/middleware/idempotency.rs:307-378`, `src/validation/mod.rs:521-566`
- **Tests:** `src/validation/mod.rs::tests::{validate_idempotency_key_valid_uuid_v4, validate_idempotency_key_invalid_empty, validate_idempotency_key_invalid_too_long, validate_idempotency_key_invalid_not_uuid, validate_idempotency_key_invalid_wrong_uuid_version, validate_idempotency_key_boundary_lengths, validate_idempotency_key_error_messages_helpful}`

### HTTP-041 Idempotency scope

- **Behavior:** A stored response is found only with the composite key `<key>:<user_slot>:<workspace_slot>:<METHOD>:<path>`, where `path` excludes `/v1` and the query string. The slots are:
  - Authenticated request on a path starting with `/users/me` → (`<user_id>`, `user-scoped`).
  - Other authenticated request → (`<user_id>`, the raw `X-Workspace-Id` header text, or `user-scoped` if absent). The header is never taken from the stored default workspace.
  - Unauthenticated (public group) → (`public`, `public`); isolation then rests on the key's randomness plus the body fingerprint.
  Different users, workspaces, methods or paths therefore never share an entry, even with the same key.
- **Kind:** `guarantee` (isolation)
- **Trigger:** Keyed requests.
- **Source:** `src/middleware/idempotency.rs:380-386,483-500,821-868`
- **Tests:** `src/middleware/idempotency.rs::tests::{tenancy_slots_workspace_scoped_key_on_header_not_auth_context, tenancy_slots_user_scoped_for_users_me, tenancy_slots_workspace_scoped_falls_back_to_user_scoped_without_header, tenancy_slots_public_when_unauthenticated, composite_cache_key_is_workspace_scoped_from_header, composite_cache_key_is_user_scoped_with_placeholder, composite_cache_key_is_public_scoped_when_unauthenticated}`

### HTTP-042 Payload fingerprint must match

- **Behavior:** The fingerprint is the hex SHA-256 of the raw request body bytes. Reusing a key whose stored response is complete with a different body returns `VALIDATION_ERROR` 400 (internal code `idempotency_fingerprint_mismatch`) "Idempotency key '<key>' was previously used with different request parameters. To retry with different parameters, please use a new idempotency key." The handler does not run. Query string and headers are not part of the fingerprint. While the original is still in flight the fingerprint is not compared (HTTP-043 answers first).
- **Kind:** `rule`
- **Trigger:** Same composite key, completed entry, different body.
- **Source:** `src/middleware/idempotency.rs:463-469,688-720,815-819`
- **Tests:** `tests/http_smoke.rs::signup_login_and_auth_reject_through_the_real_stack` (step 9)

### HTTP-043 In-flight duplicate is a 409

- **Behavior:** While the first request for a composite key is still being processed, any request with the same composite key gets 409 `RESOURCE_CONFLICT` with the generic message "Resource already exists" and no details. The internal log context carries `constraint_code=CONCURRENT_REQUEST` and "A request with this idempotency key is currently being processed. Please retry after a short delay."
- **Kind:** `rule`
- **Trigger:** Same composite key while the row is `processing`.
- **Guarantees:** The lock is the table's primary key (`INSERT … ON CONFLICT DO NOTHING RETURNING`), so exactly one of any number of concurrent requests, on any replica, acquires it. If the row disappears between the failed insert and the read-back, the request proceeds as the lock owner.
- **Source:** `src/middleware/idempotency.rs:655-678`, `src/repositories/postgresql/idempotency_repository.rs:80-176,253-263`, `src/errors/api_error.rs:211-228,1015-1020`
- **Tests:** `tests/idempotency_middleware_http.rs::idempotency_concurrent_duplicate_returns_409`; `tests/idempotency_repository_live.rs::acquire_conflicts_while_processing_then_replays_completed_response`

### HTTP-044 Replay of the stored response

- **Behavior:** A completed entry with a matching fingerprint is answered without running the handler. The replay has:
  - the stored status code;
  - the stored body byte-for-byte, so `meta.trace_id` and `meta.timestamp` are the original request's;
  - every stored response header;
  - three new headers: `X-Idempotency-Replayed: true`, `X-Original-Trace-Id: <original trace id>`, `X-Idempotency-Cache-Age: <whole seconds since the original>`.
  Fresh `X-RateLimit-*` headers are added outside (HTTP-033). Stored headers exclude `X-RateLimit-*`, `X-Idempotency-Replayed`, `X-Original-Trace-Id`, `Date`, `Set-Cookie`, `Authorization`, `Cookie`, `Age`, `Server` and `Via`, and headers whose value is not valid UTF-8.
- **Kind:** `capability`
- **Trigger:** Same composite key and body after completion.
- **Effects:** The replay still counts against the rate limit.
- **Source:** `src/middleware/idempotency.rs:41-104,722-788`
- **Tests:** `tests/idempotency_middleware_http.rs::idempotency_replay_carries_metadata_headers`; `tests/http_smoke.rs` (step 10); `src/middleware/idempotency.rs::tests::{cacheable_header_denylist_rejects_dynamic_and_security_headers, cacheable_header_denylist_is_case_insensitive, cacheable_header_allows_business_content_headers, extract_cacheable_headers_drops_excluded_keeps_business}`

### HTTP-045 What is stored and what is released

- **Behavior:** After the handler runs:
  - Any 2xx, or any 4xx except 429, is stored and the row becomes `completed` before the response is returned. Validation, auth and conflict errors therefore replay too.
  - A 5xx or a 429 (for example a handler-level lockout) deletes the processing row, so a same-key retry runs the handler again.
  - A handler panic deletes the row, then the panic continues to catch_panic (500, HTTP-014).
  - A request the client abandons before the handler finishes leaves the row `processing`; same-key requests get 409 until the stale-lock sweep (HTTP-048).
- **Kind:** `rule`
- **Trigger:** Completion of a request that acquired the lock.
- **Source:** `src/middleware/idempotency.rs:126-133,511-653`, `src/repositories/postgresql/idempotency_repository.rs:348-372`
- **Tests:** `src/middleware/idempotency.rs::tests::is_transient_status_is_5xx_and_429_only`; `tests/idempotency_middleware_http.rs::transient_429_releases_lock_and_is_not_poisoned`; `tests/idempotency_panic_http.rs::handler_panic_releases_the_idempotency_key_for_a_retry`; `tests/idempotency_repository_live.rs::release_lock_frees_the_key_but_spares_a_completed_row`

### HTTP-046 Keyed request bodies are buffered under a ceiling

- **Behavior:** To fingerprint the body, the layer buffers it in full, up to max(`MAX_REQUEST_BODY_BYTES`, `STORAGE_MAX_UPLOAD_BYTES`) (10 MiB by default). A declared `Content-Length` above the ceiling is rejected first with `PAYLOAD_TOO_LARGE` 413 "Request body exceeds the maximum allowed size", without reading. A body that fails to read, including one that exceeds the ceiling without an honest `Content-Length`, is not rejected: the request continues without idempotency and the handler receives an empty body (WARN `body_read_failed`, `failing_open`). The route's own limit still applies in its extractor (HTTP-015).
- **Kind:** `hardening`
- **Trigger:** Keyed POST/PUT/DELETE.
- **Source:** `src/middleware/idempotency.rs:106-124,408-461`
- **Tests:** `src/middleware/idempotency.rs::tests::idempotency_body_limit_is_the_larger_of_both_limits`; `tests/idempotency_middleware_http.rs::idempotency_rejects_oversized_declared_content_length_before_buffering`

### HTTP-047 Stored responses are encrypted and UTF-8 only

- **Behavior:** Each `idempotency_cache` row holds:
  - `composite_key` (primary key);
  - `status`: `processing` or `completed`;
  - the original trace id;
  - `status_code`;
  - `response_body`, encrypted with `DatabaseEncryption` (AES-256-GCM, base64 text);
  - `request_body_hash`;
  - `response_headers` (JSONB);
  - `created_at`.
  Only UTF-8 response bodies can be stored. A non-UTF-8 2xx/4xx body fails completion: the client still gets its response, but the row stays `processing` (WARN `complete_processing_failed`), so same-key requests get 409 until the sweep. If the stored response body or headers cannot be read back (decryption fails, NULL columns, unknown status), the lock attempt fails and the request runs without idempotency (HTTP-049).
- **Kind:** `guarantee` (security: cached tokens and PII are encrypted at rest)
- **Trigger:** Completion and replay.
- **Effects:** The re-encryption job rewrites these rows when keys rotate (DATA).
- **Source:** `src/repositories/postgresql/idempotency_repository.rs:177-251,279-336`, `migrations/0002_tables.sql:444-501`
- **Tests:** `tests/idempotency_repository_live.rs::acquire_conflicts_while_processing_then_replays_completed_response`

### HTTP-048 Idempotency expiry and stale-lock recovery

- **Behavior:** The `idempotency_cleanup` background job (every `IDEMPOTENCY_CLEANUP_INTERVAL_HOURS`, default 1, up to 3 attempts) deletes completed rows older than 24 h and processing rows older than 5 min. Nothing on the request path checks age: a row is honored (replayed, or answered with 409) until the sweep deletes it.
- **Kind:** `rule`
- **Trigger:** Background job `idempotency_cleanup` (RT-028); skipped when `ENABLE_BACKGROUND_CLEANUP=false`.
- **Guarantees:** Completion and release only touch rows still `processing` (`WHERE status='processing'`), so a completed row survives a late release, and a swept lock makes a late completion a no-op.
- **Source:** `src/server.rs:1650-1705`, `src/repositories/postgresql/idempotency_repository.rs:309-318,354-358,384-411`, `migrations/0006_background_tasks.sql:160-189`
- **Tests:** `tests/idempotency_repository_live.rs::cleanup_reclaims_stale_locks_and_expired_completions_only`

### HTTP-049 Idempotency infrastructure failures fail open

- **Behavior:** If the lock cannot be acquired or read (any DB or decryption error), the request runs without idempotency (WARN `lock_acquisition_failed`). If a release fails, the row stays until the sweep (WARN `lock_release_failed`). If the response body cannot be read for storage, the row is released and the client receives the status and headers with an empty body. In every case the client receives a response.
- **Kind:** `hardening`
- **Trigger:** DB or crypto failure inside the idempotency layer.
- **Source:** `src/middleware/idempotency.rs:389-406,519-528,553-584,607-629,640-650,790-805`
- **Tests:** `src/services/idempotency_service.rs::tests::{try_acquire_lock_translates_repo_error, cleanup_expired_entries_propagates_repo_error}` (service layer only).

## Data owned

- **`rate_limits`** (`migrations/0002_tables.sql:545-574`): `key TEXT PK`, `count INTEGER`, `window_start TIMESTAMPTZ`, `window_secs INTEGER`. Index `idx_rate_limits_window_start` (`0003_indexes.sql:152`). Key prefixes: `ip:`, `user:`, `telemetry_proxy:user:` (middleware); `credential_login:`, `password_reset:`, `password_reset_verify:` (HMAC of email), `verification_resend:`, `password_change:`, `email_change:` (user id), plus the telemetry byte-quota key (OBS).
- **`idempotency_cache`** (`0002_tables.sql:422-501`): `composite_key TEXT PK`, `status TEXT CHECK IN ('processing','completed') DEFAULT 'completed'`, `original_trace_id VARCHAR(32)`, `status_code INTEGER NULL CHECK 200–599`, `response_body TEXT NULL` (encrypted), `request_body_hash TEXT`, `response_headers JSONB DEFAULT '{}'`, `created_at TIMESTAMPTZ`. Index `idx_idempotency_cache_created_at` (`0003_indexes.sql:143-144`). Explicit `autovacuum_enabled = true`.
- **RLS:** none on either table (infrastructure tables; `0004_row_level_security.sql:47-51`). Access through `app_user` table grants (`0008_grants.sql:37,43`).
- **Stored procedures:** `sp_ratelimit_increment(TEXT, INTEGER, INTEGER)` (invoker; `0006_background_tasks.sql:314-344`, grant `0008_grants.sql:150`); `sp_cleanup_expired_rate_limits()` (invoker; `0006:279-292`, grant `0008:148`); `sp_cleanup_expired_idempotency_entries()` (`SECURITY DEFINER`, `search_path = pg_catalog, public`; `0006:167-189`, grant `0008:144`).
- **Background tasks (bodies owned here, scheduling per RT-028):** `rate_limit_cleanup` (`server.rs:1518-1565`), `idempotency_cleanup` (`server.rs:1650-1705`).
- **Process-local state:** the shutdown flag and broadcast channel (`shutdown.rs:25-53`); the request task-local `REQUEST_CONTEXT` (`request_context.rs:31-33`).

## Configuration

| Variable | Default | Validation | Used by |
|---|---|---|---|
| `SERVER_HOST` | `0.0.0.0` | none | bind (HTTP-002); Compose pins `0.0.0.0` |
| `SERVER_PORT` | `8080` | u16 parse | bind |
| `ENVIRONMENT` | `development` | none; only the exact value `development` is special | CORS mode (HTTP-010), console log level (OBS) |
| `CORS_ORIGINS` | empty | comma split, trim, blanks dropped; `*`/unparsable entries dropped at startup | HTTP-010 |
| `MAX_REQUEST_BODY_BYTES` | 2,097,152 | > 0 | global body limit, idempotency ceiling |
| `STORAGE_MAX_UPLOAD_BYTES` | 10,485,760 | > 0 | upload routes (FILE-005), idempotency ceiling |
| `TELEMETRY_PROXY_MAX_BODY_BYTES` | 1,048,576 | > 0 (plus OBS cross-checks) | telemetry group body limit |
| `ENFORCE_RATE_LIMITS` | `true` | `true`/`false`, case-insensitive | HTTP-032/034 and handler-level buckets |
| `RATE_LIMIT_PER_IP` | 20 | > 0; warning if > 100 | public tier |
| `RATE_LIMIT_PER_USER` | 100 | > 0; warning if > 1000 | user tier |
| `RATE_LIMIT_TELEMETRY_PROXY_PER_USER` | 600 | > 0 | telemetry tier |
| `RATE_LIMIT_WINDOW_SECS` | 60 | > 0 | all three middleware tiers |
| `RATE_LIMIT_CLEANUP_INTERVAL_HOURS` | 1 | > 0 | HTTP-038 |
| `IDEMPOTENCY_CLEANUP_INTERVAL_HOURS` | 1 | > 0; warning if > 24 | HTTP-048 |
| `ENABLE_BACKGROUND_CLEANUP` | `true` | boolean | gates both cleanups (RT-028) |
| `REQUIRE_EMAIL_VERIFICATION` | `false` | boolean | whether the mounted email gate acts (AUTH-038) |
| `RUST_LOG` | unset | EnvFilter syntax | replaces the console log filter (OBS) |

Hard-coded: `API_VERSION = "v1"`; pagination limit 1–100, default 25, sort `created_at`/`desc`; idempotency TTL 24 h, stale lock 5 min, key ≤ 100 bytes; shutdown broadcast capacity 16; background shutdown wait 10 s per task; CORS methods and headers (HTTP-010). Parse errors in any variable abort startup (`environment.rs:213-251`). Rules: `src/config/environment.rs:325-520,622-963,1430-1520`.

## Cross-domain dependencies

**This domain calls:**
- AUTH: `user_scoped_auth_middleware`, `workspace_scoped_auth_middleware`, `enforce_email_verification_middleware` (layered here), and `AuthContext` (read by rate limiting and idempotency).
- OBS: the observability service (init, flush, shutdown), `MetricsService` (metrics layer), and the span or trace id helpers behind `meta.trace_id`.
- DATA: `create_pool`, `DatabaseEncryption` (idempotency body encryption, HMAC for email buckets), the key manager at startup, `migrate::run`, and the background-task runner and distributed locks (via RT).
- RT, MAIL and FILE: services constructed at startup (SSE, SMTP, S3).
- OPS: `Config::from_env` and `validate`; Caddy's `X-Forwarded-For` handling; the Compose healthcheck on `/health/ready`.

**Called by this domain's consumers:**
- Every handler: `ApiResponse`, `PaginatedApiResponse`, `ApiError` and its helpers, the custom extractors, `PaginationQuery`.
- Every repository: `RepositoryError` (with `with_operation_context`), and `keyset::*` with `assemble_keyset_page` or `paginate_in_memory` for lists.
- Request models in AUTH, USER, WS, FILE and AUDIT: the normalizing deserializers and the shared validators (HTTP-026/027).
- `RateLimitService` callers:
  - AUTH handlers: `credential_login`, `verification_resend`, `password_reset`, `password_reset_verify`.
  - USER and AUTH services: `password_change`, `email_change`.
  - OBS: the telemetry proxy's `check_quota`.
- AUDIT services: `REQUEST_CONTEXT` (client IP and user agent).
- RT (SSE streams): `AppState::subscribe_shutdown`.

## Error code catalog

**Wire codes** (`error.code`; `response_envelope.rs:242-276`). These are the only codes clients see.

| Wire code | HTTP | Meaning |
|---|---|---|
| `VALIDATION_ERROR` | 400 | Malformed request (body, query, path, header), failed field validation, invalid cursor, business-rule rejection, bad idempotency key or fingerprint mismatch, OAuth provider not configured |
| `AUTHENTICATION_FAILED` | 401 | Missing or invalid credentials, token or session |
| `PERMISSION_DENIED` | 403 | Authenticated but not allowed (permission, workspace access, email verification, privilege escalation, OAuth scope or unverified provider email) |
| `RESOURCE_NOT_FOUND` | 404 | Entity or object does not exist or is not visible |
| `RESOURCE_CONFLICT` | 409 | Constraint violation, idempotency in-flight duplicate, identity or email conflicts |
| `PAYLOAD_TOO_LARGE` | 413 | Body over the route limit (upload, multipart, idempotency pre-check) |
| `RATE_LIMIT_EXCEEDED` | 429 | Bucket exhausted (middleware or handler-level) |
| `INTERNAL_SERVER_ERROR` | 500 | Unexpected failure; message always "An unexpected error occurred" |
| `DATA_INTEGRITY_ERROR` | 500 | Stored data failed to decode or decrypt, or a value could not be encoded; generic message |
| `SERVICE_UNAVAILABLE` | 503, also 502 | Dependency unavailable (503) or OAuth provider failure (502) |

**Internal codes → wire** (internal codes appear only in logs as `error_type` / `error_code`):

| Source | Internal code(s) | HTTP | Wire code | Wire message / details |
|---|---|---|---|---|
| `ApiError::Validation` (`error_type=validation_error`) | `field_validation`, `invalid_cursor`, `invalid_path_parameter`, `invalid_query_parameter`, `invalid_request_body`, `invalid_header`, `idempotency_fingerprint_mismatch`, `business_rule` | 400 | `VALIDATION_ERROR` | origin message; details passed through |
| `ApiError::PayloadTooLarge` (`payload_too_large`) | `payload_too_large` | 413 | `PAYLOAD_TOO_LARGE` | origin message |
| `ApiError::NotFound` (`not_found`) | `resource_not_found` | 404 | `RESOURCE_NOT_FOUND` | "<entity> not found: <id>"; `{entity_type, entity_id}` |
| `ApiError::Conflict` (`conflict`) | `resource_conflict` | 409 | `RESOURCE_CONFLICT` | "Resource already exists"; none |
| `ApiError::Internal` (`internal_error`) | `internal`, `crypto_failure`, `middleware_misconfiguration`, `email_failure`, `handler_panic` | 500 | `INTERNAL_SERVER_ERROR` | generic; none |
| `ApiError::ServiceUnavailable` (`service_unavailable`) | `dependency_unavailable`, `smtp_temporary_failure` | 503 | `SERVICE_UNAVAILABLE` | "Service temporarily unavailable"; none |
| `ApiError::RateLimitExceeded` (`rate_limit_exceeded`) | `rate_limit_exceeded` | 429 | `RATE_LIMIT_EXCEEDED` | "Rate limit exceeded. Try again in N seconds"; none (headers instead) |
| `RepositoryError` (`repository_error`) | `not_found` | 404 | `RESOURCE_NOT_FOUND` | origin message; details passed through (HTTP-020) |
| | `session_expired` | 401 | `AUTHENTICATION_FAILED` | "Session expired"; `{session_id, expires_at}` |
| | `invalid_auth_context` | 401 | `AUTHENTICATION_FAILED` | origin message; `{reason, user_id, workspace_id}` |
| | `validation_error` | 400 | `VALIDATION_ERROR` | origin message; `{field, value_type, error}` |
| | `constraint_violation` (23505, 23503) | 409 | `RESOURCE_CONFLICT` | "Resource already exists"; none |
| | `connection_failed` | 503 | `SERVICE_UNAVAILABLE` | "Database connection unavailable"; none |
| | `transient_error` (40001, 40P01), `query_failed` | 500 | `INTERNAL_SERVER_ERROR` | generic; none |
| | `data_integrity_error` | 500 | `DATA_INTEGRITY_ERROR` | generic; none |
| | `service_bridge` | (origin's) | (origin's) | unwrapped to the original error (HTTP-021) |
| `AuthError` (`authentication_error`) | `missing_auth_header`, `invalid_auth_header_format`, `invalid_access_token`, `invalid_refresh_token`, `refresh_token_expired`, `user_context_not_found`, `invalid_credentials` | 401 | `AUTHENTICATION_FAILED` | origin message (for example "Missing Authorization header", "Invalid or expired access token", "Invalid email or password"); details if any |
| `AuthError` (`authorization_error`) | `permission_denied` | 403 | `PERMISSION_DENIED` | origin message; details dropped |
| | `workspace_access_revoked`, `email_verification_required`, `privilege_escalation_prevented` | 403 | `PERMISSION_DENIED` | origin message; `details.reason` = `workspace_access_revoked` / `email_not_verified` / `privilege_escalation_prevented` (+ `workspace_id` / `missing_permissions`) |
| `AuthError` (`identity_conflict`) | `identity_already_linked`, `identity_conflict`, `last_identity_protected` | 409 | `RESOURCE_CONFLICT` | origin message; `{reason, method}` |
| `AuthError` (`email_conflict`) | `email_already_in_use` | 409 | `RESOURCE_CONFLICT` | "That email address is already in use"; `{reason:"email_already_in_use"}` |
| `AuthError` (`validation_error`) | `oauth_provider_not_configured` | 400 | `VALIDATION_ERROR` | origin message; `{requested_provider, available_providers}` |
| `OAuthError` (`oauth_error`) | `invalid_token` | 401 | `AUTHENTICATION_FAILED` | "Invalid or expired OAuth token"; details (e.g. `upstream_status`) |
| | `insufficient_scope` | 403 | `PERMISSION_DENIED` | origin message; `{required_scope}` |
| | `email_not_verified` | 403 | `PERMISSION_DENIED` | "Email not verified by OAuth provider"; none |
| | `request_failed`, `revocation_failed`, `parsing_failed` | 502 | `SERVICE_UNAVAILABLE` | "OAuth provider '<provider>' temporarily unavailable"; none |
| `StorageError` (`storage_error`) | `object_not_found` | 404 | `RESOURCE_NOT_FOUND` | "Object not found"; none |
| | `unavailable` | 503 | `SERVICE_UNAVAILABLE` | "Object storage temporarily unavailable"; none |
| | `request_failed` | 500 | `INTERNAL_SERVER_ERROR` | generic; none |

Sources: `src/errors/service_error.rs:19-126`, `src/errors/repository_error.rs:32-64`, `src/errors/auth_error.rs:16-91,365-432,438-622`, `src/errors/oauth_error.rs:16-38`, `src/errors/storage_error.rs:31-51`, `src/errors/api_error.rs:509-654,685-1093`. Each `ServiceErrorCode` is used only with the variant listed (checked across all call sites). `ObservabilityErrorCode` never reaches HTTP.

**Responses outside the envelope:** axum default 404 (unmatched path) and 405 (wrong method), empty body; axum plain-text 413 on telemetry routes; telemetry upstream status and body passthrough; file download bytes; the SSE stream; CORS preflight responses.

## Observations

1. **Verb inconsistency.** Account deletion is `POST /users/me/delete` (`handlers/mod.rs:145`) while workspace, member, invitation, file, session and OAuth-identity deletions use DELETE. Credentials unlink is `POST /auth/identities/credentials/unlink` (`:279-282`), but OAuth unlink is `DELETE /auth/identities/{method}` (`:287-290`). Partial updates use PUT: `PUT /users/me` and `PUT /workspace` leave omitted fields unchanged, and so do `PUT /workspace/invitations/{id}` and `PUT /users/me/password`. No route uses PATCH, and CORS does not allow it. Actions are POST sub-resources (`/switch`, `/transfer`, `/leave`, `/accept`, `/decline`, `/resend`). `POST /auth/validate-session` is a read. This matters because any normalization in the rewrite breaks the desktop client's wire contract.
2. **Status codes.** Every success is 200: register, `POST /users/me/workspaces`, `POST /workspace/invitations` and uploads create resources without returning 201. `success_with_status` (`response_envelope.rs:178-186`) has no caller outside tests. Deletes return `data:{}`, except `DELETE /users/me/sessions/others`, which returns `{revoked_count}` (`sessions.rs:67-69`).
3. **List shapes differ.** Nine lists use the paginated envelope. `GET /auth/identities` returns an unpaginated `{identities:[…]}` object (`identities.rs:31-33,54`). `total_count` appears only on the three in-memory lists (`workspace_repository.rs:724`, `workspace_member_repository.rs:878,1026`) and is absent on the six DB-keyset lists (`pagination.rs:930`).
4. **Path naming.**
   - The current workspace is the singular `/workspace`, selected by a header, while workspace switching is `/users/me/workspaces/{id}/switch`.
   - File routes are spread over `/users/me/files`, `/workspace/files` and `/files/{user|workspace|public/user|public/workspace}/{id}`. The workspace-file download is user-scoped, with no `X-Workspace-Id`, while workspace-file upload, list and delete are workspace-scoped (`handlers/mod.rs:251-255,399-407`).
   - Authenticated routes live under `/auth` (`/auth/identities*`, `/auth/logout`, `/auth/validate-session`, `/auth/verify-email`, `/auth/resend-verification`).
   - `/users/me/email*` and `/users/me/delete` are gate-exempt while every other `/users/me/*` route is gated.
   - Invitations for the user are `/users/me/invitations/pending`, while the workspace's are `/workspace/invitations`.
5. **Rate limiting runs after authentication** (`server.rs:614-621,637-644,660-667`). Requests rejected by auth (401, 403 workspace access, 400 `X-Workspace-Id`) are never counted and carry no rate-limit headers. Repeated invalid-token attempts against protected routes are therefore not throttled by the middleware, and each one costs a DB lookup.
6. **"Headers always present" is not true.** Comments at `server.rs:535`, `app_state.rs:53-55`, `rate_limit.rs:119-123` and `.env.example` say rate-limit headers are on every response. They are absent on exempt paths, health, auth rejections, limiter fail-open, and misconfiguration 500s (HTTP-033).
7. **Handler-level 429 headers are overwritten.** A handler-level 429 (for example the credential-login lockout) sets `X-RateLimit-*` and `Retry-After` for its own bucket (`api_error.rs:1050-1084`). The outer middleware then overwrites the three `X-RateLimit-*` headers with the general bucket's values (`rate_limit.rs:292`). The response ends up with `Remaining` > 0 next to `Retry-After` and a 429 status.
8. **One public bucket.** Every public route shares `ip:<addr>`, including anonymous `<img src>` public-file downloads and `/auth/config`. At 20 per minute, a client that loads a page of public images can exhaust its own login and refresh budget. Behind NAT, every user shares one bucket. With no derivable IP, every client shares `ip:unknown`.
9. **Unvalidated client IP.** The client IP is the first `X-Forwarded-For` entry, unvalidated, with no trusted-proxy list (`utils/request.rs:51-59`). Any string becomes a bucket key. Protection depends entirely on Caddy overwriting the header; a direct deployment lets clients choose their own IP bucket and their audit IP.
10. **CORS allows only `Authorization`, `Content-Type` and `Accept` request headers and exposes no response headers** (`cors.rs:47`). A browser client could not send `X-Workspace-Id`, `Idempotency-Key`, `Content-Disposition` (needed by uploads), `traceparent`/`baggage` or `Last-Event-ID`, nor read `X-RateLimit-*`, `Retry-After` or `X-Idempotency-*`. The Tauri desktop client presumably avoids CORS by using native HTTP; that is not verifiable here.
11. **Default environment is development.** `ENVIRONMENT` defaults to `development` (`config/observability.rs:152-154`), which enables the dev CORS baseline (localhost:1420 and its 127.0.0.1 form) and debug logging unless an operator sets it.
12. **Inconsistent oversize-body outcomes:**
    - JSON → 400 `VALIDATION_ERROR` "Failed to read request body" (`json.rs:147-150`).
    - Uploads and multipart → 413 envelope.
    - Idempotency pre-check → 413 envelope with a different message (`idempotency.rs:433-435`).
    - Telemetry → axum plain-text 413 with no envelope, because the handlers take raw `Bytes` instead of `UploadBody` (`telemetry.rs:77,107,137`).
13. **Telemetry body limit is lower than the global one.** `TELEMETRY_PROXY_MAX_BODY_BYTES` defaults to 1 MiB, below the 2 MiB global limit (`config/server.rs:4`), yet `handlers/mod.rs:307-309`, `server.rs:673-674` and `.env.example` justify it as raising the ceiling for batches over 2 MB.
14. **Body-limit comments are inaccurate.** Comments say `DefaultBodyLimit` rejects bodies "before any middleware processes the request" and "before buffering" (`server.rs:734-736`, `service_error.rs:50-55`, `body.rs:10-15`, `.env.example` "Axum applies this limit before request processing begins"). In code it only bounds extractors, and the idempotency middleware buffers bodies itself.
15. **Keyed oversize bodies arrive empty.** A keyed request whose body exceeds the idempotency ceiling without an honest `Content-Length` is not rejected; the handler receives an empty body (`idempotency.rs:440-459`). The client then sees a misleading error ("Invalid JSON syntax: EOF…") or an empty upload, instead of 413.
16. **The fingerprint ignores query string and headers.** It hashes only the raw body (`idempotency.rs:469`), and the composite key excludes the query string. A same-key retry that changes `?visibility=` on an upload, or a header such as `Content-Type`/`Content-Disposition`, replays the original response instead of failing.
17. **Composite-key slots are not canonicalized.** The key is used exactly as sent: braced, simple, `urn:uuid:` and upper-case spellings of one UUID are different keys (`validation/mod.rs:534-565`). The workspace slot is the raw `X-Workspace-Id` text, and on user-scoped routes outside `/users/me` (`/auth/identities*`, `/auth/logout`, `/files/*`) it is taken from an unvalidated client header (`idempotency.rs:835-843`).
18. **TTL is not enforced on read.** A completed entry older than 24 h keeps replaying, and a stale `processing` entry keeps answering 409, until the hourly sweep runs (`idempotency_repository.rs:144-167`). With `ENABLE_BACKGROUND_CLEANUP=false`, entries never expire and an abandoned key stays 409 forever (`server.rs:786-797`).
19. **Replay metadata is stale.** A replay's `meta.trace_id` and `meta.timestamp` are the original request's, and the replay's own trace id is not exposed anywhere in the response (`idempotency.rs:742-782`).
20. **A sweep can cause a duplicate execution.** The 5-minute stale-lock sweep deletes any `processing` row older than 5 min, including one whose handler is still running (for example a slow upload). A concurrent same-key request can then execute the handler a second time (`0006_background_tasks.sql:177-181`).
21. **Non-UTF-8 responses stick the key at 409.** A non-UTF-8 2xx/4xx response under a key leaves the row `processing`, so the key returns 409 until the sweep (`idempotency_repository.rs:288-290`). No current POST/PUT/DELETE returns binary, so this is latent.
22. **Idempotency key length messages disagree with code.** The message "must be less than 100 characters" accepts exactly 100 (`validation/mod.rs:534-536`), and the doc comment says "max 36 chars for UUID" (`:492`).
23. **`idempotency_service` is optional in type only.** `AppState.idempotency_service` is an `Option` documented as "Optional: Configurable feature" (`app_state.rs:59-62`), but it is always `Some` (`server.rs:471`) and no setting disables it. The fail-open branch for `None` (`idempotency.rs:389-406`) cannot run in production.
24. **Dead code:**
    - `RateLimitRepository::reset` and its Postgres implementation have no caller (`rate_limit_repository.rs:59-61`, `postgresql/rate_limit_repository.rs:120`).
    - `PaginatedApiResponse.error` is never populated (`response_envelope.rs:449-451`).
    - The `/health` exemption in the rate limiter is unreachable, because infra routes carry no limiter (`rate_limit.rs:144`).
    - The non-validating `Json<T>` extractor is used only inside `ValidatedJson`.
25. **Repository errors expose more than other errors.** Repository-origin 404, 401 and 400 errors send origin messages and details to the client, while every other path hides them (HTTP-020):
    - A `NotFound` built from `RowNotFound` keeps the message "database_entity not found: unknown" while `with_operation_context` overwrites `entity_type`/`entity_id` in the details and adds `operation` (`repository_error.rs:365-407,834-837`).
    - An expired access token returns `{session_id, expires_at}`, while an unknown token returns no details (`public_session_repository.rs:120-123`).
26. **Misleading 409 message.** Every `ApiError::Conflict` returns "Resource already exists" (`api_error.rs:1015-1020`), including the idempotency in-flight duplicate, where nothing already exists and the useful hint ("currently being processed, retry shortly") stays in the logs.
27. **Business rules look like validation.** Business-rule rejections share `VALIDATION_ERROR` 400 with malformed input; `business_rule` is visible only in logs, so clients can tell them apart only by message text.
28. **One wire code for two statuses.** `SERVICE_UNAVAILABLE` is used for both 502 (OAuth provider) and 503 (`api_error.rs:856-865`).
29. **Pagination error codes and messages are uninformative.** A disallowed `sort_by` is tagged `invalid_cursor` (`pagination.rs:365-373`). An out-of-range `limit` gives the generic "Validation failed: 1 field(s) invalid" with `{"limit":["Validation failed: range"]}`, because `PaginationParams` has no custom message (`pagination.rs:200`).
30. **Millisecond cursors against microsecond columns.** Cursor timestamps are truncated to milliseconds (`timestamp_millis()`, for example `session_repository.rs:282`), while the keyset predicate compares against microsecond-precision `TIMESTAMPTZ` columns (`keyset.rs:52-62`). When two or more rows fall within the same millisecond, a page boundary can skip rows (DESC `after`: rows between the truncated time and the row's true time are excluded) or repeat them (ASC).
31. **`sort_by` echo does not name the real column.** `sort_by` and the cursor's `s` say `created_at` even where the column actually sorted is `joined_at` (members) or `invited_at` (invitations) (`workspace_member_repository.rs:300,879,1027`).
32. **Pagination flags can be true on an empty page.** `has_previous` is always true on an `after` page and `has_next` is always true on a `before` page, even when that side is empty (`pagination.rs:760,799,906-910`). A client can follow them to an empty page.
33. **Doc examples disagree with code:**
    - `json.rs:64-76` and `path.rs:74-86`/`query.rs:72-84` show `"details": null`; the field is omitted.
    - `json.rs:240-249` shows `ValidatedJson` details as `[{code,message,params}]` with message "Validation failed"; actual details are arrays of strings with a counted message.
    - The `PaginatedApiResponse` docs show `"previous_cursor": null` and a `PaginationMeta` literal missing `sort_by`/`sort_direction`/`total_count` (`response_envelope.rs:423-439,483-490`).
34. **Health and logging comments are wrong.**
    - `health.rs:61-64` says health endpoints are excluded from logging, and `middleware/mod.rs:19-28` says `logging_middleware` uses `HEALTH_PATH_PREFIX`. The logging layer has no path skip, and every readiness 503 during drain is logged at ERROR.
    - `trace.rs:7-9` says health and telemetry "suppress the 2xx completion log"; no 2xx log exists at all (consistent with `LOGGING.md:159-176`).
35. **Readiness checks nothing but the shutdown flag** (`health.rs:74-89`). The Compose healthcheck (`docker-compose.yml:51-57`) and the ingress probe therefore report ready while the DB is down.
36. **The readiness signal is rarely observable.** The flag flips at the moment axum stops accepting connections (`shutdown.rs:162-168`, `server.rs:2162`). With no pre-drain delay, a 503 from `/health/ready` is visible only on connections already open. The HTTP drain is unbounded, and the background waits are sequential at 10 s each (up to 70 s for 7 tasks) against a 45 s `stop_grace_period` (`server.rs:2187-2202`).
37. **A serve error skips the rest of shutdown.** If `axum::serve` returns an error, `run` returns at once, skipping background-task shutdown, pool close and the OTel flush (`server.rs:2166-2174`). Buffered telemetry for that failure can be lost.
38. **Changing a bucket's window is delayed.** `sp_ratelimit_increment` overwrites `window_secs` on every call but tests expiry against the stored value (`0006_background_tasks.sql:330-342`). A new window length takes effect only after the current window ends, while `reset_at` already uses the new value.
39. **Inconsistent privilege model across the three procedures.** `sp_cleanup_expired_idempotency_entries` is `SECURITY DEFINER` with a pinned `search_path`; `sp_cleanup_expired_rate_limits` and `sp_ratelimit_increment` are invoker-rights with no pinned path (`0006:189` vs `0006:279-292,314-344`).
40. **Retry docs overstate the waits.** The background retry docs promise backoff "2s, 4s, 8s" with 3 attempts (`retry.rs:18-21,194-197`). With `max_attempts=3` only two sleeps occur (about 2 s and 4 s).
41. **Stale file references.** `handlers/mod.rs:98,159,311,316,340` say the middleware is applied in `main.rs`; it is in `server.rs`. Comments about `Retry-After` and `X-RateLimit-*` "on all responses" repeat HTTP-033's inaccuracy.
42. **Unused dependency feature.** `tower-http`'s `trace` feature is compiled but unused (`Cargo.toml:12`); there is no `TraceLayer`.
43. **Retry-After can be 0 while blocked.** `Retry-After` and the message's N are truncated toward zero (`rate_limit.rs:242`), so a client can be told "Try again in 0 seconds" while still blocked for less than a second.

## Open questions

1. Should the rewrite keep the current verbs, 200-for-everything status codes, the singular `/workspace` and the scattered file paths (Observations 1–4) for desktop-client compatibility, or is a breaking cleanup acceptable?
2. Should unauthenticated attempts on protected routes be rate limited, for example an IP tier ahead of authentication (Observation 5)?
3. Is the desktop client the only intended consumer, or must browser clients work? If browsers must work, the CORS allow and expose lists need the custom headers (Observation 10).
4. Should the idempotency fingerprint cover the query string and relevant headers, and should keys and the workspace slot be canonicalized (Observations 16–17)?
5. Should idempotency TTL be enforced on read, independent of the sweep? What should happen when background cleanup is disabled (Observation 18)?
6. Should a keyed body that exceeds the ceiling without an honest `Content-Length` be rejected with 413 instead of passed on empty (Observation 15)?
7. Should every oversize-body case produce one enveloped 413 (Observations 12–13)? Is the 1 MiB telemetry default intended?
8. Should readiness check the database, and should shutdown wait between flipping readiness and closing the listener (Observations 35–36)?
9. Should anonymous public-file downloads get their own bucket rather than sharing the auth endpoints' 20/min IP bucket (Observation 8)?
10. Are the repository-origin details on the wire intended (session id on expiry, the user and workspace ids in `InvalidAuthContext`, `operation` and entity fields on 404) (Observation 25)?
11. Is millisecond cursor precision intended? Fixing it changes the cursor format that clients store (Observation 30).
12. Should business-rule rejections and the idempotency in-flight 409 get distinct wire codes or messages (Observations 26–27)?
13. For handler-level 429s, which bucket should `X-RateLimit-*` describe (Observation 7)?
14. Should a replay expose its own trace id alongside `X-Original-Trace-Id` (Observation 19)?
15. Should production be the default environment, so that an unset `ENVIRONMENT` does not enable the dev CORS origins (Observation 11)?
