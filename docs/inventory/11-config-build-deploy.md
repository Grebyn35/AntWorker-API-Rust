# Inventory: Configuration, build, Docker, deploy, CI/CD, test infrastructure (`OPS`)

Reference: `AntWorker-API` @ `0210895` (2026-10-20). This file records what the old API does. It does not prescribe what the new one should do.

## Scope

OPS covers how the API is configured, built, packaged, run and verified:

- **Configuration:** the environment-variable surface, how it is loaded (`.env`, Compose `env_file`, process env), parsed and validated at startup, and the fail-fast and warning behaviour. The *meaning* of each knob belongs to the domain that consumes it (see the catalog's "Consumer" column). OPS owns the parse/validate contract.
- **Startup process:** CLI dispatch, the order of initialisation and how each step fails. Graceful-shutdown internals belong to HTTP/RT. OPS records only the coupling to Docker's `stop_grace_period`.
- **Build:** `Cargo.toml`, `Cargo.lock` (presence only), `build.rs`, `rust-toolchain.toml`, `.cargo/`.
- **Packaging and runtime topology:** `Dockerfile`, `.dockerignore`, `docker-compose.yml`, `Caddyfile`, `caddy/`, `grafana/entrypoint.sh`. The monitoring services are summarised and referred to OBS. Their config files (`prometheus*.yml`, `loki*`, `tempo*`, `otel-collector-config.yml`, `alertmanager-config.yml`, `blackbox-config.yml`, `grafana/provisioning/`) belong to OBS.
- **Deploy and operations scripts:** `scripts/init.sh`, `deploy.sh`, `admin.sh`, `db-bootstrap.sh`, `fleet-rules.sh`.
- **CI/CD:** `.github/workflows/ci.yml`, `cleanup.yml`, `.github/scripts/advisories.sh`, `.github/dependabot.yml`.
- **Test infrastructure:** how unit and live tests run, `tests/common/`, `tests/deploy/run.sh`, `tests/telemetry/run.sh` (as a CI gate only; its assertions are OBS), and what CI provisions versus what needs manual setup.

Boundaries: migration *content*, RLS, the key ring's cryptography and background-task bodies belong to DATA. The `/health/*` endpoints, CORS and client-IP extraction belong to HTTP. The alert rules, the collector pipeline and the log-filter strings belong to OBS. Where an OPS artefact triggers them, they appear under Effects or Cross-domain dependencies.

Files read in full: `src/config/{mod,database,email,environment,observability,server,storage,validation}.rs`, `src/main.rs`, `src/cli.rs`, `src/lib.rs`, `src/migrate.rs`, `src/server.rs:740-1260` and `2117-2260` (startup and shutdown), `src/encryption/key_manager.rs:1-230`, `src/background.rs:143-183`, `src/utils/request.rs:1-75`, `.env.example`, `Cargo.toml`, `build.rs`, `rust-toolchain.toml`, `.cargo/config.toml`, `.cargo/audit.toml`, `Dockerfile`, `.dockerignore`, `.gitignore`, `.gitattributes`, `docker-compose.yml`, `Caddyfile`, `caddy/entrypoint.sh`, `caddy/sites/grafana.caddy.example`, `grafana/entrypoint.sh`, `scripts/*.sh`, `.github/workflows/ci.yml`, `.github/workflows/cleanup.yml`, `.github/scripts/advisories.sh`, `.github/dependabot.yml`, `tests/common/mod.rs`, `tests/common/tls.rs`, `tests/deploy/run.sh`, `tests/telemetry/run.sh`, `README.md`, `documentation/DEPLOYMENT.md`. Partially read, for claims: `documentation/MULTI_INSTANCE.md:60-140`, `documentation/DATABASE.md:1-60,200-256`, `tests/telemetry/battery.py:1-40`, `tests/migrate_command.rs:1-40`, the headers of `tests/lint_*.rs`. Not opened, by rule: `.env` and `alertmanager-secrets/` (the latter holds only a `.gitignore` in the tree listing).

### Reference: `Cargo.toml` (package `app-api` 0.1.0, edition 2021)

There is no `[features]` section and no `[profile.release]`, so release uses Cargo defaults. `[profile.dev] debug = 1` limits debug info to line tables (`Cargo.toml:126-127`). The crate has a lib (`src/lib.rs`) and a bin (`src/main.rs`), found by Cargo's auto-discovery.

| Dependency | Version / features | Purpose (as used in `src/`) |
|---|---|---|
| axum | 0.8, `tokio`, `multipart` | HTTP framework, multipart uploads |
| axum-extra | 0.12, `query` | Query extraction with repeated keys (`extractors/query.rs`) |
| tokio | 1, `full` | Runtime, signals, timers |
| tower | 0.5, `util` | Service utilities in the extractors |
| tower-http | 0.7, `catch-panic`, `cors`, `trace` | Panic catching, CORS, request tracing |
| hyper, http | 1 | HTTP types |
| sqlx | 0.9, `runtime-tokio`, `tls-rustls-ring-native-roots`, `postgres`, `chrono`, `uuid`, `json` | Postgres driver, migrator, TLS with the system trust store. No compile-time query macros are used, so building needs no database. |
| rustls | 0.23, no defaults, `aws_lc_rs`, `std` | Process-default crypto provider (`lib.rs:142-145`) |
| ring | 0.17 | Hashing, HMAC and random primitives (`auth/*`, `app_state.rs`); the test CA |
| base64 | 0.23 | Tokens, ciphertext, pagination cursors |
| hex | 0.4 | Encryption-key decoding from env |
| percent-encoding | 2.3 | RFC 5987 `filename*` in Content-Disposition |
| sha2 | 0.11 | Idempotency request fingerprint |
| argon2 | 0.5, `std` | Password hashing, OS RNG salts |
| zeroize | 1 | Wipes key material |
| serde / serde_json / chrono / uuid | 1 / 1 / 0.4 `serde` / 1 `v4`,`serde` | Serialization, time, ids |
| reqwest | 0.13, `json`, `form` | Outbound HTTP for OAuth and the telemetry proxy. The comment says "for Google OAuth validation" only. |
| clap | 4, `derive` | CLI (`cli.rs`) |
| dotenvy | 0.15 | `.env` loading (server and test harness) |
| tracing, tracing-subscriber | 0.1, 0.3 `env-filter`,`json` | Structured JSON logs |
| opentelemetry, opentelemetry_sdk | 0.33, `trace`,`metrics`,`logs` | OTel API and SDK |
| opentelemetry-otlp | 0.33, `grpc-tonic`,`tls-ring`,`tls-roots`,`trace`,`metrics`,`logs` | OTLP/gRPC exporter, system roots for `https://` |
| tracing-opentelemetry / opentelemetry-appender-tracing | 0.34 / 0.33 | Bridges tracing spans and events to OTel |
| thiserror, async-trait, futures | 2, 0.1, 0.3 | Errors, async traits, streams |
| tokio-stream | 0.1, `sync` | SSE broadcast streams |
| validator | 0.21, `derive` | Request validation |
| url | 2.5 | URL parsing (including `OTEL_ENDPOINT` validation) |
| unicode-normalization | 0.1 | Input normalisation (`utils/deserializers.rs`) |
| rand | 0.10 | Tokens, codes, nonces, retry jitter |
| lettre | 0.11, no defaults, `builder`,`smtp-transport`,`tokio1-rustls-tls`,`pool`,`tracing` | SMTP |
| minijinja | 3 | Email templates (embedded with `include_str!`) |
| object_store | 0.14, `aws` | S3-compatible storage |
| bytes | 1 | Object payloads |
| flate2 | 1 | Inflating gzip bodies in the telemetry proxy |
| *dev:* saphyr | 0.2, no defaults | YAML parsing in tests of the collector and Prometheus configs |
| *dev:* tokio | `test-util` | Paused-clock tests |
| *dev:* syn / proc-macro2 | 3 `full`,`visit` / 1 `span-locations` | The AST lint `tests/lint_tracing_instrument.rs` |

### Reference: Compose services (`docker-compose.yml`)

The project name is `${COMPOSE_PROJECT_NAME:-app}` (`:7`). The "Logs" column marks services that use the `x-logging` anchor (json-file, 10m × 5, `:11-15`).

| Service | Image | Profile | Host ports | Networks | Volumes / mounts | Depends on | Restart | Logs |
|---|---|---|---|---|---|---|---|---|
| api | `${API_IMAGE:-app-api}:${APP_VERSION:-local}` (built from `Dockerfile`) | none | none (`expose ${SERVER_PORT}`) | default, storage | none | db-migrate completed OK | unless-stopped | yes |
| caddy | caddy:2.11.7-alpine | none | 80, 443 (all interfaces) | default, caddy | Caddyfile, entrypoint, `caddy/sites` (ro); caddy_data, caddy_config, caddy_logs | api healthy | unless-stopped | yes |
| postgres | postgres:18.6-alpine | bundled-db | 127.0.0.1:5432 | default | postgres_data → /var/lib/postgresql | none | unless-stopped | yes |
| db-bootstrap | postgres:18.6-alpine | none | none | default | `scripts/db-bootstrap.sh` (ro); tmpfs /var/lib/postgresql | none (polls) | no | default |
| db-migrate | same as api, `command: [migrate]` | none | none | default | none | db-bootstrap completed OK | no | default |
| seaweedfs | chrislusf/seaweedfs:4.48 | object-storage | 127.0.0.1:8333 | storage | seaweedfs_data | none | unless-stopped | yes |
| seaweedfs-init | amazon/aws-cli:2.38.1 | object-storage | none | storage | none | seaweedfs healthy | no | default |
| cadvisor | ghcr.io/google/cadvisor:v0.60.6 (privileged) | observability-backends | none | monitoring | host /, /var/run, /sys, /var/lib/docker, /dev/disk (ro) | none | unless-stopped | yes |
| prometheus | prom/prometheus:v3.15.0 | observability-backends | 127.0.0.1:9090 | monitoring, caddy | configs (ro), `generated/prometheus` (ro), prometheus_data | none | unless-stopped | yes |
| blackbox-exporter | prom/blackbox-exporter:v0.29.0 (uid 65534) | observability-backends | none | caddy | config (ro) | none | unless-stopped | yes |
| alertmanager | prom/alertmanager:v0.34.1 | observability-backends | 127.0.0.1:9093 | monitoring | config, `alertmanager-secrets` (ro), alertmanager_data | none | unless-stopped | yes |
| loki | grafana/loki:3.7.8 | observability-backends | 127.0.0.1:3100 | monitoring | configs, `loki-rules` (ro), loki_data | none | unless-stopped | yes |
| tempo | grafana/tempo:3.1.0 (uid 10001) | observability-backends | 127.0.0.1:3200 | monitoring | configs (ro), tempo_data | tempo-init completed OK | unless-stopped | yes |
| tempo-init | alpine:3.24 (root) | observability-backends | none | monitoring | tempo_data | none | no | default |
| otel-collector | otel/opentelemetry-collector-contrib:0.162.0 | observability | none (expose 4317, 4318, 8888, 9464, 13133) | default, monitoring | config (ro), caddy_logs (ro) | none | unless-stopped | yes |
| grafana | grafana/grafana:13.2.4 | observability-backends | 127.0.0.1:3050→3000 | monitoring, caddy | entrypoint, provisioning dirs (ro), grafana_data | prometheus, loki, tempo (started) | unless-stopped | yes |

### Reference: CI jobs (`.github/workflows/ci.yml`)

| Job | Runs on | Gate | Lines |
|---|---|---|---|
| lint | every trigger | `cargo fmt --check`, `cargo clippy --all-targets --locked -D warnings`, `cargo test --doc --locked` | 46-72 |
| audit | every trigger | `.github/scripts/advisories.sh` (cargo-audit + osv-scanner) | 74-87 |
| consistency | every trigger | Dockerfile builder Rust tag equals the `rust-toolchain.toml` channel | 89-112 |
| shellcheck | every trigger | shellcheck v0.11.0 on every tracked `*.sh` | 114-128 |
| alert-rules | every trigger | promtool rule tests, a down alert per scrape job, Alertmanager routing per alert | 130-192 |
| telemetry-gate | every trigger | `tests/telemetry/run.sh` | 196-204 |
| test | every trigger | `cargo llvm-cov --no-report --locked` against real Postgres and SeaweedFS; uploads a coverage artifact | 206-371 |
| build-verify | every trigger, matrix {full stack, lean} | image build, `docker compose up`, end-to-end assertions | 373-690 |
| deploy-test | every trigger, matrix {rollback, roll, refuse, converge, config} | `tests/deploy/run.sh <group>` | 692-704 |
| image-scan | all except `schedule` | Trivy on a fresh build, fixable HIGH/CRITICAL | 707-737 |
| published-image-scan | `schedule` only, matrix {edge, release} | Trivy plus advisories on the published images | 743-834 |
| publish | push of a `v*.*.*` tag, or push/dispatch on `main`; needs every job above except published-image-scan | multi-arch build and push to GHCR | 839-982 |

## Environment variable catalog

Notation: *bool* means `true`/`false`, case-insensitive. Any other value, including empty, is fatal at startup (OPS-003). *enum* is case-insensitive over a closed set (OPS-004). Numbers use Rust `FromStr` with no trimming, so a set-but-empty or space-padded number is fatal. Optional strings are read with `env::var(..).ok()`, so `KEY=` (empty) counts as **set to `""`**, not unset. "When smtp/s3/sse/otel" means the check runs only when that backend is selected (OPS-006). "Ex." gives the `.env.example` state: a value, `#` (commented out), or `—` (absent).

### A. API binary: `Config::from_env` (`src/config/environment.rs`)

| Variable | Type | Default | Required | Validation (`validate()` unless noted) | Read at | Consumer | Ex. |
|---|---|---|---|---|---|---|---|
| APP_DISPLAY_NAME | str | `App` | no | none | :344 | MAIL | `App` |
| DATABASE_URL | libpq DSN | `""` | **yes** | non-empty (:630); must not contain `:replace-with-a-strong-password@` (:639) | :347 | DATA | # (Compose builds it) |
| DATABASE_MAX_CONNECTIONS | u32 | 10 | no | > 0 (:656) | :348 | DATA | 10 |
| DATABASE_ACQUIRE_TIMEOUT_SECS | u64 | 10 | no | > 0 (:661) | :349 | DATA | 10 |
| GOOGLE_CLIENT_ID | str? | unset | no | ≠ `your-google-client-id.apps.googleusercontent.com` (:1201) | :352 | AUTH | # |
| GITHUB_CLIENT_ID | str? | unset | with secret | both or neither (:1217-1238); ≠ `your-github-client-id` (:1240) | :353 | AUTH | # |
| GITHUB_CLIENT_SECRET | str? | unset | with id | ≠ `your-github-client-secret` (:1250) | :354 | AUTH | # |
| ACCESS_TOKEN_LIFETIME_SECS | i64 | 3600 | no | > 0; < refresh (:668-683) | :356 | AUTH | # |
| REFRESH_TOKEN_LIFETIME_SECS | i64 | 2592000 | no | > 0 (:673) | :360 | AUTH | # |
| REQUIRE_EMAIL_VERIFICATION | bool | false | no | `true` requires `EMAIL_BACKEND=smtp` (:1263) | :364 | AUTH | false |
| VERIFICATION_CODE_EXPIRY_MINUTES | u32 | 15 | no | 1-60 when smtp (:1274-1282) | :368 | AUTH | 15 |
| VERIFICATION_CODE_MAX_ATTEMPTS | u32 | 5 | no | > 0 when smtp (:1283) | :372 | AUTH | 5 |
| PASSWORD_RESET_CODE_EXPIRY_MINUTES | u32 | 15 | no | 1-60 when smtp (:1291-1299) | :376 | AUTH | 15 |
| PASSWORD_RESET_CODE_MAX_ATTEMPTS | u32 | 5 | no | > 0 when smtp (:1302) | :380 | AUTH | 5 |
| EMAIL_CHANGE_CODE_EXPIRY_MINUTES | u32 | 15 | no | 1-60 when smtp (:1313-1321) | :384 | USER | 15 |
| EMAIL_CHANGE_CODE_MAX_ATTEMPTS | u32 | 5 | no | > 0 when smtp (:1322) | :388 | USER | 5 |
| OAUTH_PROVIDER_CONNECT_TIMEOUT_SECS | u64 | 10 | no | 1-30, always (:1170-1177) | :392 | AUTH | 10 |
| OAUTH_PROVIDER_TOTAL_TIMEOUT_SECS | u64 | 30 | no | 5-120 and ≥ connect, always (:1178-1193) | :396 | AUTH | 30 |
| INVITATION_EXPIRY_DAYS | u32 | 7 | no | 1-90 (:1330-1337) | :401 | WS | 7 |
| SERVER_HOST | str | `0.0.0.0` | no | none | :404 | HTTP | — (Compose pins `0.0.0.0`) |
| SERVER_PORT | u16 | 8080 | no | parse only; 0 accepted | :405 | HTTP/OPS | 8080 |
| CORS_ORIGINS | comma list | `[]` | no | split on `,`, trimmed, blanks dropped (:332-340) | :332 | HTTP | 2 origins |
| ENABLE_BACKGROUND_CLEANUP | bool | true | no | none; master switch for every background task | :408 | DATA | true |
| BACKGROUND_TASK_LOCK_DURATION_SECS | i32 | 300 | no | > 0 (:725) | :412 | DATA/RT | # |
| CLEANUP_INTERVAL_HOURS | u64 | 6 | no | > 0 (:686); warns > 168 | :415 | DATA | 6 |
| IDEMPOTENCY_CLEANUP_INTERVAL_HOURS | u64 | 1 | no | > 0 (:693); warns > 24 | :417 | HTTP/DATA | 1 |
| RATE_LIMIT_CLEANUP_INTERVAL_HOURS | u64 | 1 | no | > 0 (:700) | :421 | HTTP/DATA | 1 |
| AUDIT_LOG_RETENTION_DAYS | u64 | 365 | no | > 0 (:707); warns < 30 or > 3650 | :424 | AUDIT | 365 |
| AUDIT_LOG_CLEANUP_INTERVAL_HOURS | u64 | 24 | no | > 0 (:714); warns > 168 | :426 | AUDIT | 24 |
| ENFORCE_RATE_LIMITS | bool | true | no | none | :429 | HTTP | true |
| RATE_LIMIT_PER_USER | u32 | 100 | no | > 0; warns > 1000 | :430 | HTTP | 100 |
| RATE_LIMIT_PER_IP | u32 | 20 | no | > 0; warns > 100 | :431 | HTTP | 20 |
| RATE_LIMIT_TELEMETRY_PROXY_PER_USER | u32 | 600 | no | > 0 | :433 | HTTP/OBS | 600 |
| RATE_LIMIT_WINDOW_SECS | u64 | 60 | no | > 0 | :436 | HTTP | 60 |
| CREDENTIAL_LOGIN_RATE_LIMIT_MAX_ATTEMPTS / _WINDOW_SECS | u32 / u64 | 5 / 900 | no | > 0; max warns > 20 | :438, :442 | AUTH | 5 / 900 |
| VERIFICATION_RESEND_RATE_LIMIT_MAX_ATTEMPTS / _WINDOW_SECS | u32 / u64 | 3 / 300 | no | > 0; max warns > 20 | :446, :450 | AUTH | 3 / 300 |
| PASSWORD_RESET_RATE_LIMIT_MAX_ATTEMPTS / _WINDOW_SECS | u32 / u64 | 3 / 300 | no | > 0; max warns > 20 | :454, :458 | AUTH | 3 / 300 |
| PASSWORD_RESET_VERIFY_RATE_LIMIT_MAX_ATTEMPTS / _WINDOW_SECS | u32 / u64 | 10 / 900 | no | > 0; max warns > 50 | :462, :466 | AUTH | 10 / 900 |
| PASSWORD_CHANGE_RATE_LIMIT_MAX_ATTEMPTS / _WINDOW_SECS | u32 / u64 | 5 / 900 | no | > 0; max warns > 20 | :470, :474 | USER | 5 / 900 |
| EMAIL_CHANGE_RATE_LIMIT_MAX_ATTEMPTS / _WINDOW_SECS | u32 / u64 | 3 / 300 | no | > 0; max warns > 20 | :478, :482 | USER | 3 / 300 |
| SSE_NOTIFICATION_BACKEND | enum `postgres`\|`disabled` | postgres | no | parse (:253-265) | :254 | RT | postgres |
| SSE_KEEPALIVE_INTERVAL_SECS | u64 | 15 | no | > 0 when sse; warns > 60 | :487 | RT | 15 |
| SSE_EVENT_RETENTION_HOURS | u64 | 1 | no | > 0 when sse; warns > 24 | :490 | RT | 1 |
| SSE_EVENT_CLEANUP_INTERVAL_HOURS | f64 | 1.0 | no | finite, > 0, × 3600 ≥ 1 when sse (:923-931); warns > 24 | :492 | RT | 1 |
| SSE_EVENT_REPLAY_LIMIT | u64 | 100 | no | > 0 when sse; warns > 500 | :495 | RT | 100 |
| RE_ENCRYPTION_INTERVAL_HOURS | u64 | 6 | no | > 0 (:942) | :497 | DATA | 6 |
| MAX_REQUEST_BODY_BYTES | usize | 2097152 | no | > 0 (:949) | :501 | HTTP | 2097152 |
| STORAGE_MAX_UPLOAD_BYTES | usize | 10485760 | no | > 0, always (:959) | :505 | FILE/HTTP | 10485760 |
| TELEMETRY_PROXY_COLLECTOR_ENDPOINT | str | `http://otel-collector:4318` | no | not blank; starts with `http://` or `https://`, case-sensitive and untrimmed (:760-783) | :508 | OBS | same |
| TELEMETRY_PROXY_MAX_BODY_BYTES | usize | 1048576 | no | > 0 (:785) | :511 | OBS | 1048576 |
| TELEMETRY_PROXY_USER_BYTES_PER_MINUTE | u64 | 1048576 | no | ≥ max body (:790-798) | :515 | OBS | 1048576 |
| TELEMETRY_PROXY_INSTANCE_BYTES_PER_SECOND | u64 | 16384 | no | > 0 (:799) | :519 | OBS | 16384 |
| TELEMETRY_PROXY_UPSTREAM_TIMEOUT_SECS | u64 | 10 | no | 1-60 (:805-817) | :523 | OBS | 10 |
| EMAIL_BACKEND | enum `disabled`\|`smtp` | disabled | no | parse (:281-293) | :282 | MAIL | disabled |
| SMTP_HOST | str? | unset | when smtp | non-empty; ≠ `smtp.example.com` (:968, :989) | :529 | MAIL | # |
| SMTP_PORT | u16 | 587 | no | > 0 when smtp (:1021) | :530 | MAIL | # |
| SMTP_USERNAME | str? | unset | no | ≠ `your-smtp-username` when smtp (:997) | :531 | MAIL | # |
| SMTP_PASSWORD | str? | unset | no | ≠ `your-smtp-password` when smtp (:1004) | :532 | MAIL | # |
| SMTP_TLS_MODE | enum `starttls`\|`tls`\|`none` | starttls | no | parse (:309-322); `none` warns when smtp | :310 | MAIL | # |
| SMTP_FROM_EMAIL | str? | unset | when smtp | non-empty; ≠ `noreply@yourdomain.com` (:975, :1011); format not checked | :534 | MAIL | # |
| SMTP_FROM_NAME | str? | unset | no | none | :535 | MAIL | # |
| SMTP_CONNECTION_TIMEOUT_SECS | u64 | 10 | no | 1-60 when smtp (:1028-1036) | :537 | MAIL | # |
| STORAGE_BACKEND | enum `disabled`\|`s3` | disabled | no | parse (:295-307) | :296 | FILE | s3 |
| S3_BUCKET | str? | unset | when s3 | non-empty (:1043) | :543 | FILE | app-objects |
| S3_REGION | str? | `us-east-1` | when s3 | non-empty (:1048) | :547 | FILE | us-east-1 |
| S3_ENDPOINT | str? | unset (AWS) | no | an `http://` prefix requires `S3_ALLOW_HTTP` (:1090-1099) | :550 | FILE | `http://seaweedfs:8333` |
| S3_ACCESS_KEY_ID | str? | unset | when s3 | non-empty (:1053) | :551 | FILE | app-storage |
| S3_SECRET_ACCESS_KEY | str? | unset | when s3 | non-empty; ≠ `replace-with-a-strong-password` (:1063, :1076) | :552 | FILE | placeholder |
| S3_ALLOW_HTTP | bool | false | no | `true` warns when s3 | :553 | FILE | true |
| S3_FORCE_PATH_STYLE | bool | true | no | none | :555 | FILE | true |
| S3_CONNECT_TIMEOUT_SECS | u64 | 10 | no | 1-30 when s3 | :559 | FILE | 10 |
| S3_REQUEST_TIMEOUT_SECS | u64 | 30 | no | 5-300 and ≥ connect when s3 | :563 | FILE | 30 |
| S3_RETRY_TIMEOUT_SECS | u64 | 5 | no | 0-180 when s3 | :567 | FILE | 5 |
| STORAGE_DELETION_REAPER_INTERVAL_SECS | u64 | 300 | no | > 0 when s3 | :571 | FILE/DATA | 300 |
| STORAGE_DELETION_BATCH_SIZE | i32 | 100 | no | > 0 when s3 | :575 | FILE/DATA | 100 |
| STORAGE_DELETION_RETRY_BACKOFF_SECS | i32 | 600 | no | ≥ 0 when s3 | :579 | FILE/DATA | 600 |
| STORAGE_DELETION_MAX_ATTEMPTS | i32 | 10 | no | > 0 when s3 | :583 | FILE/DATA | 10 |
| OBSERVABILITY_BACKEND | enum `local`\|`opentelemetry` | local | no | parse (:267-279) | :268 | OBS | opentelemetry |
| OTEL_ENDPOINT | URL? | unset | when otel | present, non-empty, parses as a URL, scheme http/https (:1342-1408) | :589 | OBS | `http://otel-collector:4317` |
| OTEL_SERVICE_NAME | str | `app-api` | no | non-empty when otel | :590 | OBS | app-api |
| OTEL_SERVICE_VERSION | str | `CARGO_PKG_VERSION` | no | non-empty when otel | :592 | OBS | # |
| ENVIRONMENT | str | `development` | no | non-empty when otel (:1427); exactly `development` (case-sensitive) switches dev mode (OPS-012) | :594 | HTTP, OBS | development |
| OTEL_TRACE_RATIO | f64 | 1.0 | no | finite and 0.0-1.0 when otel (:1358-1366) | :597 | OBS | 1.0 |
| OTEL_METRIC_EXPORT_INTERVAL_SECS | u64 | 15 | no | 1-150 when otel (:1370-1382) | :601 | OBS | 15 |

### B. API binary, outside `Config`

| Variable | Type | Default | Required | Validation | Read at | Consumer | Ex. |
|---|---|---|---|---|---|---|---|
| APP_DB_ENCRYPTION_KEY_1 … _255 | 64 hex chars (32 bytes) | none | at least one | hex-decodes to exactly 32 bytes; a failure **panics** at startup (OPS-011) | `src/encryption/key_manager.rs:105-112`, `:141-185` | DATA | `_1` placeholder |
| APP_DB_ACTIVE_KEY_VERSION | u8 | lowest version present | no | integer 1-255, not 0, and must be loaded (`key_manager.rs:188-228`) | `key_manager.rs:115` | DATA | 1 |
| RUST_LOG | EnvFilter directives | unset → environment default | no | read by `EnvFilter::try_from_default_env`; an invalid value falls back to the default silently | `src/server.rs:978-979` | OBS | # |
| DATABASE_URL (`migrate` only) | DSN | none | yes for `migrate` | empty counts as unset; must parse as `PgConnectOptions` | `src/migrate.rs:19`, `:89-95`, `:137` | DATA/OPS | Compose sets it (`docker-compose.yml:174`) |
| SSL_CERT_FILE, SSL_CERT_DIR | path | system store | no | read by `rustls-native-certs` (a dependency), not by repo code | stated in `.env.example:1060-1062`, `otlp_observability_repository.rs:138` | OPS | comment only |
| OTEL_EXPORTER_OTLP_HEADERS | `k=v,…` | unset | no | read by `opentelemetry-otlp` itself | stated in `.env.example:1078-1083` | OBS | # |

### C. Docker Compose interpolation (`docker-compose.yml`)

| Variable | Default in Compose | Required | Used at | Notes | Ex. |
|---|---|---|---|---|---|
| COMPOSE_PROJECT_NAME | `app` | no | :7, :44, :120, :139, :150, :174; `admin.sh:51,88` | Project name, Postgres DB name, DSN path. The naming rules (lowercase etc.) are not checked by repo code. | app |
| COMPOSE_PROFILES | none (read natively by Compose) | no | profiles at :111, :189, :217, :239… | Unset means none of the profiled services run (OPS-023) | all four |
| API_IMAGE | `app-api` | no | :21, :166 | `init.sh` rewrites it to `ghcr.io/<owner>/<repo>/api` | app-api |
| APP_VERSION | `local` | no | :21, :166; `deploy.sh:43-44` | Tag, and deploy mode (OPS-047) | local |
| API_REPLICAS | 1 | no | :29; `deploy.sh:47-54` | `deploy.sh`: positive integer, no leading zero | 1 |
| API_CPU_LIMIT / API_MEMORY_LIMIT | `1` / `1g` | no | :32-33 | Per-replica cgroup caps | 1 / 1g |
| APP_DB_PASSWORD | none | **yes** (`:?`, :151) | :44, :151; `db-bootstrap.sh:20` | app_user password, woven into the default DSN | placeholder |
| POSTGRES_ADMIN_PASSWORD | `postgres-dev-only-change-in-prod` | no | :119, :150, :174; `admin.sh:86` | Read by Postgres on first init only; blanked in api (:47) | dev default |
| MIGRATE_DATABASE_URL | bundled admin DSN | managed DB: yes | :150, :174 | Admin DSN for the one-shots; blanked in api (:48) | # |
| DATABASE_URL | `postgresql://app_user:${APP_DB_PASSWORD}@postgres:5432/${COMPOSE_PROJECT_NAME}` | no | :44 | api runtime DSN | # |
| OTEL_ENDPOINT | `http://otel-collector:4317` | no | :43 | Always injected into api | set |
| SERVER_PORT | 8080 | no | :36, :53, :92 | api expose, healthcheck, Caddy upstream port | 8080 |
| POSTGRES_MEMORY_LIMIT / _CPU_LIMIT / _SHARED_BUFFERS / _EFFECTIVE_CACHE_SIZE / _MAX_CONNECTIONS | 2g / 1 / 512MB / 1536MB / 150 | no | :115-116, :133-137 | Bundled Postgres only | same |
| CADDY_SITE_ADDRESS | `:80` | no | :87; `Caddyfile:58` | Mode switch (OPS-033) | :80 |
| CADDY_ACME_EMAIL | none | **yes** (`:?`, :88) | `Caddyfile:11`; `caddy/entrypoint.sh:114` | Placeholder `dev@localhost.invalid` is refused with a public host (OPS-037) | dev@localhost.invalid |
| CADDY_GRAFANA_SITE_ADDRESS | `""` | no | :91, :471; `grafana.caddy.example:13` | Also derives `GF_SERVER_ROOT_URL` | empty |
| S3_ACCESS_KEY_ID / S3_SECRET_ACCESS_KEY | `app-storage` / none | secret **yes** (`:?`, :198, :223) | :197-198, :222-223 | Become SeaweedFS `AWS_*` credentials | set / placeholder |
| S3_REGION / S3_BUCKET | `us-east-1` / `app-objects` | no | :224-225 | seaweedfs-init | set |
| ENVIRONMENT | `development` | no | :277, :430 | prometheus, otel-collector | development |
| CLIENT_SERVICE_NAME | `app-desktop` | no | :431 | otel-collector only; the API never reads it | app-desktop |
| OTEL_TAIL_SAMPLING_PERCENTAGE / OTEL_TAIL_LATENCY_THRESHOLD_MS | 10 / 500 | no | :432-433 | otel-collector only | 10 / 500 |
| CLIENT_TELEMETRY_LOGS_RATE_MB / _TRACES_RATE_BYTES / _RETENTION | `""` | no | :353-354, :379-380 | loki / tempo; empty means the config file's default | set |
| GRAFANA_ADMIN_PASSWORD | none | **yes** (`:?`, :467) | :467; `admin.sh:103`; `grafana/entrypoint.sh:18` | Blanked in api (:50) | placeholder |

Fixed container env (not configurable): `SERVER_HOST=0.0.0.0` (api, :42), `POSTGRES_USER=postgres`, `POSTGRES_DB=${COMPOSE_PROJECT_NAME}` (:118-120), `WEED_MASTER_VOLUME_GROWTH_COPY_1=1` (:201), and Grafana's `GF_USERS_ALLOW_SIGN_UP=false`, `GF_AUTH_ANONYMOUS_ENABLED=false` and three `GF_ANALYTICS_*=false` (:468-475).

### D. Scripts, tests, CI

| Variable | Read by | Behaviour | Ex. |
|---|---|---|---|
| DEPLOY_SH_SYNCED | `scripts/deploy.sh:208`, `:244` | Internal: set on the re-exec after a pull so the pull happens once | — |
| TEST_DATABASE_URL | `tests/common/mod.rs:42`, `:331` | Admin DSN for live tests; unset means skip; blanked in api (`docker-compose.yml:49`) | active (`:186`) |
| REQUIRE_TEST_DB | `tests/common/mod.rs:51-66` | Non-empty turns a would-be skip into a panic | — (CI sets 1, `ci.yml:214`) |
| TEST_S3_ENDPOINT / _REGION / _BUCKET / _ACCESS_KEY_ID / _SECRET_ACCESS_KEY | `tests/common/mod.rs:99-103`, `:163-169` | All five needed, else S3 tests skip | active (`:1005-1009`, via `${S3_*}`) |
| TEST_S3_ALLOW_HTTP / TEST_S3_FORCE_PATH_STYLE | `tests/common/mod.rs:112-113`, `:140-149` | bool, default true, strict like production | # |
| REQUIRE_TEST_S3 | `tests/common/mod.rs:118-133` | Non-empty turns an S3 skip into a panic | — (CI sets 1, `ci.yml:215`) |
| APP_DB_ENCRYPTION_KEY_1 | set by `tests/common/mod.rs:229` | The harness sets 64 `0`s | — |
| CARGO_TERM_COLOR, RUST_BACKTRACE | `ci.yml:41-43` | `always`, `1` for every job | — |

**Cross-check, code against `.env.example`:**
- Read by the API binary but absent from `.env.example`: `SERVER_HOST`. The omission is deliberate (`.env.example:364-369`). Compose pins it.
- Present in `.env.example` but never read by the API binary: `COMPOSE_PROJECT_NAME`, `COMPOSE_PROFILES`, `API_IMAGE`, `APP_VERSION`, `API_REPLICAS`, `API_MEMORY_LIMIT`, `API_CPU_LIMIT`, `APP_DB_PASSWORD`, `POSTGRES_ADMIN_PASSWORD`, `MIGRATE_DATABASE_URL`, `POSTGRES_{MEMORY_LIMIT,CPU_LIMIT,SHARED_BUFFERS,EFFECTIVE_CACHE_SIZE,MAX_CONNECTIONS}`, `CADDY_SITE_ADDRESS`, `CADDY_ACME_EMAIL`, `CADDY_GRAFANA_SITE_ADDRESS`, `CLIENT_SERVICE_NAME`, `CLIENT_TELEMETRY_{LOGS_RATE_MB,TRACES_RATE_BYTES,RETENTION}`, `OTEL_TAIL_SAMPLING_PERCENTAGE`, `OTEL_TAIL_LATENCY_THRESHOLD_MS`, `GRAFANA_ADMIN_PASSWORD`, `TEST_DATABASE_URL`, `TEST_S3_*` (7). `OTEL_EXPORTER_OTLP_HEADERS` and `RUST_LOG` are read only by dependencies. All of these are consumed by Compose, the scripts, the collector or the tests.
- Read by tests but absent from `.env.example`: `REQUIRE_TEST_DB`, `REQUIRE_TEST_S3`. Read by dependencies, documented only in comments: `SSL_CERT_FILE`, `SSL_CERT_DIR`.

## Behaviors

The entries are grouped: Configuration & startup (001-016), Build (017-020), Image (021-022), Compose (023-032), Caddy & entrypoints (033-038), Scripts (039-056), CI/CD (057-071), Test infrastructure (072-075).

### OPS-001 Configuration comes from the process environment, with an optional `.env` for the server

- **Behavior:** The server reads all configuration from process environment variables. Before reading, it loads a `.env` file (searched from the cwd upward) without overriding variables already set, and ignores a missing file. The `migrate` subcommand does not load `.env`. In Compose, the api gets `.env` through `env_file` plus explicit overrides. The image contains no `.env`.
- **Kind:** `capability`. Operators configure the API through env vars or `.env`.
- **Trigger:** startup of `app-api` (no subcommand).
- **Input & validation:** see the catalog. Load errors are discarded with `.ok()`.
- **Effects:** none beyond process env.
- **Guarantees:** process env wins over `.env` (dotenvy semantics). `.env` is excluded from the build context, so it is never baked into the image (OPS-022).
- **Source:** `src/server.rs:836`; `src/migrate.rs:4-6`, `:137`; `docker-compose.yml:37-50`; `.dockerignore:22-24`.
- **Tests:** none found for the server path. The harness copies the same `.env` loading (`tests/common/mod.rs:305-310`).

### OPS-002 Unparseable values abort startup before logging is up

- **Behavior:** A set variable that does not parse as its type aborts startup. Unset variables take their defaults. The process prints `FATAL: Configuration error: <KEY> has an invalid value '<v>': <parse error>` to stderr and exits with status 1. `main` then also prints `Error: "Configuration error: …"`.
- **Kind:** `guarantee`
- **Trigger:** startup, `Config::from_env`.
- **Input & validation:** numbers via `FromStr` with no trimming. `""` and `" 10"` are errors for numeric keys. `f64` accepts `nan` and `inf`, which `validate()` then rejects (OPS-005).
- **Effects:** stderr only. Tracing is not initialised yet, so nothing reaches Loki.
- **Errors:** exit 1 → any invalid numeric, bool or enum value.
- **Guarantees:** a typo never silently falls back to the default.
- **Source:** `src/config/environment.rs:213-224`, `:325-606`; `src/server.rs:842-845`; `src/main.rs:5-8`; `src/cli.rs:34`.
- **Tests:** `environment.rs` `parse_helper_tests::parse_env_numeric_default_override_and_fail_fast`.

### OPS-003 Boolean variables accept only `true` or `false`

- **Behavior:** `REQUIRE_EMAIL_VERIFICATION`, `ENABLE_BACKGROUND_CLEANUP`, `ENFORCE_RATE_LIMITS`, `S3_ALLOW_HTTP` and `S3_FORCE_PATH_STYLE` accept `true`/`false` in any case. Any other value is fatal, including `1`, `yes`, `on`, empty and whitespace. The error names the key and the value.
- **Kind:** `rule`
- **Trigger:** startup.
- **Input & validation:** lowercased exact match. No trimming.
- **Errors:** exit 1, `<KEY> must be 'true' or 'false' (case-insensitive), got '<v>'`.
- **Guarantees:** an operator cannot silently leave a protection enabled by writing `False`.
- **Source:** `src/config/environment.rs:239-251`.
- **Tests:** `parse_helper_tests::parse_env_bool_*` (4 tests).

### OPS-004 Backend selectors are closed, case-insensitive enums with defaults

- **Behavior:** `SSE_NOTIFICATION_BACKEND` (`postgres`*, `disabled`), `OBSERVABILITY_BACKEND` (`local`*, `opentelemetry`), `EMAIL_BACKEND` (`disabled`*, `smtp`), `STORAGE_BACKEND` (`disabled`*, `s3`) and `SMTP_TLS_MODE` (`starttls`*, `tls`, `none`) are matched case-insensitively. \* marks the default when unset. Unknown or empty values are fatal.
- **Kind:** `rule`
- **Trigger:** startup.
- **Errors:** exit 1, `<KEY> must be 'a' or 'b', got '<v>'`.
- **Guarantees:** the code defaults are the least-dependency options. `.env.example` overrides two of them (`OBSERVABILITY_BACKEND=opentelemetry`, `STORAGE_BACKEND=s3`) to match the bundled Compose stack.
- **Source:** `src/config/environment.rs:253-322`; `.env.example:813`, `:898`, `:1055`.
- **Tests:** `parse_helper_tests::backend_parsers_accept_known_reject_unknown_and_default_when_unset`.

### OPS-005 Unconditional semantic validation aborts startup on the first violation

- **Behavior:** After parsing, `validate()` checks every rule below in fixed order. It stops at the first failure and prints `FATAL: Configuration validation error: <message>` (exit 1). The checks: `DATABASE_URL` non-empty; pool max and acquire timeout > 0; access and refresh lifetimes > 0; cleanup, idempotency, rate-limit and audit intervals > 0; audit retention > 0; lock duration > 0; per-user, per-IP, telemetry and window rate limits > 0; telemetry endpoint non-blank with an `http(s)://` prefix; telemetry max body > 0; instance budget > 0; upstream timeout 1-60; all seven per-flow limit pairs (max and window) > 0; re-encryption interval > 0; `MAX_REQUEST_BODY_BYTES` > 0; `STORAGE_MAX_UPLOAD_BYTES` > 0; OAuth connect timeout 1-30 and total 5-120; `INVITATION_EXPIRY_DAYS` 1-90.
- **Kind:** `guarantee`
- **Trigger:** startup, right after `from_env`.
- **Input & validation:** as listed. The thresholds are in the catalog.
- **Effects:** stderr only.
- **Errors:** exit 1 → the first violated rule. The message names the env var.
- **Guarantees:** only one error is reported per start, so fixing several bad values takes several restarts.
- **Source:** `src/config/environment.rs:622-963`, `:1165-1193`, `:1329-1337`; `src/server.rs:852-855`.
- **Tests:** `environment.rs` `tests::config_rejects_zero_for_every_floor_guarded_field`, `config_rejects_*` (about 60 tests).

### OPS-006 Backend-specific validation runs only for the selected backend

- **Behavior:** Validation for a feature runs only when its backend is selected. Otherwise its variables are not checked, even when invalid or still placeholders. The gates:
  - **smtp:** host and from-address non-empty and not placeholders; username and password not placeholders; port > 0; connect timeout 1-60; verification, password-reset and email-change code expiry 1-60 and max attempts > 0.
  - **s3:** bucket, region, key id and secret non-empty; secret not the placeholder; `http://` endpoint only with `S3_ALLOW_HTTP`; connect 1-30; request 5-300 and ≥ connect; retry 0-180; reaper interval > 0; batch > 0; max attempts > 0; backoff ≥ 0.
  - **sse=postgres:** keepalive, retention and replay > 0; cleanup interval finite and ≥ 1 s.
  - **otel:** endpoint present, non-empty, a valid URL with http(s) scheme; trace ratio finite and in 0-1; metric interval 1-150; service name, service version and `ENVIRONMENT` non-empty.
- **Kind:** `rule`
- **Trigger:** startup.
- **Errors:** exit 1 → first violated gated rule.
- **Guarantees:** a lean deployment (email, storage and OTel disabled) can boot with the `.env.example` placeholders left in place for the disabled features.
- **Source:** `src/config/environment.rs:897-939`, `:965-1163`, `:1269-1327`, `:1339-1433`.
- **Tests:** `config_smtp_placeholder_guards_inactive_when_email_disabled`, `config_storage_placeholder_guards_inactive_when_storage_disabled`, `config_verification_code_guards_inactive_when_email_disabled`, `config_rejects_reaper_knobs_only_when_storage_enabled`, and the S3/OTel/SSE tests in `environment.rs`.

### OPS-007 Cross-field configuration invariants

- **Behavior:** Startup fails when any of these pairs is inconsistent:
  - access lifetime is not strictly less than refresh lifetime;
  - OAuth total timeout < connect timeout;
  - S3 request timeout < connect timeout (s3 only);
  - `TELEMETRY_PROXY_USER_BYTES_PER_MINUTE` < `TELEMETRY_PROXY_MAX_BODY_BYTES`;
  - `REQUIRE_EMAIL_VERIFICATION=true` without `EMAIL_BACKEND=smtp`;
  - exactly one of `GITHUB_CLIENT_ID` / `GITHUB_CLIENT_SECRET` is set.
- **Kind:** `rule`
- **Trigger:** startup.
- **Errors:** exit 1 with a message naming both variables, e.g. `GITHUB_CLIENT_ID is set but GITHUB_CLIENT_SECRET is missing…`.
- **Guarantees:** "set" means the variable is present. Both GitHub vars set to `""` passes (see Observations).
- **Source:** `src/config/environment.rs:678-683`, `:790-798`, `:1121-1125`, `:1186-1193`, `:1217-1238`, `:1263-1267`.
- **Tests:** `config_rejects_access_lifetime_not_less_than_refresh`, `config_rejects_oauth_total_below_connect`, `config_rejects_s3_request_timeout_below_connect`, `config_rejects_telemetry_user_budget_below_max_body`, `config_rejects_require_email_verification_without_smtp`, `config_rejects_github_client_*_without_*`.

### OPS-008 Values still set to their `.env.example` placeholder are refused

- **Behavior:** Startup fails when a value is still its `.env.example` placeholder. Each check and its gate:
  - DSN password, always: `DATABASE_URL` contains `:replace-with-a-strong-password@`.
  - OAuth, always: `GOOGLE_CLIENT_ID`, `GITHUB_CLIENT_ID` or `GITHUB_CLIENT_SECRET` equals its placeholder.
  - SMTP, when smtp: `SMTP_HOST`, `SMTP_USERNAME`, `SMTP_PASSWORD` or `SMTP_FROM_EMAIL` equals its placeholder.
  - S3, when s3: `S3_SECRET_ACCESS_KEY` equals `replace-with-a-strong-password`.
  - Encryption key: `APP_DB_ENCRYPTION_KEY_1=your-encryption-key-here` fails hex decoding and panics (OPS-011).
  
  The messages point to `./scripts/init.sh` and, for the DB password, `./scripts/admin.sh reset-db`.
- **Kind:** `guarantee`
- **Trigger:** startup.
- **Input & validation:** exact string match. The DSN check is anchored on `:` and `@`.
- **Errors:** exit 1 (exit 101 for the key placeholder).
- **Guarantees:** a fresh `cp .env.example .env` without `init.sh` cannot start the full stack. The API never sees `POSTGRES_ADMIN_PASSWORD` (blanked) or `GRAFANA_ADMIN_PASSWORD`; their placeholders are guarded elsewhere (OPS-038, OPS-039).
- **Source:** `src/config/environment.rs:38-49`, `:636-648`, `:986-1018`, `:1074-1086`, `:1195-1259`.
- **Tests:** `config_rejects_placeholder_*`, `config_rejects_database_url_with_placeholder_password`, `config_rejects_s3_placeholder_secret`. The test `placeholder_consts_match_env_example` checks that the literals still occur in `.env.example`.

### OPS-009 Startup configuration report: warnings and feature statuses

- **Behavior:** Once tracing is initialised, each non-fatal validation warning is logged at WARN, followed by one INFO line per feature status and a final `Configuration validation successful` line with `warning_count`. Warning fields: `component=infrastructure`, `subsystem=config_validation`, `category`, `field`, `current_value`. Feature-status lines carry `subsystem=feature_status`, `feature`, `enabled`, `mode` and `details="k=v, …"`, with the message `<name> ENABLED (<mode>)` or `<name> DISABLED`.
  - Warnings fire when: cleanup interval > 168 h; idempotency interval > 24 h; audit cleanup interval > 168 h; retention < 30 d or > 3650 d; per-user limit > 1000; per-IP limit > 100; login, resend, reset, change or email-change max > 20; reset-verify max > 50; SSE keepalive > 60, retention > 24, cleanup > 24 or replay > 500 (sse only); `SMTP_TLS_MODE=none` (smtp only); `S3_ALLOW_HTTP=true` (s3 only).
  - Features reported: `rate_limiting` (mode `enforced` or `observability-only`, backend `postgres`, every limit), `sse_notifications`, `email`, `object_storage`, `email_verification`, `password_reset`, `email_change`, `workspace_invitations`, `opentelemetry` (mode `OTLP/gRPC`, sampling percent).
  - Before tracing starts, the process also prints one `eprintln!` line naming the observability backend, endpoint, service@version, environment and the instance UUID.
- **Kind:** `capability`. Operators can see which features are on and which values are risky.
- **Trigger:** startup, after the tracing subscriber is initialised.
- **Effects:** logs (console JSON; OTLP when enabled).
- **Source:** `src/config/environment.rs:1435-1790`; `src/config/validation.rs:86-388`; `src/server.rs:885-915`, `:1027-1097`.
- **Tests:** `validate_warns_on_*` (7 tests); `validation.rs` `warning_category_display`.

### OPS-010 Startup sequence and how each step fails

- **Behavior:** `app-api` with no subcommand starts in this order; each step's failure mode is in brackets.
  1. Install the crypto provider.
  2. Load `.env`.
  3. `from_env`, then `validate` [exit 1, `FATAL:` line].
  4. Generate the per-process instance UUID v4.
  5. Initialise observability providers [signal failures logged at WARN; startup continues].
  6. Initialise the tracing subscriber and log the report (OPS-009).
  7. Create the Postgres pool [exit 1 with `Error: …`, no `FATAL:` line, after `DATABASE_ACQUIRE_TIMEOUT_SECS`]. Pool settings: max = `DATABASE_MAX_CONNECTIONS`, min = min(max, 2), idle timeout 600 s, max lifetime 1800 s, test before acquire. The pool connects eagerly.
  8. Load the encryption key ring [panic, exit 101].
  9. Metrics service and pool gauge.
  10. Shutdown coordinator.
  11. SSE listener (if postgres).
  12. SMTP service (if smtp) [`?`, exit 1].
  13. S3 repository (if s3) [`?`, exit 1].
  14. `build_app_state` [`?`, exit 1].
  15. Plan and spawn background tasks.
  16. Bind `SERVER_HOST:SERVER_PORT` [`?`, exit 1].
  17. Serve with peer-address `ConnectInfo` and graceful shutdown.
- **Kind:** `guarantee`
- **Trigger:** process start.
- **Effects:** logs `Database connection pool initialized`, `Database encryption initialized…` (active version, key count) and `API server listening` (bind address).
- **Errors:** exit 1 → config, pool, service init or bind failure; exit 101 → key-ring panic.
- **Guarantees:** the API never serves before the database is reachable and the key ring is loaded. In Compose, `restart: unless-stopped` restarts a failed api container (`docker-compose.yml:64`).
- **Source:** `src/cli.rs:31-37`; `src/server.rs:834-1260`, `:1385`, `:1423`, `:1473`, `:2129-2162`; `src/config/database.rs:24-43`.
- **Tests:** `server.rs` `all_tasks_spawn_when_enabled_and_all_backings_present`, `nothing_spawns_when_cleanup_disabled`, `conditional_tasks_*`. No test of the full startup sequence was found.

### OPS-011 Encryption key ring from versioned env vars

- **Behavior:** At startup the server scans `APP_DB_ENCRYPTION_KEY_1` to `_255`. Each present key must hex-decode to exactly 32 bytes, and at least one must exist. `APP_DB_ACTIVE_KEY_VERSION`, when set, must parse as a u8, not be 0, and name a loaded version; when unset, the lowest present version is active. Any failure panics with `Failed to load encryption key ring: …`. The background re-encryption task runs only when more than one version is loaded and `ENABLE_BACKGROUND_CLEANUP=true`.
- **Kind:** `guarantee`
- **Trigger:** startup, after the DB pool (step 8 of OPS-010).
- **Input & validation:** as above. Error details name the env var (`hex`, `length`, `missing`, `parse`, `range`, `not_found`).
- **Effects:** INFO log with `active_key_version` and `key_version_count`. Hex strings and key bytes are zeroized.
- **Errors:** process panic (exit 101) → invalid, missing or mismatched keys.
- **Source:** `src/encryption/key_manager.rs:105-228`; `src/server.rs:1114-1131`, `:786-797`.
- **Tests:** `key_manager.rs` inline tests (`rejects_*`, `default_active_version_is_lowest_present`, `multiple_keys_with_explicit_active`).

### OPS-012 `ENVIRONMENT` switches development mode on an exact match

- **Behavior:** `ENVIRONMENT` defaults to `development`. Only the exact, case-sensitive string `development` enables development mode: the dev CORS baseline and the debug-level console and OTel-logs filters. Every other value, including `Development`, `staging` and typos, gets production behaviour. The value is also the OTel `deployment.environment` attribute, and must be non-empty when OTel export is on. Compose forwards it, defaulting to `development`, to prometheus and otel-collector. `RUST_LOG`, when valid, replaces the console filter.
- **Kind:** `rule`
- **Trigger:** startup.
- **Effects:** console filter `app_api=debug,tower_http=debug,…` in development, else `app_api=info,tower_http=warn,…`. CORS behaviour is HTTP's.
- **Source:** `src/config/observability.rs:152-154`; `src/config/environment.rs:594-595`, `:1427-1432`; `src/server.rs:809-832`, `:966`, `:976-980`; `src/middleware/cors.rs:33-35`; `docker-compose.yml:277`, `:430`.
- **Tests:** `server.rs` `default_filters_route_sdk_diagnostics_to_the_console_only`, `otel_logs_filter_holds_under_a_permissive_rust_log`; `config_rejects_empty_environment_when_opentelemetry`.

### OPS-013 Command-line dispatch

- **Behavior:** The binary `app-api` runs the server when given no subcommand, and the one-shot migration with `migrate`. Like any clap CLI it supports `--help` and `--version` (prints `app-api <CARGO_PKG_VERSION>`). Unknown subcommands, extra arguments and flags are rejected by clap with exit 2. The crypto provider (OPS-015) is installed before parsing.
- **Kind:** `capability`
- **Trigger:** process start.
- **Errors:** exit 2 → clap usage error (`InvalidSubcommand`, `UnknownArgument`).
- **Source:** `src/cli.rs:16-37`; `src/main.rs:5-8`; `Dockerfile:51`; `docker-compose.yml:172`.
- **Tests:** `cli.rs` `definition_is_valid`, `no_argument_runs_the_server`, `migrate_selects_the_migrate_command`, `unknown_arguments_are_rejected`; `tests/migrate_command.rs`.

### OPS-014 `app-api migrate` one-shot contract

- **Behavior:** Reads only `DATABASE_URL` from the process environment (no `.env`, no `Config`; empty counts as unset). It connects within 30 s, takes the sqlx advisory lock, and applies every embedded migration not yet recorded in `_sqlx_migrations`. Versions applied by a newer release are ignored. A modified applied migration is an error naming its version. It logs exactly one JSON line (`EnvFilter app_api=info`, no OTel), either `Database migrations applied` with `applied_count`, `applied_versions` and `embedded_count`, or `Database migrations failed` with `error`. It exits 0 on success and 1 on failure.
- **Kind:** `guarantee`
- **Trigger:** `app-api migrate`. In Compose this is the `db-migrate` one-shot, with `DATABASE_URL=${MIGRATE_DATABASE_URL:-<bundled admin DSN>}`.
- **Input & validation:** DSN must parse as `PgConnectOptions` (libpq params such as `sslmode` and `sslrootcert` are honoured).
- **Effects:** DDL and `_sqlx_migrations` rows (DATA).
- **Errors:** exit 1. Messages: `DATABASE_URL is not set…`, `…is not a valid PostgreSQL connection string`, `could not connect … within 30s`, `could not connect…`, `migration N differs from the copy already applied…`, or a sqlx migrate error.
- **Guarantees:** the one-shot holds no app secrets. The api service starts only after it exits 0 (OPS-024). A failed migration blocks every new replica (OPS-048).
- **Source:** `src/migrate.rs:19-164`; `docker-compose.yml:162-178`.
- **Tests:** `migrate.rs` inline tests (5); `tests/migrate_command.rs` (14, 8 of them DB-backed).

### OPS-015 Process-wide rustls crypto provider

- **Behavior:** At process start, `aws-lc-rs` is installed as the default rustls crypto provider. A second install is ignored. Both `ring` and `aws-lc-rs` are compiled in, so rustls cannot pick one itself; without an installed default, sqlx's `sslmode=verify-ca` path panics. The test harness installs it too.
- **Kind:** `hardening`
- **Trigger:** `cli::run` before argument parsing.
- **Source:** `src/lib.rs:138-145`; `src/cli.rs:32`; `tests/common/mod.rs:354`; `Cargo.toml:19-21`, `:61`.
- **Tests:** indirectly `tests/migrate_command.rs` (the `sslmode` tests against a fake TLS server).

### OPS-016 TLS clients trust the system store, replaceable with `SSL_CERT_FILE`

- **Behavior:** The runtime image installs `ca-certificates`. The Postgres driver (`tls-rustls-ring-native-roots`) and the OTLP exporter for `https://` endpoints (`with_native_roots`) load the system trust store. Per `.env.example`, `SSL_CERT_FILE` (or `SSL_CERT_DIR`) replaces that store for every TLS client, so a private-CA bundle must also include the public roots in use.
- **Kind:** `capability`
- **Trigger:** first TLS connection of each client.
- **Source:** `Dockerfile:43-46`; `Cargo.toml:16-19`, `:59-61`; `src/repositories/otlp_observability_repository.rs:138-148`; `.env.example:1060-1062`.
- **Tests:** `tests/otlp_export_tls.rs` (OTLP only).

### OPS-017 One pinned Rust toolchain for local, CI and the image builder

- **Behavior:** `rust-toolchain.toml` pins channel `1.99.1` with `clippy`, `rustfmt` and `llvm-tools-preview` (profile `minimal`). The Dockerfile builder is `rust:1.99.1-alpine3.24`. CI job `consistency` extracts both versions and fails with a message naming the FROM line to fix. Dependabot bumps the toolchain file weekly but deliberately ignores the `rust` Docker image.
- **Kind:** `guarantee`
- **Trigger:** every CI run; every cargo invocation in the checkout.
- **Errors:** CI failure, `Dockerfile builds with rust X, rust-toolchain.toml pins Y…`.
- **Source:** `rust-toolchain.toml:1-7`; `Dockerfile:8-12`; `.github/workflows/ci.yml:89-112`; `.github/dependabot.yml:36-49`.
- **Tests:** none found (CI configuration).

### OPS-018 Migrations are embedded at build time

- **Behavior:** `build.rs` sets `rerun-if-changed=migrations`. `sqlx::migrate!("./migrations")` embeds the SQL files in the binary. The Dockerfile copies `migrations/` before the final build. An image therefore carries exactly the migrations of the commit it was built from.
- **Kind:** `guarantee`
- **Trigger:** `cargo build`.
- **Source:** `build.rs:1-5`; `src/migrate.rs:82-87`; `Dockerfile:29-34`.
- **Tests:** `migrate.rs` `embedded_migrations_match_the_directory`.

### OPS-019 Static, cross-compiled release binary for amd64 and arm64

- **Behavior:** The builder runs on the build platform, cross-compiling with `tonistiigi/xx:1.10.0`, clang and lld, plus the target's musl-dev and gcc. It first builds dependencies against a stub `main` (cached layer), then the app with `xx-cargo build --release --locked`. `xx-verify --static` fails the build if the binary is not statically linked. The binary is copied to `/usr/local/bin/app-api`. Release settings are Cargo's defaults. CI publishes `linux/amd64` and `linux/arm64`.
- **Kind:** `guarantee`
- **Trigger:** `docker build`; CI `build-verify`, `image-scan`, `publish`.
- **Errors:** build failure → stale `Cargo.lock` (`--locked`) or a non-static binary.
- **Source:** `Dockerfile:1-38`; `.github/workflows/ci.yml:965-982`.
- **Tests:** none found (CI builds it).

### OPS-020 Local Windows build settings

- **Behavior:** `[profile.dev] debug = 1` limits debug info to line tables, to avoid the MSVC LNK1318 PDB limit. `.cargo/config.toml` sets `rust-lld` as the linker for `x86_64-pc-windows-msvc` only.
- **Kind:** `hardening`
- **Trigger:** local `cargo build` on Windows.
- **Source:** `Cargo.toml:126-127`; `.cargo/config.toml:1-2`.
- **Tests:** none found.

### OPS-021 Runtime image: minimal Alpine, non-root, one entrypoint for both roles

- **Behavior:** The runtime is `alpine:3.24`. It runs `apk upgrade`, installs `ca-certificates`, creates system group and user `app`, and runs as `USER app`. It declares `EXPOSE 8080` and `ENTRYPOINT ["app-api"]`. There is no `HEALTHCHECK` (Compose defines one with the image's busybox `wget`) and no `WORKDIR`. The same image serves (no args) and migrates (`migrate`). CI rebuilds the runtime stage uncached (`no-cache-filters: runtime`), so each build picks up current Alpine fixes.
- **Kind:** `guarantee`
- **Trigger:** container start.
- **Source:** `Dockerfile:40-51`; `docker-compose.yml:51-57`; `.github/workflows/ci.yml:403`, `:725`, `:977`.
- **Tests:** none found. `build-verify` boots it (OPS-064).

### OPS-022 Build context excludes secrets and non-build files

- **Behavior:** `.dockerignore` excludes `target/`, IDE files, `.git/`, `.gitignore`, every `**/*.md`, `.env` and `.env.*` (except `.env.example`), `alertmanager-secrets/`, `generated/`, `*.log`, `coverage/`, OS files, `Dockerfile` and `docker-compose*.yml`.
- **Kind:** `guarantee`
- **Trigger:** `docker build`.
- **Guarantees:** runtime secrets and receiver credentials are never sent to the builder or baked into layers.
- **Source:** `.dockerignore:1-39`.
- **Tests:** none found.

### OPS-023 Backing services are optional Compose profiles

- **Behavior:** `COMPOSE_PROFILES` selects the bundled services:
  - `observability`: otel-collector.
  - `observability-backends`: cadvisor, prometheus, blackbox-exporter, alertmanager, loki, tempo, tempo-init, grafana.
  - `object-storage`: seaweedfs, seaweedfs-init.
  - `bundled-db`: postgres.
  
  api, caddy, db-bootstrap and db-migrate have no profile and always run. `.env.example` enables all four profiles. Pairing a dropped profile with the matching backend selector (e.g. `OBSERVABILITY_BACKEND=local`, `STORAGE_BACKEND=disabled`, or managed DSNs) is documented, not enforced.
- **Kind:** `capability`
- **Trigger:** any `docker compose` command.
- **Guarantees:** with no profiles and default DSNs, db-bootstrap polls a non-existent `postgres` host and fails (OPS-040), which blocks the api.
- **Source:** `docker-compose.yml:111`, `:189`, `:217`, `:239`, `:261`, `:298`, `:315`, `:338`, `:364`, `:394`, `:421`, `:448`; `.env.example:31-50`.
- **Tests:** CI `build-verify` matrix (full and lean); `tests/deploy/run.sh` `group_converge` ("managed Postgres (bundled-db dropped)").

### OPS-024 Startup ordering: role bootstrap, then migrations, then api, then ingress

- **Behavior:** db-bootstrap has no dependencies and polls its DSN. db-migrate waits for db-bootstrap to exit 0. api waits for db-migrate to exit 0. caddy waits for api to be healthy. Other orderings: seaweedfs-init waits for seaweedfs to be healthy; tempo waits for tempo-init to exit 0; grafana waits for prometheus, loki and tempo to start. api does not depend on postgres directly, nor on seaweedfs or the collector.
- **Kind:** `guarantee`
- **Trigger:** `docker compose up`.
- **Guarantees:** no replica ever sees a half-migrated schema. On a cold start, caddy (the only ingress) does not start until an api replica passes `/health/ready`.
- **Source:** `docker-compose.yml:58-60`, `:102-104`, `:145-160`, `:175-177`, `:218-220`, `:386-388`, `:476-479`.
- **Tests:** `tests/deploy/run.sh` `group_rollback` (a failing migration means no api starts), `group_roll` (db-migrate exits before the first new replica starts).

### OPS-025 api container contract

- **Behavior:**
  - Image `${API_IMAGE:-app-api}:${APP_VERSION:-local}` with a `build:` fallback.
  - Replicas and limits: `deploy.replicas=${API_REPLICAS:-1}`; per-replica cpus `${API_CPU_LIMIT:-1}`, memory `${API_MEMORY_LIMIT:-1g}` (hard cgroup cap).
  - No host port: `expose ${SERVER_PORT:-8080}` only.
  - Environment: `env_file: .env`, overridden with `SERVER_HOST=0.0.0.0`, a defaulted `OTEL_ENDPOINT`, and a defaulted `DATABASE_URL` built from `APP_DB_PASSWORD`.
  - Healthcheck: `wget -qO- http://127.0.0.1:${SERVER_PORT}/health/ready`, every 10 s, timeout 3 s, 5 retries, start period 30 s.
  - `stop_grace_period: 45s`; `restart: unless-stopped`; networks `default` and `storage`.
- **Kind:** `guarantee`
- **Trigger:** `docker compose up` / `scripts/deploy.sh`.
- **Guarantees:** a replica is only "healthy" when `/health/ready` succeeds (HTTP returns 503 while draining). Docker sends SIGKILL 45 s after SIGTERM; the app's own shutdown waits are not bounded to fit (see Observations).
- **Source:** `docker-compose.yml:18-64`; `src/server.rs:2183-2201`; `src/background.rs:143-183`.
- **Tests:** CI `build-verify`; `tests/deploy/run.sh` `group_roll` ("a request in flight on a draining replica completes", using a stub image).

### OPS-026 Credentials are scoped to the containers that need them

- **Behavior:** The api receives all of `.env`, but `POSTGRES_ADMIN_PASSWORD`, `MIGRATE_DATABASE_URL`, `TEST_DATABASE_URL` and `GRAFANA_ADMIN_PASSWORD` are overridden with empty values. Each other container gets only what it needs:
  - db-bootstrap: `MIGRATE_DATABASE_URL` and `APP_DB_PASSWORD`.
  - db-migrate: only `DATABASE_URL` (the admin DSN).
  - seaweedfs, seaweedfs-init: only the S3 credentials (plus region and bucket for init).
  - grafana: only its admin password.
  - caddy: only its four variables.
- **Kind:** `guarantee`
- **Trigger:** container creation.
- **Guarantees:** the runtime API holds no DB-admin or Grafana credential. It connects as `app_user` (DATA covers RLS).
- **Source:** `docker-compose.yml:37-50`, `:86-92`, `:149-151`, `:173-174`, `:196-198`, `:221-225`, `:466-467`; `src/migrate.rs:4-6`.
- **Tests:** none found. `tests/deploy/run.sh` `group_converge` checks the api's `DATABASE_URL` after a password rotation.

### OPS-027 Compose refuses to run without required secrets

- **Behavior:** `docker compose` aborts with the given message when any of these is unset or empty: `APP_DB_PASSWORD` (`…must be set in .env (run ./scripts/init.sh)`), `CADDY_ACME_EMAIL` (a message explaining that any placeholder works for HTTP-only dev), `S3_SECRET_ACCESS_KEY` or `GRAFANA_ADMIN_PASSWORD`.
- **Kind:** `guarantee`
- **Trigger:** Compose interpolation of `docker-compose.yml`.
- **Errors:** Compose error, non-zero exit.
- **Source:** `docker-compose.yml:88`, `:151`, `:198`, `:223`, `:467`.
- **Tests:** none found.

### OPS-028 Network segmentation and host exposure

- **Behavior:** Containers are split across four networks:
  - `default` (IPv6 enabled): api, caddy, postgres, db-bootstrap, db-migrate, otel-collector.
  - `storage`: api, seaweedfs, seaweedfs-init.
  - `monitoring`: otel-collector, prometheus, alertmanager, loki, tempo, tempo-init, cadvisor, grafana.
  - `caddy` (`internal: true`, no gateway): caddy, prometheus, grafana, blackbox-exporter.
  
  Only caddy publishes on all host interfaces (80, 443). Postgres 5432, SeaweedFS 8333, Prometheus 9090, Alertmanager 9093, Loki 3100, Tempo 3200 and Grafana 3050 are published on `127.0.0.1` only. Inside its container, SeaweedFS binds every port except S3 8333 (and its unbindable 18333 gRPC) to loopback. Alertmanager's cluster listener is disabled.
- **Kind:** `guarantee`
- **Trigger:** `docker compose up`.
- **Guarantees:** the internet-facing api and caddy share no network with alertmanager, loki, tempo or cadvisor. CI's `build-verify` asserts this exact policy (OPS-064).
- **Source:** `docker-compose.yml:63`, `:80-85`, `:105`, `:121-123`, `:194-195`, `:202-203`, `:211`, `:255`, `:266-269`, `:289`, `:309`, `:320-321`, `:330-332`, `:343-344`, `:371-372`, `:442`, `:457-458`, `:481`, `:487-506`.
- **Tests:** `.github/workflows/ci.yml:472-592` (the policy step).

### OPS-029 Bundled PostgreSQL

- **Behavior:** `postgres:18.6-alpine` with superuser `postgres`, password `${POSTGRES_ADMIN_PASSWORD:-postgres-dev-only-change-in-prod}` and database `${COMPOSE_PROJECT_NAME:-app}`. Tuning comes from env: `max_connections` (150), `shared_buffers` (512MB), `effective_cache_size` (1536MB). Limits: memory 2g, cpus 1. The volume `postgres_data` is mounted at `/var/lib/postgresql`, so PG18 keeps its cluster in a per-major subdirectory. Healthcheck: `pg_isready -U postgres -d <db>` every 5 s, 5 retries.
- **Kind:** `capability`
- **Trigger:** the `bundled-db` profile.
- **Guarantees:** the admin password applies only on first cluster init. Rotation is OPS-055.
- **Source:** `docker-compose.yml:108-143`; `.env.example:114-120`, `:216-225`.
- **Tests:** `tests/deploy/run.sh` `group_converge` (setting change, failed start, major upgrade).

### OPS-030 Bundled S3-compatible object store and bucket bootstrap

- **Behavior:** `seaweedfs` (4.48) runs `server -s3` with data in `seaweedfs_data`. Its credentials come from `S3_ACCESS_KEY_ID` and `S3_SECRET_ACCESS_KEY`. `WEED_MASTER_VOLUME_GROWTH_COPY_1=1` lowers the free disk needed for a bucket's first write. Iceberg and Lance listeners and telemetry are off. Healthcheck: `/healthz` every 5 s, 12 retries; memory 512m. `seaweedfs-init` (aws-cli 2.38.1) creates `S3_BUCKET` when `head-bucket` fails, so it is a no-op when the bucket exists.
- **Kind:** `capability`
- **Trigger:** the `object-storage` profile.
- **Source:** `docker-compose.yml:180-232`.
- **Tests:** the CI `test` job reuses this command and environment (OPS-063); `tests/s3_object_storage_live.rs`.

### OPS-031 Container log rotation

- **Behavior:** Every long-running service uses the json-file driver with `max-size 10m`, `max-file 5`. The one-shots (db-bootstrap, db-migrate, seaweedfs-init, tempo-init) use Docker's default. Caddy also writes a lifecycle log file that rolls at 50 MB, keeping 5 (OPS-035).
- **Kind:** `guarantee`
- **Trigger:** container logging.
- **Source:** `docker-compose.yml:9-15` and each service's `logging: *default-logging`; `Caddyfile:15-24`.
- **Tests:** none found.

### OPS-032 Bundled monitoring services (summary; details in OBS)

- **Behavior:** All monitoring images are pinned to exact versions (see the services table).
  - prometheus: TSDB retention 30d or 20GB, whichever comes first; the remote-write receiver and exemplar storage are enabled; it mounts `generated/prometheus` for the fleet rule (OPS-045).
  - alertmanager: credentials from `alertmanager-secrets/` via `*_file` settings.
  - loki and tempo: `-config.expand-env=true` for the client-tenant knobs.
  - tempo-init: chowns `tempo_data` to 10001.
  - otel-collector: receives the API's OTLP/gRPC on 4317 and the proxied client OTLP/HTTP on 4318, and tails Caddy's log.
  - cadvisor: privileged, for container metrics.
  - blackbox-exporter: runs as uid 65534 and probes caddy:2021.
  - grafana: provisioned datasources and dashboards, sign-up, anonymous access and analytics off, `GF_SERVER_ROOT_URL` derived from `CADDY_GRAFANA_SITE_ADDRESS`.
- **Kind:** `capability`
- **Trigger:** the `observability` / `observability-backends` profiles.
- **Source:** `docker-compose.yml:234-482`.
- **Tests:** CI `alert-rules`, `telemetry-gate`, and the `build-verify` observability steps.

### OPS-033 Ingress mode set by `CADDY_SITE_ADDRESS`

- **Behavior:** Caddy (`caddy:2.11.7-alpine`, memory 256m) always binds host 80 and 443. The API site address is `{$CADDY_SITE_ADDRESS::80}`. `:80` (the default) serves plain HTTP. A hostname, or a comma-separated list of hostnames, makes Caddy obtain Let's Encrypt certificates with ACME account email `{$CADDY_ACME_EMAIL}`, serve HTTPS on 443 and redirect HTTP to HTTPS. Certificates and state persist in the `caddy_data` and `caddy_config` volumes. The Caddyfile sets no response headers and no request limits.
- **Kind:** `capability`
- **Trigger:** caddy start.
- **Guarantees:** the API is reachable only through Caddy.
- **Source:** `Caddyfile:1-11`, `:58-60`; `docker-compose.yml:70-106`; `.env.example:447-482`.
- **Tests:** CI `build-verify` (`:80` mode only).

### OPS-034 Upstream discovery and load balancing across replicas

- **Behavior:** Caddy resolves the `api` A records through Docker DNS (`127.0.0.11`) every 5 s, IPv4 only, on port `{$SERVER_PORT:8080}`. It balances round robin with a 500 ms dial timeout and retries failed dials for up to 5 s at 250 ms intervals. It runs no active health checks, so a draining replica that refuses connections is skipped by the retries until the next DNS refresh drops it. The same upstream block serves the public site and the `:2021` probe site.
- **Kind:** `guarantee`
- **Trigger:** each proxied request.
- **Guarantees:** replicas join and leave rotation within one DNS refresh. A replica that is listening but unhealthy still receives traffic, which is why `deploy.sh` removes failed replicas (OPS-049).
- **Source:** `Caddyfile:29-56`.
- **Tests:** CI `build-verify` latency step (at most 2 of 20 requests ≥ 200 ms, `ci.yml:425-438`); `tests/deploy/run.sh` `group_roll` (no failed request during a roll).

### OPS-035 Caddy's internal surfaces: admin API, metrics, probe site, lifecycle log

- **Behavior:**
  - The admin API listens on `127.0.0.1:2019` inside the container only.
  - Metrics are enabled globally and served by the `:2020` site.
  - The `:2021` site proxies only `/health/ready` to the api upstream and answers 404 for everything else.
  - The lifecycle log is JSON at `/var/log/caddy/caddy.log`, mode 0644, rolling at 50MB with 5 kept, and excludes `http.log.access`. It lives on the `caddy_logs` volume, which the otel-collector mounts read-only.
  
  Ports 2020 and 2021 are only `expose`d, so they are reachable on Compose networks but not from the host.
- **Kind:** `guarantee`
- **Trigger:** caddy start.
- **Source:** `Caddyfile:5-27`, `:62-77`; `docker-compose.yml:83-85`, `:101`, `:436-437`.
- **Tests:** CI `build-verify` "Assert the ingress probe succeeds" (`ci.yml:641-655`).

### OPS-036 Optional Caddy sites enabled by renaming a file

- **Behavior:** The Caddyfile imports `/etc/caddy/sites/*.caddy`, a read-only mount of `caddy/sites/`. `*.caddy.example` files are ignored. The shipped template `grafana.caddy.example` proxies `{$CADDY_GRAFANA_SITE_ADDRESS}` to `grafana:3000`. Enabling it means renaming it, setting the variable and redeploying. Any other variable a site needs must also be forwarded in caddy's `environment:`.
- **Kind:** `capability`
- **Trigger:** caddy start or restart.
- **Source:** `Caddyfile:79-81`; `caddy/sites/grafana.caddy.example:1-15`; `docker-compose.yml:89-97`, `:471`.
- **Tests:** `tests/deploy/run.sh` `group_refuse` ("Caddy sites: enabling, renaming to .example and deleting each restart caddy").

### OPS-037 Caddy entrypoint pre-flight guards and `check` mode

- **Behavior:** Before Caddy starts, `caddy/entrypoint.sh` checks two things.
  1. In every enabled site file, each bare `{$VAR}` must be non-empty. Otherwise it prints `ERROR: <site> is enabled but $VAR is empty/unset in .env.` and exits 1.
  2. If `CADDY_ACME_EMAIL` is the placeholder `dev@localhost.invalid` while `CADDY_SITE_ADDRESS`, or the first enabled site with a public address, names a public hostname, it prints an error and exits 1.
  
  An address counts as public unless: it has the `http://` scheme; it is on port 80; the host is empty, `localhost`, `*.localhost`, `*.local`, `*.internal`, `*.home.arpa` or `*.ts.net`; it is an IPv6 or IPv4 literal; it ends with a dot; or it has more than one wildcard label. Hosts are lowercased and other schemes and paths stripped first.
  
  With argument `check`, the script runs these guards and then `caddy validate`, and exits. Otherwise it `exec`s `caddy run`.
- **Kind:** `guarantee`
- **Trigger:** caddy container start; `docker compose run caddy check` (OPS-044).
- **Errors:** exit 1 → missing site variable or placeholder ACME email with a public host; non-zero → `caddy validate` failure.
- **Guarantees:** a misconfigured certificate setup fails loudly at start instead of leaving the site unreachable.
- **Source:** `caddy/entrypoint.sh:1-134`.
- **Tests:** `tests/deploy/run.sh` `group_refuse` ("invalid Caddy site", "public site with the placeholder ACME email").

### OPS-038 Grafana refuses the placeholder admin password

- **Behavior:** `grafana/entrypoint.sh` exits 1 with an error when `GF_SECURITY_ADMIN_PASSWORD` equals `replace-with-a-strong-password`. With `check`, it exits 0 after the guard. Otherwise it `exec`s `/run.sh`.
- **Kind:** `guarantee`
- **Trigger:** grafana container start; `docker compose run grafana check` (OPS-044).
- **Guarantees:** Grafana never creates its admin user (stored permanently in `grafana_data`) with the public placeholder.
- **Source:** `grafana/entrypoint.sh:14-29`; `docker-compose.yml:453-467`.
- **Tests:** `tests/deploy/run.sh` `group_refuse` ("Grafana admin password still the .env.example placeholder").

### OPS-039 `scripts/init.sh` prepares `.env` with strong secrets

- **Behavior:**
  1. If `.env` is missing, copies `.env.example` (fails if neither exists). Requires `openssl`.
  2. Rewrites `.env` line by line, comparing CR-stripped lines. Only exact placeholder lines are replaced: `APP_DB_PASSWORD`, `GRAFANA_ADMIN_PASSWORD`, `S3_SECRET_ACCESS_KEY` and `POSTGRES_ADMIN_PASSWORD` get `openssl rand -hex 24`; `APP_DB_ENCRYPTION_KEY_1` gets `openssl rand -hex 32`.
  3. Rewrites the exact line `API_IMAGE=app-api` to `ghcr.io/<owner>/<repo>/api`, lowercased and derived from `git remote get-url origin` (ssh or https form, must look like `owner/repo`).
  4. Writes through a temp file in the repo root, renamed into place.
  5. Prints the changed keys and next steps, or "nothing to do" when nothing changed.
- **Kind:** `capability`
- **Trigger:** `./scripts/init.sh` (manual; CI `build-verify` and `deploy-test` run it).
- **Errors:** exit 1 → no `.env` or `.env.example`; `openssl` missing.
- **Guarantees:** idempotent. A value someone has already set is never overwritten. An interrupted run never leaves `.env` half-written (`.env.tmp.*` is gitignored). It does not touch SMTP, OAuth or `CADDY_ACME_EMAIL`.
- **Source:** `scripts/init.sh:1-145`; `.gitignore:9-11`.
- **Tests:** exercised by CI `build-verify` "Prepare .env" and `tests/deploy/run.sh:175-176`. No dedicated test.

### OPS-040 `db-bootstrap` creates and passwords the runtime role

- **Behavior:** This one-shot (POSIX sh, postgres image) checks its inputs and exits 1 with `FATAL:` if `MIGRATE_DATABASE_URL` or `APP_DB_PASSWORD` is empty. It then polls `psql "$MIGRATE_DATABASE_URL" -c '\q'` up to 60 times, 2 s apart; on the last failure it re-runs once with output so the cause is shown, and exits 1. In one psql session with `ON_ERROR_STOP` it then:
  1. runs `CREATE ROLE app_user NOLOGIN` if the role is absent;
  2. always runs `ALTER ROLE app_user LOGIN PASSWORD :'app_password'`, using psql's server-side quoting.
  
  It prints `db-bootstrap: app_user role ready…`.
- **Kind:** `guarantee`
- **Trigger:** every `docker compose up` that starts it; every `deploy.sh` (one-shots are recreated, OPS-047).
- **Errors:** exit 1 → missing env, unreachable DSN or SQL error. db-migrate and the api then never start.
- **Guarantees:** idempotent, and works the same on bundled and managed Postgres. Changing `APP_DB_PASSWORD` and redeploying rotates the password before new replicas use it.
- **Source:** `scripts/db-bootstrap.sh:1-65`; `docker-compose.yml:145-160`.
- **Tests:** `tests/deploy/run.sh` `group_converge` ("app_user password rotation", "managed Postgres").

### OPS-041 `deploy.sh` reads `.env` as Compose does and validates `API_REPLICAS`

- **Behavior:** Requires `.env` (else `ERROR: .env not found — run ./scripts/init.sh first.`, exit 1). Reads values through `docker compose config --environment`: the last duplicate wins, quotes are stripped, and the process environment overrides `.env`. `APP_VERSION` defaults to `local` and `API_REPLICAS` to `1`. `API_REPLICAS` must match a positive integer with no leading zero; otherwise exit 1 with `ERROR: API_REPLICAS must be a positive integer with no leading zero (got '<v>').`
- **Kind:** `rule`
- **Trigger:** `./scripts/deploy.sh`.
- **Source:** `scripts/deploy.sh:32-54`.
- **Tests:** `tests/deploy/run.sh` `group_refuse` ("API_REPLICAS that is not a positive integer": `0` and `02`), `group_roll` (duplicate and quoted values).

### OPS-042 One deploy at a time per checkout

- **Behavior:** `deploy.sh` takes a lock directory `.deploy.lock/` with a `pid` file created under `set -C` (O_EXCL). A second deploy prints `ERROR: another deploy (pid N) holds the deploy lock of this checkout.` with guidance, changes nothing, and exits 1. If the holder pid is not alive (checked with `kill -0`, `ps -p` or `/proc`), the lock is taken over: exactly one of several concurrent takers wins through a `takeover` marker file, and it prints `>>> Took over the deploy lock from pid N…`. The pulled `deploy.sh` re-exec keeps the same PID, so it keeps the lock. Every exit path releases the lock (`rm pid; rmdir`), but only when the PID file still names this process.
- **Kind:** `guarantee`
- **Trigger:** start of `deploy.sh`.
- **Errors:** exit 1 → lock held by a live process, or the lock directory cannot be created.
- **Guarantees:** two overlapping deploys never stop each other's replicas. Works without `flock`, on Git Bash, macOS and BusyBox.
- **Source:** `scripts/deploy.sh:56-148`; `.gitignore:13-14`.
- **Tests:** `tests/deploy/run.sh` `group_refuse` (a second deploy, BusyBox `ps`, two started together ×10 rounds, a stale lock).

### OPS-043 `deploy.sh` syncs the checkout according to the deploy mode

- **Behavior:** Skipped when `DEPLOY_SH_SYNCED` is set. Otherwise, by mode:
  - **Release (`APP_VERSION` matches `X.Y.Z`):** HEAD must equal tag `vX.Y.Z`, else exit 1 with `git checkout` instructions. Never pulls.
  - **Not a git work tree:** deploys as is.
  - **Detached HEAD:** `local` deploys as is; any other mode exits 1.
  - **Otherwise:** `git pull --ff-only --no-rebase`. If HEAD moved, it re-execs the pulled `deploy.sh` once with `DEPLOY_SH_SYNCED=1`. If the pull fails, `local` continues (`>>> Could not fast-forward; deploying the checkout as-is…`) and image modes exit 1.
- **Kind:** `rule`
- **Trigger:** `deploy.sh`, after the lock.
- **Guarantees:** a published image is never run against a checkout whose compose and config files may not match it. Local edits are never clobbered (git refuses the fast-forward).
- **Source:** `scripts/deploy.sh:197-256`, `:562`.
- **Tests:** `tests/deploy/run.sh` `group_refuse` (fast-forward conflict, upstream release takeover, release from its tag, detached, no git), `group_roll` (build mode that cannot fast-forward).

### OPS-044 `deploy.sh` refuses invalid configuration before touching the stack

- **Behavior:** After the sync and before any container changes, `deploy.sh` runs four checks. Each failure exits 1 with `… Nothing was changed.`
  1. **Alertmanager secrets** (only when `alertmanager` is among the configured services): every `*_file:` path in `alertmanager-config.yml` must contain a non-whitespace character. This is checked inside a one-off Alertmanager container, as that container's user.
  2. **Caddy config:** `docker compose run --rm --no-deps -T caddy check` (OPS-037).
  3. **Grafana config** (only when `grafana` is configured): `… grafana check` (OPS-038).
  4. **Postgres major version** (only when `postgres` is configured): a one-off postgres container compares `postgres -V` with `$PGDATA/PG_VERSION`. It refuses a different major, or a volume holding another major's cluster when `PGDATA` has none, and points to `documentation/DATABASE.md`.
- **Kind:** `guarantee`
- **Trigger:** `deploy.sh`.
- **Errors:** exit 1 with `ERROR: Alertmanager cannot read a secret file…`, `ERROR: the Caddy configuration is invalid…`, `ERROR: the Grafana configuration is invalid…` or `ERROR: the postgres image cannot run the cluster in its volume…`.
- **Guarantees:** the running ingress and database keep serving when a deploy is refused. This does not cover the checkout itself, which the sync may already have moved (see Observations).
- **Source:** `scripts/deploy.sh:189-195`, `:275-352`, `:562-566`.
- **Tests:** `tests/deploy/run.sh` `group_refuse` (secret missing, unreadable or empty; Caddy typo; placeholder ACME; Grafana placeholder), `group_converge` ("postgres image of a new major version").

### OPS-045 Expected fleet size is published to Prometheus as a generated rule

- **Behavior:** `scripts/fleet-rules.sh N` validates N (a positive integer with no leading zero, else exit 2) and prints a rule group `fleet` with `record: api:expected_instances, expr: vector(N)`. `deploy.sh` writes it to `generated/prometheus/fleet.rules.yml`, mounted read-only into Prometheus, with mode 0644. It replaces the file only when the content changed, so an unchanged deploy does not restart Prometheus. CI's `alert-rules` job generates the same rule for N=2.
- **Kind:** `guarantee`
- **Trigger:** every `deploy.sh`; CI `alert-rules`.
- **Effects:** file write. Prometheus picks it up when restarted by OPS-052. The alert logic is OBS.
- **Source:** `scripts/fleet-rules.sh:1-27`; `scripts/deploy.sh:258-273`, `:567`; `docker-compose.yml:273-274`; `generated/prometheus/.gitignore`; `.github/workflows/ci.yml:139-146`.
- **Tests:** `tests/deploy/run.sh` `group_config` (Prometheus evaluates 2, then 3 after a resize), `group_roll` (`vector(2)`).

### OPS-046 Postgres is converged and healthy before the api roll

- **Behavior:** When postgres is a configured service, `deploy.sh` runs `docker compose up -d --no-deps postgres`, uninterruptibly (OPS-050). It then polls every 2 s until the container is healthy. If the container stops running, its restart count changes, or 180 s pass, it prints the last 20 log lines with `ERROR: postgres is not starting…` and exits 1 without touching the api replicas.
- **Kind:** `guarantee`
- **Trigger:** `deploy.sh`, after the checks.
- **Guarantees:** a `POSTGRES_*` value that Postgres cannot start with is reported before the roll, and correcting `.env` and redeploying repairs it.
- **Source:** `scripts/deploy.sh:354-375`, `:568`.
- **Tests:** `tests/deploy/run.sh` `group_converge` ("a postgres setting it cannot start with", "interrupted while postgres is recreated").

### OPS-047 Deploy modes: build on host or pull a published image

- **Behavior:** `APP_VERSION=local` runs `docker compose build --pull api`. Any other value runs `docker compose pull api`. `edge` follows the moving tag; `X.Y.Z` pins a release. In every mode, `docker compose create --force-recreate db-bootstrap db-migrate` follows, so the one-shots use the refreshed image and the current `.env`. With no api running (cold start), the deploy refreshes images and then converges the whole stack at `API_REPLICAS` (OPS-051). When the running count differs from `API_REPLICAS`, it prints a note and reconciles to `API_REPLICAS`.
- **Kind:** `capability`
- **Trigger:** `deploy.sh`.
- **Source:** `scripts/deploy.sh:382-395`, `:570-584`; `README.md:18-30`.
- **Tests:** `tests/deploy/run.sh` `group_roll` (a moved `edge` pulled by a roll and by a cold start; build mode on a pulled base image).

### OPS-048 Zero-downtime rolling deploy

- **Behavior:** With at least one api running, `deploy.sh` rolls the fleet in these steps.
  1. Force-remove api containers in `created`, `exited` or `dead` state.
  2. Record the running set (OLD) and refresh the images (OPS-047).
  3. Run `docker compose up -d --scale api=<live+target> --no-recreate api`. This first runs db-bootstrap and db-migrate through `depends_on`, so migrations apply while OLD still serves. A failure shows the one-shots' logs and rolls back.
  4. Poll the new containers every 5 s. Proceed when all are `healthy`; roll back if any is `unhealthy` or gone, or after 180 s.
  5. Wait 5 s, one Caddy DNS refresh.
  6. `docker stop` OLD, which honours the 45 s grace, then `docker rm` OLD.
  
  The target is `API_REPLICAS`, so one roll both swaps the image and resizes. Even a single-replica deploy rolls.
- **Kind:** `guarantee`
- **Trigger:** `deploy.sh` with live replicas.
- **Effects:** container create, start, stop and remove; the migrations (DATA); logs `>>> Rolled to N replicas.`
- **Errors:** exit 1 with rollback → start failure, no new replicas, unhealthy replicas or timeout.
- **Guarantees:** no request fails during a roll. Old and new code run against the same schema for a while, so migrations must be backward compatible (`documentation/MULTI_INSTANCE.md:120-127`).
- **Source:** `scripts/deploy.sh:433-504`, `:580-581`.
- **Tests:** `tests/deploy/run.sh` `group_roll` (no failed request, start order after migrations, redeploy of the same version, in-flight request), `group_rollback`.

### OPS-049 A deploy that ends before the drain rolls back

- **Behavior:** From the start of the new replicas until the drain begins, any exit of `deploy.sh` triggers `roll_back`. That covers a failure, Ctrl-C (exit 130), TERM (143), HUP (129) and SIGPIPE (141, a lost SSH session without a pty). `roll_back` ignores further INT, TERM, HUP and PIPE signals, prints `ERROR: <reason> — rolling back.` and the last 30 log lines of a new replica. It then stops and removes every api container not in OLD, in its own process group, and prints `Nothing was deployed; the previous replicas are still serving.` Once the drain has begun, an interrupt leaves the new replicas serving; the next deploy removes the stopped ones (OPS-048 step 1).
- **Kind:** `guarantee`
- **Trigger:** any exit or signal while `ROLLING` is set.
- **Guarantees:** broken replicas never stay in Caddy's rotation. A second Ctrl-C cannot cut the rollback short.
- **Source:** `scripts/deploy.sh:126-147`, `:397-418`, `:450-452`, `:494-498`.
- **Tests:** `tests/deploy/run.sh` `group_rollback` (INT, INT twice, TERM, HUP with and without a pty), `group_roll` (interrupted before the drain, drain interrupted).

### OPS-050 Recreate steps are not interrupted halfway

- **Behavior:** `run_to_completion` runs a step (postgres converge, stack converge, service restart) in its own process group, with output to a temp file and stdin from `/dev/null`. It defers HUP, INT and TERM until the step finishes, then prints the output and, if interrupted, exits 129/130/143 with `ERROR: interrupted; the deploy stopped after this step finished.` `wait_job` polls whether the process is alive instead of using `wait`, which returns at once after SIGPIPE.
- **Kind:** `hardening`
- **Trigger:** the steps named above.
- **Guarantees:** no service is left stopped in the middle of its recreate.
- **Source:** `scripts/deploy.sh:150-187`, `:361`, `:531`, `:601`.
- **Tests:** `tests/deploy/run.sh` `group_converge` ("interrupted while the converge recreates caddy", "…postgres is recreated").

### OPS-051 Stack converge applies changes with the ingress last

- **Behavior:** After the roll or a cold start, three commands run in sequence: `docker compose up -d <every configured service except caddy>`, then `docker compose up -d --wait --no-deps api`, then `docker compose up -d --no-deps caddy`. This recreates only what changed. A failure shows the one-shots' logs and exits 1.
- **Kind:** `guarantee`
- **Trigger:** `deploy.sh`, after the roll or image refresh.
- **Guarantees:** a changed caddy definition is stopped only after the one-shots have run and the api is healthy, so the ingress is never down while migrations run.
- **Source:** `scripts/deploy.sh:586-604`.
- **Tests:** `tests/deploy/run.sh` `group_converge` ("a changed service definition … caddy only once db-migrate is done").

### OPS-052 Services restart when their mounted config files change

- **Behavior:** For each running service in a fixed list, `deploy.sh` restarts it (`docker compose restart`) if any of its bind-mounted paths changed since it started. Change means `find -newer` against a reference file touched to `StartedAt`, ignoring `*.example`. If `touch -d` cannot parse the timestamp, the service counts as changed. The list:
  - caddy: `Caddyfile`, `caddy/entrypoint.sh`, `caddy/sites`
  - prometheus: `prometheus.yml`, `prometheus-alerts.yml`, `generated/prometheus`
  - blackbox-exporter: `blackbox-config.yml`
  - alertmanager: `alertmanager-config.yml`, `alertmanager-secrets`
  - loki: `loki-config.yml`, `loki-overrides.yml`, `loki-rules`
  - tempo: `tempo-config.yml`, `tempo-overrides.yml`
  - otel-collector: `otel-collector-config.yml`
  - grafana: `grafana/entrypoint.sh`, `grafana/provisioning/datasources`, `grafana/provisioning/dashboards/dashboard.yml`
- **Kind:** `guarantee`
- **Trigger:** `deploy.sh`, after the converge.
- **Guarantees:** Compose hashes service definitions, not mounted file contents, so this step is what applies a pulled config change. Enabling, renaming or deleting a site file counts as a change of the directory.
- **Source:** `scripts/deploy.sh:506-544`, `:606`.
- **Tests:** `tests/deploy/run.sh` `group_config` (each service restarts on its own change and only then; every checkout mount is watched), `group_refuse` (site enable, rename and delete; disabled-site edit; BusyBox `touch`).

### OPS-053 A deploy fails if the ingress does not stay up

- **Behavior:** Finally, `deploy.sh` requires a caddy container that is still `running` with an unchanged restart count after 5 s. Otherwise it prints `ERROR: caddy is not staying up (status=…, restarts a→b):` and the last 20 log lines, and exits 1. On success it prints `Deploy complete (APP_VERSION=…)`.
- **Kind:** `guarantee`
- **Trigger:** end of `deploy.sh`.
- **Source:** `scripts/deploy.sh:546-560`, `:607-614`.
- **Tests:** `tests/deploy/run.sh` `group_refuse` ("a site that validates but cannot start", "an ingress that exits a few seconds after each start").

### OPS-054 `admin.sh reset-db` destroys and rebuilds the bundled database

- **Behavior:** Requires `.env`. It refuses with exit 2 when `DATABASE_URL` resolves to a non-empty value (managed Postgres). It asks the operator to type the project name unless `-y`, `--yes` or `--force` is given; any other argument exits 2. It then stops and removes postgres, db-bootstrap and db-migrate, deletes volume `<project>_postgres_data` (skipping a missing one), and runs `docker compose up -d postgres db-bootstrap db-migrate`.
- **Kind:** `capability`
- **Auth:** shell access to the host's Docker.
- **Effects:** all data in the bundled DB is deleted; role and schema are recreated. Observability volumes are untouched.
- **Errors:** exit 1 → confirmation mismatch; exit 2 → managed DB or bad argument.
- **Source:** `scripts/admin.sh:42-82`, `:112`.
- **Tests:** none found.

### OPS-055 `admin.sh rotate-db-password` applies the superuser password from `.env`

- **Behavior:** Reads `POSTGRES_ADMIN_PASSWORD` the way Compose resolves it (exit 1 if empty). It runs `ALTER ROLE postgres PASSWORD :'admin_password'` through `docker compose exec -T postgres psql` on the bundled container, with the statement on stdin so psql's `:'var'` quoting applies.
- **Kind:** `capability`
- **Auth:** host Docker access; bundled postgres running.
- **Source:** `scripts/admin.sh:84-99`, `:113`.
- **Tests:** none found.

### OPS-056 `admin.sh rotate-grafana-password` applies the Grafana admin password from `.env`

- **Behavior:** Reads `GRAFANA_ADMIN_PASSWORD` (exit 1 if empty) and runs `grafana cli admin reset-admin-password <pw>` in the running grafana container. Run with no arguments, `-h`, `--help` or `help`, `admin.sh` prints its usage. An unknown command prints usage to stderr and exits 2.
- **Kind:** `capability`
- **Auth:** host Docker access; grafana running.
- **Source:** `scripts/admin.sh:14-27`, `:101-117`.
- **Tests:** none found.

### OPS-057 CI triggers and concurrency

- **Behavior:** The workflow `CI/CD` runs on: push to `main`; push of tags matching the glob `v*.*.*`; pull requests to `main`; a weekly schedule (Mondays 04:00 UTC); and manual dispatch. The concurrency group is `cicd-<event>-<ref>`, and only pull-request runs cancel a superseded run. Default token permission is `contents: read`.
- **Kind:** `rule`
- **Trigger:** GitHub events.
- **Source:** `.github/workflows/ci.yml:18-43`.
- **Tests:** none found.

### OPS-058 Lint gate: formatting, clippy with warnings as errors, doctests

- **Behavior:** The `lint` job installs the pinned toolchain and uses a rust-cache that saves even on failure. It runs `cargo fmt --all --check`, `cargo clippy --all-targets --locked -- -D warnings` and `cargo test --doc --locked`. Any formatting difference, clippy warning, stale `Cargo.lock` or doctest failure fails the run, and with it `publish`. The crate allows `clippy::result_large_err` globally (`src/lib.rs:96`).
- **Kind:** `guarantee`
- **Trigger:** every CI run.
- **Source:** `.github/workflows/ci.yml:46-72`; `src/lib.rs:85-96`.
- **Tests:** none found (CI configuration).

### OPS-059 Advisory gate: RustSec and GitHub advisories against `Cargo.lock`

- **Behavior:** `.github/scripts/advisories.sh [DIR]` requires `DIR/Cargo.lock` (else exit 1). It runs `cargo +stable audit` under `DIR/.cargo/audit.toml`, whose ignore list is empty; any vulnerability fails, while unmaintained, unsound and yanked are warnings. It then runs `osv-scanner scan source --lockfile Cargo.lock` and fails on any OSV advisory group with no `RUSTSEC-` alias; scanner exit codes above 1 are propagated. The `audit` job runs it on every CI run. `published-image-scan` runs it on each published image's source commit.
- **Kind:** `guarantee`
- **Trigger:** every CI run; the weekly schedule.
- **Errors:** exit 1 → any vulnerability, a GitHub-only advisory, or a missing lockfile.
- **Source:** `.github/scripts/advisories.sh:1-50`; `.cargo/audit.toml:1-16`; `.github/workflows/ci.yml:74-87`, `:828-834`.
- **Tests:** none found.

### OPS-060 Shellcheck gate on every tracked shell script

- **Behavior:** The `shellcheck` job runs `koalaman/shellcheck:v0.11.0` (pinned, bumped by hand) over `git ls-files '*.sh'`, each script against its own shebang. Any finding fails the run.
- **Kind:** `guarantee`
- **Trigger:** every CI run.
- **Source:** `.github/workflows/ci.yml:114-128`; `.github/dependabot.yml:4-6`.
- **Tests:** none found.

### OPS-061 Alert-rule gate

- **Behavior:** The `alert-rules` job uses the Prometheus and Alertmanager images named in `docker-compose.yml`. It runs three checks.
  1. `promtool test rules tests/prometheus/alert-rules.test.yml`, with the fleet rule generated for 2 replicas.
  2. A test generated with `yq` from `prometheus.yml`: every scrape job, with `up=0`, must have a firing critical or warning alert carrying that `job` label within 10 minutes.
  3. Every alert in `prometheus-alerts.yml` and `loki-rules/*.yml` must route, through `amtool config routes test`, to the receiver its severity names: `Watchdog` → `heartbeat`; `AlertmanagerNotificationsFailing` → `warning-alerts,critical-alerts`; critical, warning and info to their receivers. An unknown severity fails.
- **Kind:** `guarantee`
- **Trigger:** every CI run.
- **Source:** `.github/workflows/ci.yml:130-192`.
- **Tests:** the job is itself the test. The rule content belongs to OBS.

### OPS-062 Telemetry ingest-contract gate

- **Behavior:** The `telemetry-gate` job runs `tests/telemetry/run.sh`. It starts Loki, Tempo, Prometheus and the collector on a private Docker network, using the images and config files that `docker-compose.yml` ships. `battery.py` and `telemetrygen` then send forged, malformed, high-cardinality, oversized and flood client batches plus server telemetry. The script fails on any assertion failure, or if the collector restarted or was OOM-killed. Needs Docker, curl and python3.
- **Kind:** `guarantee`
- **Trigger:** every CI run.
- **Source:** `.github/workflows/ci.yml:194-204`; `tests/telemetry/run.sh:1-163`.
- **Tests:** the job is itself the test. The assertions belong to OBS.

### OPS-063 Test-and-coverage job against real Postgres and S3

- **Behavior:**
  1. Frees runner disk and installs the toolchain, rust-cache and `cargo-llvm-cov`.
  2. Reads the postgres, seaweedfs and seaweedfs-init images from `docker-compose.yml` with `yq`.
  3. Starts Postgres with `postgres/postgres`, `fsync`, `synchronous_commit` and `full_page_writes` off, waiting up to 30 s over TCP.
  4. Starts SeaweedFS with the compose `command` and its literal `environment` entries (both must be lists), CI credentials and a 4g tmpfs `/data`, then creates bucket `app-test-objects`.
  5. Runs `timeout --kill-after=60s 35m cargo llvm-cov --no-report --locked` with `TEST_DATABASE_URL`, `REQUIRE_TEST_DB=1`, `REQUIRE_TEST_S3=1` and the `TEST_S3_*` values.
  6. Always writes summary, HTML and lcov reports, unless no profraw exists, and uploads artifact `coverage-report` for 14 days.
  
  Doctests are excluded here (`lint` runs them). No coverage threshold is enforced.
- **Kind:** `guarantee`
- **Trigger:** every CI run.
- **Effects:** artifact `coverage-report`.
- **Errors:** job failure → any test failure, a skipped DB or S3 test (OPS-073, OPS-074), or the 35 m timeout.
- **Guarantees:** live tests cannot pass by silently skipping. A Dependabot bump of a compose image is tested by the suite.
- **Source:** `.github/workflows/ci.yml:206-371`.
- **Tests:** n/a (the job runs the suite: 1047 inline `#[test]`/`#[tokio::test]` functions in `src/`, and 249 in 56 files under `tests/`, with 183 `skip_if_no_db!` call sites and 4 `skip_if_no_s3!` call sites).

### OPS-064 Build-and-boot verification of the real image (full and lean stacks)

- **Behavior:** For each matrix leg ("full stack": all profiles, opentelemetry, s3; "lean": `bundled-db`, local, disabled):
  1. Builds the image with buildx and the GHA cache (scope `api`, runtime stage uncached), loaded as `app-api:local`.
  2. Prepares `.env`: copies `.env.example`, runs `init.sh`, and uses `sed` to set `API_IMAGE`, `COMPOSE_PROFILES`, `OBSERVABILITY_BACKEND` and `STORAGE_BACKEND`.
  3. Runs `docker compose up -d`.
  4. Requests `/health/ready` through Caddy with retries, then 20 timed requests; more than 2 at ≥ 200 ms fails.
  5. Full stack only: waits up to 120 s for Prometheus, Alertmanager, Loki, Tempo, Grafana and the collector's health endpoint.
  6. Checks every container against a hard-coded policy of networks, off-loopback listeners and host ports (only caddy's 80 and 443 may be off loopback), and probes every TCP listener by IP from api, from caddy and from a container on each network. Each listener must be open exactly when policy says so.
  7. Full stack only: Loki loaded every `loki-rules` group; every Prometheus scrape job is `up`, with `instance_up` from each api replica; `probe_success{job="ingress-probe"}` is 1.
  8. No container is restarting, has restarted, or exited non-zero.
  9. Dumps logs on failure; always runs `down -v`.
- **Kind:** `guarantee`
- **Trigger:** every CI run.
- **Source:** `.github/workflows/ci.yml:373-690`.
- **Tests:** the job is itself the test.

### OPS-065 Deploy-script scenario gate (`tests/deploy/run.sh`)

- **Behavior:** The `deploy-test` job runs one matrix job per group: `rollback`, `roll`, `refuse`, `converge` and `config`. For each group the harness:
  - seeds a bare git origin from the checkout's tracked and new, not-ignored files (never `.env`) and works in a clone of it;
  - starts `registry:2` on 127.0.0.1:5000 and pushes busybox-based stub api images that emulate `migrate`, `/health/ready`, drain-on-TERM and a slow endpoint;
  - runs `init.sh`, then sets project `deploytest`, `COMPOSE_PROFILES=bundled-db`, storage disabled, observability local and 2 replicas;
  - drives `scripts/deploy.sh` (300 s timeout per deploy) through each group's scenarios, asserting exact log lines, container sets, Docker events and lock release.
  
  Cleanup always runs `down -v` with every profile, removes the registry and deletes the temp dir. Host requirements: Docker, free ports 80, 443 and 5000, plus the observability loopback ports for `config`. The `converge` group also runs the PostgreSQL major-upgrade block extracted from `documentation/DATABASE.md`.
- **Kind:** `guarantee`
- **Trigger:** every CI run; manually with `bash tests/deploy/run.sh [GROUP…]`.
- **Source:** `.github/workflows/ci.yml:692-704`; `tests/deploy/run.sh:1-1100`.
- **Tests:** the job is itself the test.

### OPS-066 Image CVE gate on a fresh build

- **Behavior:** On every non-schedule run, the `image-scan` job builds the image (reading the GHA cache, writing none; runtime uncached) and runs Trivy with severity `HIGH,CRITICAL`, `ignore-unfixed` and exit code 1. Only fixable findings fail.
- **Kind:** `guarantee`
- **Trigger:** push, PR or dispatch.
- **Source:** `.github/workflows/ci.yml:706-737`.
- **Tests:** none found.

### OPS-067 Weekly re-scan of the published images

- **Behavior:** On the schedule only, matrix `edge` and `release`. The job logs in to GHCR (`packages: read`). `release` resolves to the newest `vX.Y.Z` tag on origin (strict semver; "no release yet" exits 0 with a notice). It pulls `ghcr.io/<repo lowercased>/api:<tag>` and fails if the pull fails. It reads the `org.opencontainers.image.revision` label (must be a 40-hex commit), fetches that commit into a worktree, runs Trivy (fixable HIGH/CRITICAL), and runs `advisories.sh` on that commit's `Cargo.lock`. The two checks run independently.
- **Kind:** `guarantee`
- **Trigger:** weekly schedule.
- **Errors:** job failure → unpullable image, missing or invalid revision label, unfetchable commit, a CVE or an advisory.
- **Source:** `.github/workflows/ci.yml:739-834`.
- **Tests:** none found.

### OPS-068 Publishing to GHCR: gating, version check, tags

- **Behavior:**
  - **Gate:** `publish` needs lint, audit, consistency, shellcheck, alert-rules, telemetry-gate, test, build-verify, deploy-test and image-scan. It runs only for a pushed `refs/tags/v*` tag, or for push/dispatch on `refs/heads/main`.
  - **Permissions and serialisation:** `packages: write`; concurrency group `publish-<ref>`, never cancelled.
  - **Version check (tags only):** `cargo +stable metadata` must report a version equal to the tag without its `v`, else exit 1 with bump instructions.
  - **Build:** QEMU and buildx; builds `linux/amd64,linux/arm64` with `provenance: false`, runtime uncached, cache scope `api-publish` (reading `api` too).
  - **Tags and labels:** `docker/metadata-action` with `latest=false` produces `edge` on main, `{{version}}` on semver tags, and `sha-<full sha>` on tags only, plus OCI labels including `org.opencontainers.image.revision`. The image is pushed to `ghcr.io/<owner>/<repo>/api`.
- **Kind:** `guarantee`
- **Trigger:** green push on `main`, tag push, or dispatch on `main`.
- **Effects:** GHCR image tags; a GHA build cache.
- **Errors:** exit 1 → tag/version mismatch, unreadable `edge` (OPS-069), or a build or push failure.
- **Source:** `.github/workflows/ci.yml:836-982`.
- **Tests:** none found.

### OPS-069 `edge` never moves backwards

- **Behavior:** For a main run (not a tag), the job fetches `refs/heads/main`. If main's head is this commit, it publishes. If main no longer contains this commit, it skips with a notice. Otherwise it reads the `edge` image's revision label (or labels, one per platform, which must agree): no `edge` yet means publish; an unreadable `edge` fails the job; it publishes only if the `edge` commit is an ancestor of, or equal to, this commit, and otherwise skips. Publishes are serialised by the concurrency group.
- **Kind:** `guarantee`
- **Trigger:** `publish` on main.
- **Guarantees:** a late-finishing run never replaces a newer `edge`. A newer head with no run (`[skip ci]`) does not block an older descendant.
- **Source:** `.github/workflows/ci.yml:887-943`.
- **Tests:** none found.

### OPS-070 Weekly pruning of untagged GHCR images

- **Behavior:** `cleanup.yml` runs Sundays at 03:00 UTC and on dispatch, with `packages: write`. It runs `dataaxiom/ghcr-cleanup-action` on package `<repo lowercased>/api` with `keep-n-untagged: 3`. Only untagged versions are pruned, and multi-arch children of tagged indexes are kept.
- **Kind:** `guarantee`
- **Trigger:** weekly schedule; dispatch.
- **Guarantees:** every tagged image (releases, `sha-*`, `edge`) survives.
- **Source:** `.github/workflows/cleanup.yml:1-32`.
- **Tests:** none found.

### OPS-071 Weekly dependency update PRs

- **Behavior:** Dependabot runs weekly for five ecosystems:
  - `github-actions`: one grouped PR for minor and patch updates.
  - `cargo`: all dependency types including transitive; the OpenTelemetry crates are grouped together; other minor and patch updates are grouped; majors come as individual PRs; up to 20 open PRs.
  - `rust-toolchain`: bumps the channel.
  - `docker` (Dockerfile): `rust` ignored; minor and patch grouped.
  - `docker-compose`: minor and patch grouped.
- **Kind:** `guarantee`
- **Trigger:** weekly schedule.
- **Source:** `.github/dependabot.yml:1-62`.
- **Tests:** none found.

### OPS-072 Unit tests need no infrastructure

- **Behavior:** `cargo test --lib` runs the inline `#[cfg(test)]` modules without a database, object store or network. Config tests build `Config` values directly rather than calling `from_env`. The env-touching parse tests use unique keys, or a single test owns each fixed key.
- **Kind:** `capability`
- **Trigger:** developer or CI.
- **Source:** `README.md:62-66`; `src/config/environment.rs:3405-3418`.
- **Tests:** n/a.

### OPS-073 Live-test database harness

- **Behavior:**
  - **Setup:** `TestDb::setup()` loads `.env` once and reads `TEST_DATABASE_URL`. Unset returns `None`, and `skip_if_no_db!` prints a skip and returns, unless `REQUIRE_TEST_DB` is non-empty, in which case it panics. A set but malformed or unreachable URL panics.
  - **Per-binary template:** each test binary creates `app_user` (NOLOGIN, tolerating concurrent creation), grants it to the admin, and builds a migrated template database `apptest_<epoch>_tmpl_<uuid>` once, by executing each `migrations/*.sql` in order with `raw_sql`.
  - **Per test:** each test gets `apptest_<epoch>_<uuid>`, cloned from the template (`setup_empty()` creates an unmigrated one). Every connection carries `statement_timeout=60s` and `lock_timeout=10s`. Admin connects time out after 10 s. `app_user_pool()` runs `SET ROLE app_user` for RLS.
  - **Teardown:** `Drop` terminates the scratch database's backends and drops it on a separate thread. Each setup also drops `apptest_*` databases older than 30 minutes, so orphans and templates are reclaimed later.
  - **Encryption:** `test_encryption()` sets `APP_DB_ENCRYPTION_KEY_1` to 64 zeros.
- **Kind:** `capability`
- **Trigger:** integration tests calling `skip_if_no_db!`.
- **Guarantees:** the application's own database is never touched. The role needs CREATEDB, CREATEROLE and the ability to create `uuid-ossp`.
- **Source:** `tests/common/mod.rs:1-613`; `.env.example:170-186`.
- **Tests:** n/a (it is the harness).

### OPS-074 Live-test object-storage harness

- **Behavior:** `s3_test_repo()` loads `.env` and needs all five `TEST_S3_{ENDPOINT,REGION,BUCKET,ACCESS_KEY_ID,SECRET_ACCESS_KEY}`. With any missing, `skip_if_no_s3!` skips, or panics when `REQUIRE_TEST_S3` is non-empty. `TEST_S3_ALLOW_HTTP` and `TEST_S3_FORCE_PATH_STYLE` default to true and are strict bools (a bad value panics). It builds the production `S3ObjectStorageRepository` with fixed timeouts and reaper values. Tests write and delete under `s3-live-test/<uuid>`.
- **Kind:** `capability`
- **Trigger:** `tests/s3_object_storage_live.rs`.
- **Source:** `tests/common/mod.rs:95-219`; `.env.example:995-1016`.
- **Tests:** n/a.

### OPS-075 In-process TLS test certificate authority

- **Behavior:** `tests/common/tls.rs` builds an ECDSA P-256 CA and leaf certificates as raw DER with `ring`, with no certificate crate. The validity runs from 2000 to 9999-12-31, and each leaf has a DNS SAN. It provides a rustls `ServerConfig` (aws-lc-rs provider) and the CA in PEM, the format of `sslrootcert` and `SSL_CERT_FILE`. Used by the `migrate` sslmode tests and the OTLP TLS tests.
- **Kind:** `capability`
- **Trigger:** `tests/migrate_command.rs`, `tests/otlp_export_tls.rs`.
- **Source:** `tests/common/tls.rs:1-157`.
- **Tests:** n/a.

## Data owned

OPS owns no database tables. It owns these persistent or generated artefacts:

- **Docker named volumes** (prefixed `<COMPOSE_PROJECT_NAME>_`): `postgres_data`, `seaweedfs_data`, `prometheus_data`, `alertmanager_data`, `grafana_data`, `loki_data`, `tempo_data`, `caddy_data` (ACME account and certificates), `caddy_config`, `caddy_logs` (`docker-compose.yml:508-518`). `docker compose down -v` deletes all ten. `admin.sh reset-db` deletes only `postgres_data`.
- **Host files:** `.env` (gitignored, created by `init.sh`); `.env.tmp.*` (transient); `.deploy.lock/pid` and `.deploy.lock/takeover` (transient, gitignored); `generated/prometheus/fleet.rules.yml` (generated, gitignored); `tests/prometheus/fleet.rules.yml` (generated for CI or local promtool, gitignored); `alertmanager-secrets/*` (operator-supplied, gitignored).
- **Registry and CI:** GHCR package `ghcr.io/<owner>/<repo>/api` with tags `edge`, `X.Y.Z` and `sha-<40-hex>` and OCI labels, private by default; the GHA build caches `api` and `api-publish`; the CI artifact `coverage-report` (14 days).
- **Test databases:** `apptest_<epoch>_<uuid>` and `apptest_<epoch>_tmpl_<uuid>` on the `TEST_DATABASE_URL` server, transient and swept after 30 minutes.
- **Role bootstrap:** the `app_user` role's existence and password are set by `db-bootstrap` (OPS-040). Its grants and policies belong to DATA.

## Configuration

The full catalog is above. The variables OPS itself reads (as opposed to the API's domains) are the Compose interpolation variables in table C, the script variables `APP_VERSION`, `API_REPLICAS`, `COMPOSE_PROJECT_NAME`, `DATABASE_URL`, `POSTGRES_ADMIN_PASSWORD`, `GRAFANA_ADMIN_PASSWORD` and `DEPLOY_SH_SYNCED`, the test variables in table D, and, for the API process, `SERVER_HOST` and `SERVER_PORT` (bind address, healthcheck and Caddy upstream port).

## Cross-domain dependencies

- **Into DATA:** `app-api migrate` runs the embedded migrations (OPS-014). db-bootstrap creates `app_user` (OPS-040). The key ring is loaded from env (OPS-011). Every background task is gated by `ENABLE_BACKGROUND_CLEANUP` and its interval knobs (`src/server.rs:786-797`). The test harness applies `migrations/` directly (OPS-073).
- **Into HTTP:** the compose healthcheck, Caddy probe and deploy health wait all depend on `GET /health/ready` returning 200 when ready and 503 while draining. CORS reads `ENVIRONMENT` and `CORS_ORIGINS`. Client-IP extraction (`src/utils/request.rs:49-75`) takes the first `X-Forwarded-For` entry, which Caddy sets (OPS-034). Body limits come from config.
- **Into OBS:** the observability backend, `OTEL_*`, the console and OTel log filters (`src/server.rs:799-832`), and the monitoring services and their configs (OPS-032). The `alert-rules` and `telemetry-gate` CI jobs test OBS content. `deploy.sh` feeds `api:expected_instances` (OPS-045).
- **Into RT:** the SSE backend and knobs. Each instance holds one dedicated LISTEN connection beyond its pool, which the Postgres `max_connections` sizing in `.env.example:188-209` and `docker-compose.yml:131-133` accounts for.
- **Into FILE / MAIL / AUTH / USER / WS / AUDIT:** their configuration surfaces (catalog "Consumer" column). SeaweedFS is the bundled S3 endpoint (OPS-030).
- **From every domain into OPS:** startup fail-fast (OPS-002 to OPS-011), and CI gates on all code (OPS-058, OPS-063).

## Observations

1. **Empty strings count as configured.** `env::var(..).ok()` turns `KEY=` into `Some("")`. `GOOGLE_CLIENT_ID=` passes validation (only the placeholder is rejected) and registers a Google provider with an empty client id (`src/config/environment.rs:352`, `src/auth/provider_factory.rs:58`). Setting both GitHub variables to `""` passes the pairing check (`environment.rs:1217-1238`). `S3_ENDPOINT=` passes the `http://` check (`:1090`). `SMTP_HOST`, `S3_*` and `OTEL_ENDPOINT` reject empty values explicitly; these do not. This matters because Compose `env_file` passes `KEY=` lines through as empty values.
2. **No trimming anywhere.** Numbers, bools and enum selectors are parsed untrimmed (`environment.rs:218-322`). `ENVIRONMENT` is compared case-sensitively with `"development"` (`src/server.rs:810`, `src/middleware/cors.rs:35`). A trailing space or `Development` silently selects production mode.
3. **Fail-open default for `ENVIRONMENT`.** The code default and the `.env.example` value are both `development` (`src/config/observability.rs:152-154`, `.env.example:1330`). This mode enables the dev CORS baseline and debug logging. A production deploy must change it explicitly (`README.md:36-40`, `documentation/DEPLOYMENT.md:16-25`).
4. **Startup failures report in three different ways.** Config errors print `FATAL: …` and then `main` prints `Error: "…"`, so the message appears twice (`src/server.rs:842-855`, `src/main.rs:6`). Pool, SMTP, S3, app-state and bind errors print only `Error: …` with exit 1 (`src/server.rs:1100`, `:1385`, `:1423`, `:1473`, `:2130`). Key-ring errors panic through `expect`, with exit 101 and a Rust panic message (`src/server.rs:1114-1118`). The same failure class (bad config) therefore yields different exit codes depending on which variable is wrong.
5. **An invalid `RUST_LOG` is ignored silently.** `EnvFilter::try_from_default_env().unwrap_or_else(default)` falls back without a warning (`src/server.rs:978-979`).
6. **Unchecked interval arithmetic.** Background intervals are computed as `hours * 3600` on `u64` without overflow checks (`src/server.rs:1520`, `:1575`, `:1656`, `:1712`, `:1847`). Absurdly large values wrap in release builds instead of being rejected. `SERVER_PORT=0` is accepted and binds an ephemeral port (`environment.rs:405`). The Dockerfile hard-codes `EXPOSE 8080` while Compose uses `SERVER_PORT` (`Dockerfile:50`, `docker-compose.yml:36`).
7. **Shutdown does not fit `stop_grace_period`.** Docker sends SIGKILL 45 s after SIGTERM (`docker-compose.yml:62`). The app drains HTTP with no bound, then waits up to 10 s for each of 7 background tasks *sequentially*, up to 70 s, before closing the pool and flushing OTel (`src/server.rs:2183-2201`, `src/background.rs:147-175`). The comment at `src/server.rs:2186-2189` acknowledges that Docker's SIGKILL is the real bound, so the final OTel flush can be lost.
8. **The placeholder literals are duplicated in five places**: the Rust consts (`environment.rs:38-49`), `init.sh:51-55`, `grafana/entrypoint.sh:16`, `caddy/entrypoint.sh:22` and the `deploy-test` harness (which reads them from `.env.example`). Only the test `placeholder_consts_match_env_example` (`environment.rs:3375-3402`) ties any of them, the Rust consts, to `.env.example`. The other copies have no lockstep check.
9. **Some rules exist only as tests.** These CI-enforced properties are defined only by tests: the placeholder lockstep (item 8); the "every `#[tracing::instrument]` uses `skip_all` and names no PII field" lint (`tests/lint_tracing_instrument.rs`); the "every SECURITY DEFINER SP is classified for scoping coverage" lint (`tests/lint_definer_sp_scoping_coverage.rs`); and the collector-config ↔ route-constant sync checks (`src/middleware/mod.rs:79-404`, which `include_str!` `otel-collector-config.yml`, `prometheus.yml` and `documentation/TELEMETRY.md`). They are not cataloged as behaviors.
10. **The live tests migrate differently from production.** The harness executes each `migrations/*.sql` with `raw_sql`, without `_sqlx_migrations` (`tests/common/mod.rs:497-515`). Production uses the sqlx migrator with checksums and an advisory lock (`src/migrate.rs:83-117`). Only `tests/migrate_command.rs` exercises the real migrator.
11. **CI never deploys the real image with `deploy.sh`.** `deploy-test` uses busybox stub images (`tests/deploy/run.sh:133-165`). `build-verify` boots the real image with a plain `docker compose up -d`, not `deploy.sh` (`.github/workflows/ci.yml:420-421`). The real API's drain (503 from `/health/ready`) under a rolling deploy is not exercised.
12. **The published image is not the tested image.** The image is built separately in `build-verify` (amd64, `:396-405`), `image-scan` (`:718-726`) and `publish` (multi-arch, `:965-982`). Each rebuilds the runtime stage uncached with `apk upgrade`, so the pushed image's Alpine packages can differ from what Trivy scanned. The arm64 image is never booted in CI (`README.md:28` and `DEPLOYMENT.md:35-36` describe both architectures as published).
13. **CI relies on tools from the runner image.** `cargo +stable` is used by `advisories.sh:28` and the tag check (`ci.yml:869`), although only the pinned toolchain is installed (`rustup toolchain install` reads `rust-toolchain.toml`; the `audit` and `publish` jobs install no toolchain at all). `yq` and `jq` come from the runner image (`ci.yml:156`, `:173`, `:251`, `:299`, `:501`, `:600`, `:622`). `deploy-test` pulls the floating `registry:2` tag (`tests/deploy/run.sh:131`), and every job runs on the floating `ubuntu-latest` runner (`ci.yml`, `cleanup.yml:17`). Actions are pinned by commit SHA, and the other images by version.
14. **Inconsistent compose-image extraction.** CI and scripts find images in `docker-compose.yml` with a grep for the first `image:` line (`ci.yml:143-144`, `:153-154`, `:171-172`; `tests/telemetry/run.sh:34-40`), and elsewhere with `yq` by service name (`ci.yml:251`).
15. **The telemetry gate hard-codes the client service name.** It uses `app-desktop` (`tests/telemetry/run.sh:82`, `tests/telemetry/battery.py:20`) instead of reading the `CLIENT_SERVICE_NAME` default from `docker-compose.yml:431` or `.env.example:409`.
16. **Branch protection is outside the repo.** Nothing in the repository makes a red CI block a merge. Only `publish` is gated, through `needs` (`ci.yml:841`). No job enforces a coverage threshold (`ci.yml:346-371`).
17. **The tag trigger is looser than the docs say.** `documentation/DEPLOYMENT.md:59-60` says "Only `vMAJOR.MINOR.PATCH` triggers a release", but the trigger glob `v*.*.*` (`ci.yml:23`) also matches e.g. `v1.2.3-rc.1`. Such a tag publishes if `Cargo.toml` carries the same version, but `deploy.sh` treats `APP_VERSION=1.2.3-rc.1` as a pulling non-release mode (`scripts/deploy.sh:209`), and the weekly scan's `release` ignores it (`ci.yml:775-776`).
18. **Deploy-doc claims that do not fully hold.**
    - `DEPLOYMENT.md:172-173` and `MULTI_INSTANCE.md:83` say `deploy.sh` refuses bad config "before anything changes". But `sync_checkout` (`deploy.sh:562`) runs `git pull` before the checks (`:563-566`), so a refused deploy can leave the checkout fast-forwarded. Directory bind mounts (`caddy/sites`, `loki-rules`, `grafana/provisioning/*`, `generated/prometheus`) expose the new files to running containers immediately.
    - `DEPLOYMENT.md:214-216` does not say that re-encryption needs `ENABLE_BACKGROUND_CLEANUP=true` (`src/server.rs:794`).
    - DEPLOYMENT.md's network table, port list, publish rules and scanning description match the code.
19. **One-shots run twice per rolling deploy.** db-bootstrap and db-migrate run once through `up --scale … api` (`deploy.sh:458`). They are then included in `docker compose up -d $others` (`deploy.sh:593-595`), which restarts exited one-shot containers (as noted at `deploy.sh:391-393`). Both are idempotent, but every deploy re-applies the `app_user` password and re-runs the migrator twice.
20. **Watched-path coverage is uneven.** `converge_mounted_config` watches only `grafana/provisioning/dashboards/dashboard.yml`, not the dashboard JSON files in the same mounted directory (`deploy.sh:541`, `docker-compose.yml:463`). Editing a dashboard JSON does not restart Grafana. Whether Grafana picks it up depends on its provisioning reload (OBS).
21. **The api container receives all of `.env`.** That includes `CADDY_*`, `API_IMAGE`, `CLIENT_TELEMETRY_*` and `TEST_S3_SECRET_ACCESS_KEY`. Only four keys are blanked (`docker-compose.yml:47-50`). `TEST_DATABASE_URL` is blanked, but the `TEST_S3_*` credentials, active by default in `.env.example:1005-1009`, are not. They equal the S3 credentials the api already holds.
22. **Caddy header handling relies on defaults.** The Caddyfile configures no response headers (no HSTS or security headers), no request-size limit and no `trusted_proxies` (`Caddyfile:1-81`). The per-IP rate limit's resistance to spoofing therefore rests on Caddy's default of replacing a client-supplied `X-Forwarded-For` with the real peer address, because the API trusts the first XFF entry and falls back to `X-Real-IP` (`src/utils/request.rs:49-71`).
23. **`.env.example` and documentation drift from the code.**
    - `.env.example:530` says session cleanup removes "90+ day stale refresh tokens"; the code default refresh lifetime is 30 days and cleanup uses `refresh_token_expires_at < NOW()` (`src/config/server.rs:89-94`).
    - `.env.example:1325` says log formatting is in `src/main.rs`; it is in `src/server.rs:976-993`.
    - `.env.example:1201` says the collector is at "localhost:4317 in Docker Compose"; Compose publishes no host port for it (`docker-compose.yml:422-428`).
    - `.env.example:139` and `documentation/DATABASE.md:45` give `migrations/0012_*.sql` as the next migration; the repo has `0001`-`0008`.
    - `.env.example:173-175` names test databases `apptest_<uuid>`; they are `apptest_<epoch>_<uuid>` (`tests/common/mod.rs:377`).
    - `.env.example:183` advises `127.0.0.1`, not `localhost`, for `TEST_DATABASE_URL`; CI uses `localhost` (`ci.yml:212`).
24. **Code comments that disagree with code.**
    - `Cargo.toml:90` claims lettre's "file-transport for development", but that feature is not enabled (`:92-98`).
    - `Cargo.toml:40` limits reqwest's purpose to Google OAuth; it also serves GitHub OAuth and the telemetry proxy.
    - `tests/common/mod.rs:292-294` and `:435-436` refer to sqlx 0.8; the crate uses 0.9 (`Cargo.toml:19`).
    - The `.dockerignore:15-18` claim that "nothing include_str!s" a `.md` holds only for non-test code (`src/middleware/mod.rs:380` does so in a test).
25. **The healthcheck depends on the base image.** The compose healthcheck needs `wget` (Alpine's busybox) inside the api image (`docker-compose.yml:53`). The Dockerfile has no `HEALTHCHECK` of its own (`Dockerfile:40-51`).
26. **`admin.sh` relies on partial managed-DB detection.** `admin.sh reset-db` decides it is on managed Postgres only by a non-empty `DATABASE_URL` (`scripts/admin.sh:53-60`). `rotate-db-password` has no managed-DB check and assumes a running bundled `postgres` container (`:93-97`).
27. **Two compose behaviours are documented but not enforced.** Interpolating `.env` values that reference other variables (`${APP_DB_PASSWORD}` in a managed `DATABASE_URL`, `${S3_*}` in `TEST_S3_*`) relies on Compose and dotenvy substitution (`.env.example:98-101`, `:1005-1009`). A `COMPOSE_PROFILES` value without `bundled-db`, combined with the default DSNs, fails only at db-bootstrap's 60-attempt poll (about 2 minutes, `scripts/db-bootstrap.sh:30-42`).

## Open questions

1. Should every startup configuration failure (including the encryption key ring) share one exit code and message format? Today the key ring panics with exit 101 and config errors print twice (Observation 4).
2. Should set-but-empty optional variables (`GOOGLE_CLIENT_ID=`, `GITHUB_*=`, `S3_ENDPOINT=`) count as unset, be rejected, or keep enabling the feature with an empty value (Observation 1)?
3. Is the fail-open `ENVIRONMENT=development` default, with its case-sensitive exact match, intended to carry over (Observations 2-3)?
4. Should the shutdown sequence be bounded to fit inside `stop_grace_period` (45 s), or is losing the final OTel flush to SIGKILL acceptable (Observation 7)?
5. Is the deploy tooling (`deploy.sh` with git-pull and re-exec, the lock, rollback, mounted-config restarts, the Compose topology) part of the "functionality to preserve" for the rewrite, or only the API binary's contract (CLI, env, healthcheck, migrate one-shot)?
6. Should CI publish the exact image it tested and scanned, and boot the arm64 image at least once (Observation 12)? Should the real image be exercised by `deploy-test` (Observation 11)?
7. Which CI checks are required for merging (branch protection is not in the repo), and is a coverage threshold wanted (Observation 16)?
8. Should prerelease tags (`v1.2.3-rc.1`) publish, and should `deploy.sh` treat them as releases (Observation 17)?
9. Should the live-test harness apply migrations through the production migrator rather than raw SQL (Observation 10)?
10. Do Compose's `:?` guards on profile-gated services (`S3_SECRET_ACCESS_KEY`, `GRAFANA_ADMIN_PASSWORD`) block commands when those profiles are dropped? Static reading cannot tell; it depends on Compose's interpolation of inactive services.
11. Should `TEST_*` variables be active in `.env.example` and so reach the api container (Observation 21)?
12. Is local development on Windows (the MSVC `rust-lld` linker and `debug = 1`, OPS-020) a supported target for the rewrite?
13. Should Caddy set security headers (HSTS etc.) and explicit `trusted_proxies`, or should the API stop trusting `X-Forwarded-For` / `X-Real-IP` unconditionally? Today's safety depends on Caddy's defaults (Observation 22).
