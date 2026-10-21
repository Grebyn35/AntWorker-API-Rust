# Inventory: Observability (`OBS`)

Reference: `AntWorker-API` @ `0210895` (2026-10-20). This file records what the old API does. It does not prescribe what the new one should do.

## Scope

OBS covers how the API produces and ships telemetry, and the bundled stack that stores, alerts on and displays it:

- **Trace and correlation IDs**: where they come from, how they move through the process and reach the client, and how a developer gets from an ID to the trace and its logs.
- **Logging**: the console and OTLP log pipelines, levels and filters, the structured-field conventions, and PII rules (including the `tests/lint_tracing_instrument.rs` lint).
- **Tracing**: spans, attributes, sampling, OTLP exporters, TLS, the two backends (`local`, `opentelemetry`), and startup and shutdown.
- **Metrics**: every instrument the API emits, plus the client and infrastructure metrics the alerts and dashboards use.
- **The client telemetry proxy**: `POST /v1/telemetry/{traces,metrics,logs}`.
- **The monitoring stack**: the OTel collector, Prometheus, alert rules, Alertmanager, blackbox-exporter, Loki, Tempo, cAdvisor and Grafana, and how the stack is switched on and off.

Boundaries:
- **HTTP** owns the error envelope, `ApiError`, rate limiting (including the telemetry request-count tier), idempotency (the `X-Original-Trace-Id` header) and CORS. OBS lists them here only where they carry a trace ID or shape the proxy.
- **AUDIT**, **RT** and **DATA** own the tables that store a `trace_id` column.
- **USER** owns `PUT /v1/users/me/telemetry-consent`.
- **MAIL** owns sending email. OBS owns the email metrics.
- **OPS** owns `deploy.sh`, CI and the Compose topology outside the observability services.

Files read in full:
- Rust:
  - `src/config/observability.rs`
  - `src/services/{observability_service,metrics_service,noop_metrics_service,otel_metrics_service,telemetry_proxy_service}.rs`
  - `src/handlers/telemetry.rs`
  - `src/repositories/{observability_repository,local_observability_repository,otlp_observability_repository}.rs`
  - `src/middleware/{logging,metrics,request_context,catch_panic,cors}.rs`
  - `src/utils/{trace,db_pool_metrics}.rs`
  - `src/errors/observability_error.rs`
  - `src/background.rs`
- Collector and backend configs:
  - `otel-collector-config.yml`, `prometheus.yml`, `prometheus-alerts.yml`, `alertmanager-config.yml`, `blackbox-config.yml`
  - `loki-config.yml`, `loki-overrides.yml`, `loki-rules/tls.rules.yml`, `tempo-config.yml`, `tempo-overrides.yml`
  - `grafana/entrypoint.sh`, `grafana/provisioning/**` (the six dashboards summarised by parsing their JSON)
  - `generated/prometheus/.gitignore` (the directory holds no committed file)
- Compose, scripts and docs:
  - `docker-compose.yml`, `Caddyfile`, `scripts/fleet-rules.sh`
  - `documentation/LOGGING.md`, `documentation/TELEMETRY.md`
- Tests:
  - `tests/lint_tracing_instrument.rs`, `tests/otlp_export_tls.rs`, `tests/telemetry/run.sh`
  - `tests/telemetry/battery.py` (skimmed for coverage)
  - `tests/prometheus/alert-rules.test.yml` (skimmed for coverage)

Read in part:
- `src/server.rs`: subscriber setup, provider and gauge wiring, router and middleware composition, shutdown, and its inline tests.
- `src/config/environment.rs`: OBS env parsing and validation, and the feature status.
- `src/middleware/{mod,rate_limit,idempotency}.rs`
- `src/errors/{mod,response_envelope,api_error}.rs`
- `src/services/{email_service,sse_notification_service,public_auth_service,rate_limit_service}.rs`
- `src/repositories/postgresql/{sse_event_repository,sse_notification_repository}.rs`
- `src/handlers/{mod,sse_notifications}.rs`
- `.env.example` (OBS sections), `scripts/deploy.sh` (OBS parts), `.github/workflows/ci.yml` (OBS jobs), `README.md`, `documentation/DEPLOYMENT.md`
- `tests/panic_response_http.rs`, `tests/router_composition.rs`
- For library semantics: the local cargo-registry sources of `opentelemetry-appender-tracing` 0.31.1 and `tracing-opentelemetry` 0.32. `Cargo.lock` pins 0.33.1 and 0.34.0, which are not in the local cache.

Not opened, by rule: `.env` and `alertmanager-secrets/`. Both exist. The secrets directory holds the receiver credentials that `alertmanager-config.yml` references through `*_file` settings.

## Trace-ID journey

Throughout this document, a span is one timed step in a request; a trace is the tree of spans for one request; a trace ID is the 32-character hex identifier they share. The API has no request ID. The W3C trace ID is the only correlation ID.

1. **Origin.**
   - `logging_middleware` reads the inbound `traceparent` header, plus `tracestate` and `baggage` when present (`src/middleware/logging.rs:99-174`). It parses them with the global W3C TraceContext + Baggage propagator. Both backends register that propagator at startup (`src/repositories/local_observability_repository.rs:125-132`, `src/repositories/otlp_observability_repository.rs:334-341`).
   - If `traceparent` is valid, the request span joins the caller's trace (`logging.rs:351-371`).
   - If it is missing, invalid or not ASCII, the SDK starts a new trace for the request span.
   - CORS allows only `Authorization`, `Content-Type` and `Accept` request headers (`src/middleware/cors.rs:47`). A cross-origin browser client therefore cannot send `traceparent` or `baggage` past a preflight.
   - Caddy forwards the headers unchanged. It neither starts nor records a trace and writes no access log (`Caddyfile:12-22`).

2. **The request span.**
   - Every request gets one `http_request` span. Its exported name is `"{METHOD} {route}"`.
   - Inside the span's scope the middleware records the span's own `otel.trace_id` and `otel.span_id` as span fields (`logging.rs:394-403`).
   - Sampling:

     | Backend | Sampler | Effect |
     |---|---|---|
     | `local` | `ParentBased(AlwaysOff)` (`local_observability_repository.rs:101`) | Real IDs, never exported |
     | `opentelemetry` | `ParentBased(TraceIdRatioBased(OTEL_TRACE_RATIO))` (`otlp_observability_repository.rs:202`) | A caller's sampled flag overrides the ratio either way |

3. **Inside the process.**
   - Handler, service and repository functions are `#[tracing::instrument]` spans nested under the request span (305 instrumented functions).
   - `extract_or_invalid_in(subsystem)` reads the current span's trace ID (`src/utils/trace.rs:104-126`). Error contexts, audit inserts, SSE event inserts, idempotency cache writes, `retry` and `database_retry` all call it. Each of them falls back to the zero sentinel plus an `error!` log when no span is active.
   - **Background jobs:** each cycle of each periodic task is a new root trace named `background_task {task_id}` (`src/background.rs:82-104`). The shared SSE listener runs under a root span (`src/server.rs:1287-1292`).
   - **Email:** synchronous sends run inside the request's trace as a `service_email_send` span. The password-reset fire-and-forget send is spawned with `.instrument(Span::current())`, so it stays in the request's trace (`src/services/public_auth_service.rs:1975-1988`).
   - **SSE delivery:** the stream is polled after the handler returns, outside the request span. Each `service_sse_process_notification` span therefore starts a new root trace with no link to the publishing request (`src/services/sse_notification_service.rs:235-244`).
   - **Outbound HTTP:** no code injects `traceparent` (no propagator `inject` call anywhere in `src/`). OAuth providers, S3, avatar fetches and the collector hop all start fresh.

4. **Stored with data.**
   - Columns that store the publishing request's trace ID:
     - `workspace_audit_logs.trace_id` and `user_audit_logs.trace_id` (AUDIT)
     - `sse_events.trace_id` (RT, `src/repositories/postgresql/sse_event_repository.rs:77-94`)
     - `idempotency_cache.original_trace_id` (HTTP)
   - No trace context reaches PostgreSQL: no `application_name` and no SQL comment (`src/config/database.rs`). Database-side logs cannot be correlated.

5. **Logs.**
   - **Console JSON:** each line carries the full span list. Once recorded, that includes the request span's `otel.trace_id`. The centralised 4xx/5xx log also writes `trace_id` as an event field (`logging.rs:457-493`).
   - **OTLP logs** (opentelemetry backend only):
     - The log bridge copies event fields only. Span fields such as `component`, `subsystem` and `http.route` are not copied.
     - The OTel SDK fills each record's trace and span IDs from the current OTel context.
     - That context exists only because `tracing-opentelemetry` activates it when a span is entered. The appender's own span-context feature is not enabled (`Cargo.toml:63`).
     - Loki stores `trace_id` as structured metadata (`loki-config.yml:1-2,77`).

6. **To the client.**
   - **Every JSON envelope:** `meta.trace_id` on success and error alike (`src/errors/response_envelope.rs:123-129`).
   - **SSE events:** the `data` envelope carries `trace_id`, which is the publishing request's ID, not the stream's (`sse_notification_service.rs:394-397,478-482`).
   - **Audit log API rows:** carry `trace_id` (AUDIT).
   - **Idempotent replays:** carry `X-Original-Trace-Id` and the original cached body, whose `meta.trace_id` is the original request's (`src/middleware/idempotency.rs:741-768`).
   - **Nothing else:**
     - No response header carries the current trace ID (no `traceresponse`, no `X-Trace-Id`).
     - CORS exposes no headers.
     - Raw responses carry no trace ID at all: file downloads, the proxy's relayed collector answers and the SSE stream.
     - Emails carry none.

7. **Export.**
   - The API exports over OTLP/gRPC to the collector.
   - The collector's tail sampler keeps a trace if any of these hold (`otel-collector-config.yml:74-116`):
     - any span has ERROR status;
     - the trace runs longer than `OTEL_TAIL_LATENCY_THRESHOLD_MS` (500 ms by default);
     - it falls in the `OTEL_TAIL_SAMPLING_PERCENTAGE` share (10% by default) of the rest. Health and telemetry-proxy routes are excluded from that share.
   - Traces go to Tempo tenant `fake`; logs to Loki tenant `fake`.
   - Client spans forwarded by the proxy go to tenant `client` and skip tail sampling.

8. **From ID to trace and logs.**
   - **Dashboard search box:** the App API dashboard (`app-api-red`) has a `search` textbox. Pasting a trace ID filters two panels:
     - "Recent Traces", a TraceQL search: `{resource.service.name="$service_api" && trace:id =~ "$search.*"}`
     - "Application Logs Stream", a Loki query: `{service_name="$service_api", service_instance_id=~"$instance"} | trace_id=~"(?i).*$search.*"`

     The desktop dashboard has the same pair for `$service_desktop`.
   - **Trace to logs:** in Explore, the Tempo datasource's `tracesToLogsV2` links a span to Loki, filtered by the span's `service.name` and the trace ID (`grafana/provisioning/datasources/tempo.yml:22-31`).
   - **Both tenants:** both datasources send `X-Scope-OrgID: fake|client`.
   - **Logs to trace:** the Loki datasource has no derived field, so a log line has no link to its trace (`grafana/provisioning/datasources/loki.yml`).
   - **Metrics to trace:** Prometheus stores exemplars and the datasource links them to Tempo. The Rust SDK emits none (`grafana/provisioning/datasources/prometheus.yml:17-21`).

**Where the chain breaks:**
- **(a) `local` backend:** nothing is exported. The ID only helps with grepping console output (`docker compose logs api`).
- **(b) Head sampling:** with `OTEL_TRACE_RATIO` below 1.0, or when a caller sends `sampled=0`, the API's spans are never exported. That includes 5xx errors: tail sampling cannot keep what never arrived. The logs still carry the ID.
- **(c) 4xx requests:** they log at warn and leave span status unset, so tail sampling does not treat them as errors. By default, a fast 4xx a user reports has a 10% chance of being in Tempo. Its warn log is in Loki.
- **(d) Zero sentinel everywhere:** two causes:
  - The tracer exporter fails to build in `opentelemetry` mode (`src/server.rs:959-962`).
  - The registry-wide filter disables info-level spans, for example `RUST_LOG=warn`.

  Either way no OTel span exists, and every envelope carries `00000000000000000000000000000000`.
- **(e) Unlinked traces:** SSE delivery spans and background jobs are separate traces, and no ID from them is shown to anyone.
- **(f) Outbound calls:** they do not propagate the trace.
- **(g) Grafana links:** there is no log-to-trace link and no exemplar.
- **(h) PostgreSQL:** it is not correlated.
- **(i) Successful requests:** they write no log line of their own, so "see the logs" for a 2xx shows only the business and debug events emitted inside it.
- **(j) Idempotent replays:** the body's `meta.trace_id` names the original request, not the replay.
- **(k) Loki filters:** OTLP log records lack span fields, so a Loki filter on `subsystem` or `component` matches only events that set those fields themselves.

## Behaviors

### OBS-001 Every request runs inside one `http_request` server span

- **Behavior:**
  - Every request that reaches the global middleware tier is wrapped in a tracing span named `http_request`, with OTel name `"{METHOD} {route}"`.
  - Its fields: `http.request.method`, `http.route`, `url.path`, `http.response.status_code`, `duration_ms`, `error.type`, `otel.trace_id`, `otel.span_id`, `user_id`, `workspace_id`.
  - Every handler, service and repository span is its child.
- **Kind:** `guarantee`
- **Trigger:** Any HTTP request, including `/health/*`, `/v1/telemetry/*` and unmatched paths.
- **Auth:** Not applicable (global layer, runs before auth).
- **Effects:**
  - `duration_ms` and the status code are recorded after the inner stack returns.
  - For streamed bodies (SSE), the span closes when the handler returns its response, not when the stream ends.
  - `error.type` is `client_error` for 4xx and `server_error` for 5xx, and empty otherwise.
- **Guarantees:**
  - Global order: `DefaultBodyLimit → cors → request_context → logging → metrics → catch_panic → route layers`. So:
    - a handler panic is recorded inside the span as a 500;
    - CORS preflights answered by `CorsLayer` get no span.
  - The span is created with `.instrument()`, not `enter()`, so it is safe across `.await`.
- **Source:**
  - `src/middleware/logging.rs:215-546`
  - `src/server.rs:711-741`
  - `src/middleware/catch_panic.rs:18-28`
- **Tests:**
  - `tests/router_composition.rs::build_router_returns_its_routes_wrapped_in_the_global_middleware_tier`
  - `tests/panic_response_http.rs`

### OBS-002 An inbound W3C `traceparent` continues the caller's trace

- **Behavior:** If the request carries a valid `traceparent`, the request span becomes a child of that remote span and shares its trace ID. `tracestate` and `baggage` are parsed along with it.
- **Kind:** `capability`
- **Trigger:** Any HTTP request carrying `traceparent`.
- **Auth:** None (before auth).
- **Input & validation:**
  - `traceparent` must be present and visible ASCII; `tracestate` and `baggage` are optional.
  - A missing, non-ASCII or invalid `traceparent` (per the propagator) is ignored and a new trace starts. That case logs `debug!` and echoes the raw header.
  - Baggage is extracted only when `traceparent` is valid.
- **Effects:**
  - On success, `debug!` "Extracted W3C Trace Context" (fields `trace_id`, `parent_span_id`, `sampled`, `has_baggage`).
  - The caller's sampled flag decides export, because the samplers are `ParentBased` (OBS-016, OBS-018).
- **Source:**
  - `src/middleware/logging.rs:99-174`, `:261`, `:351-361`
  - `src/repositories/local_observability_repository.rs:125-132`
  - `src/repositories/otlp_observability_repository.rs:334-341`
- **Tests:** none found.

### OBS-003 Client baggage `user_id` and `workspace_id` become span attributes

- **Behavior:** When a valid `traceparent` comes with W3C baggage entries `user_id` or `workspace_id`, their values are recorded as the `user_id` and `workspace_id` attributes of the request span, so a TraceQL query such as `{ span.user_id = "..." }` can find the trace.
- **Kind:** `capability`
- **Trigger:** A request with both a valid `traceparent` and a `baggage` header.
- **Auth:** None. The values are client-asserted and never checked against the authenticated session.
- **Input & validation:** The values are recorded as given, with no format check.
- **Source:** `src/middleware/logging.rs:332-333`, `:355-370`
- **Tests:** none found.

### OBS-004 Route templating, with unmatched paths bucketed as `/<unmatched>`

- **Behavior:**
  - The span name, the `http.route` attribute, metric labels and the error-log `http_path` field all use Axum's matched route template.
  - A request that matches no route uses the literal `/<unmatched>`.
  - The raw path is kept in the span attribute `url.path` and in the `url_path` field of 4xx and 5xx logs.
- **Kind:** `rule`
- **Trigger:** Any HTTP request.
- **Effects:** A request without a matched path also logs `debug!` "No MatchedPath found…", once from logging and once from metrics.
- **Source:**
  - `src/middleware/logging.rs:224-250`, `:297`
  - `src/middleware/metrics.rs:132-150`
- **Tests:** none found.

### OBS-005 Every JSON envelope carries `meta.trace_id`

- **Behavior:**
  - Every `ApiResponse` envelope, success or error, includes `meta: { api_version: "v1", trace_id, timestamp }`.
  - `trace_id` is the current span's 32-character lowercase hex trace ID, read when the envelope is built.
  - Error contexts also capture the trace ID where the error is created (`auth_error`, `oauth_error`, `repository_error`, `service_error`, `storage_error`). The log keeps that origin ID; the envelope's `meta.trace_id` is read again when the response is built.
- **Kind:** `capability`
- **Trigger:** Any handler or middleware response built with `ApiResponse` or `ApiError::into_response`.
- **Auth:** Any.
- **Effects:**
  - The client can quote the ID.
  - It is a real ID even under `OBSERVABILITY_BACKEND=local`.
- **Guarantees:** The ID is never a fabricated UUID: either the real trace ID or the zero sentinel (OBS-006).
- **Source:**
  - `src/errors/response_envelope.rs:93-129`
  - `src/errors/service_error.rs:152-160`
  - `src/errors/repository_error.rs:786-801`
- **Tests:**
  - `tests/panic_response_http.rs`: real non-zero ID in a 500 envelope.
  - `src/errors/response_envelope.rs` inline tests: field always present.
  - `src/errors/service_error.rs::new_captures_trace_id_and_timestamp`

### OBS-006 Zero sentinel and loud log when no span is active

- **Behavior:**
  - Any code that needs a trace ID and finds no valid span gets `00000000000000000000000000000000`.
  - At the same time it emits `error!` with `component="infrastructure"`, `trace_id=<sentinel>`, `subsystem=<caller tag>` and the message "trace_id extraction failed…".
- **Kind:** `hardening`
- **Trigger:** `extract_or_invalid` or `extract_or_invalid_in` called outside a span.
- **Effects:**
  - Caller tags: `response_envelope`, `auth_error`, `oauth_error`, `repository_error`, `service_error`, `storage_error`, `sse_event_repository`, `user_audit_log_repository`, `workspace_audit_log_repository`, `idempotency_repository`, `metrics_middleware`, `retry`, `database_retry`, `unspecified`.
  - The sentinel is also written into the audit, `sse_events` and `idempotency_cache` rows.
- **Source:**
  - `src/utils/trace.rs:39-126`
  - `src/utils/mod.rs:18`
- **Tests:** `src/utils/trace.rs` inline tests (`invalid_trace_id_matches_w3c_spec`, `extract_or_invalid_returns_sentinel_when_outside_any_span`, `extract_or_invalid_in_tags_subsystem`).

### OBS-007 Span status follows log level: 5xx is ERROR, 4xx is unset

- **Behavior:**
  - A 5xx response logs at `error!` inside the request span. The trace layer turns an error-level event into ERROR status on that span, which is what the collector's `errors-policy` keeps.
  - A 4xx logs at `warn!` and leaves the status unset.
  - Nothing sets status explicitly; this relies entirely on `tracing-opentelemetry`'s error-event behaviour.
- **Kind:** `rule`
- **Trigger:** A response with status 400-599.
- **Source:**
  - `src/middleware/logging.rs:205-214`, `:452-534`
  - `otel-collector-config.yml:84-87`
- **Tests:** none found.

### OBS-008 One centralised structured log per 4xx and 5xx response

- **Behavior:** When a response carries `ErrorLogContext` (attached by `ApiError::into_response`), `logging_middleware` emits exactly one event.
  - **Levels and messages:** 5xx → `error!` "Request failed with server error"; 4xx → `warn!` "Request failed with client error".
  - **Fields:** `component="middleware"`, `subsystem="logging"`, `error_type`, `error_code`, `error_message` (the internal message, not the client-facing one), `trace_id` (captured where the error was created), `timestamp`, `error_details`, `http_status`, `http_method`, `http_path` (route template), `url_path` (raw path), `duration_ms`.
  - **Without context:** a 4xx or 5xx with no `ErrorLogContext` gets the same level with the message "Request failed without structured error context" and only the HTTP fields. Examples:
    - an unmatched 404;
    - axum's own extractor rejections, such as the body-limit 413 on the telemetry routes;
    - a collector status relayed by the proxy.
- **Kind:** `guarantee`
- **Trigger:** Any 4xx or 5xx response passing through the global tier.
- **Effects:**
  - With the `opentelemetry` backend the event also goes to Loki.
  - A caught handler panic logs `error_code="handler_panic"` and `error_type="internal_error"` with the panic text, which never reaches the client.
- **Guarantees:**
  - Lower layers do not log returned errors, so each error is logged exactly once.
  - Fail-open swallows log at their own site instead (OBS-028).
- **Source:**
  - `src/middleware/logging.rs:425-534`
  - `src/errors/mod.rs:24-57`
  - `src/middleware/catch_panic.rs:18-28`
- **Tests:**
  - `tests/panic_response_http.rs`: one ERROR event per panic, with its fields and trace ID.
  - `src/middleware/catch_panic.rs` inline tests.

### OBS-009 No log line for 1xx, 2xx or 3xx responses

- **Behavior:** `logging_middleware` emits nothing for a successful or redirecting response. The span and the metrics are the only record of it.
- **Kind:** `rule`
- **Trigger:** Any response below 400.
- **Source:** `src/middleware/logging.rs:535-541`
- **Tests:** none found.

### OBS-010 Background task cycles and the SSE listener are root traces

- **Behavior:**
  - Each tick of a periodic task runs inside a new `background_task` span (OTel name `background_task {task_id}`, fields `component="background_task"`, `subsystem=task_id`, `task_id`), wrapping the distributed-lock acquire and the work.
  - The shared PgListener runs in an `sse_shared_listener` span (`parent: None`, `component="background_task"`, `subsystem="sse_notification"`).
  - Shutdown events log outside these spans.
- **Kind:** `guarantee`
- **Trigger:** Background task intervals, and the SSE listener at startup.
- **Effects:**
  - Cleanup outcomes go to `log_cleanup_result`: `info!` when the count is above zero, `debug!` at zero, `error!` on failure. `component="background_cleanup"`, `subsystem=cleanup_type`.
  - The listener's final exit is logged by its caller: `error!` on init failure, `warn!` when the stream ends, `debug!` on shutdown.
- **Source:**
  - `src/background.rs:68-127`, `:201-230`
  - `src/server.rs:1281-1330`
- **Tests:** `src/background.rs` inline tests (shutdown only, no span assertions).

### OBS-011 SSE delivery is traced per notification, separately from the publisher

- **Behavior:**
  - Each live notification a connected stream processes gets a `service_sse_process_notification` span (fields: `event_id`, `event_type`, `scope_type`, `scope_id`, `user_id`) with a `service_sse_fetch_and_format_event` child.
  - These spans are created while the response stream is polled, outside the request span, so they start new traces.
  - The publisher's trace ID reaches the client only inside the event `data` envelope (`{"data": …, "trace_id": …}`); there is no span link back to the publisher.
- **Kind:** `guarantee` (the per-event span is promised; the missing link is an observation).
- **Trigger:** An SSE notification delivered on `GET /v1/users/me/events`.
- **Auth:** Session.
- **Source:**
  - `src/services/sse_notification_service.rs:229-284`, `:371-410`, `:464-482`
  - `src/repositories/postgresql/sse_event_repository.rs:71-94`
- **Tests:** `tests/sse_event_repository_live.rs` (stored `trace_id`).

### OBS-012 Instance identity on all telemetry

- **Behavior:**
  - At startup the process generates one UUID v4.
  - The UUID becomes the OTel resource attribute `service.instance.id` (Prometheus label `service_instance_id`, Loki stream label `service_instance_id`).
  - The distributed locks use the same UUID.
  - Every resource has exactly `service.name`, `service.version`, `deployment.environment` and `service.instance.id`. Resource detectors are off (`Resource::builder_empty()`), so `OTEL_RESOURCE_ATTRIBUTES` is ignored.
- **Kind:** `guarantee`
- **Trigger:** Startup.
- **Effects:**
  - A restarted container reports under a new ID.
  - Event-level logs deliberately carry no `instance_id` field.
- **Source:**
  - `src/server.rs:857-864`, `:1504-1513`
  - `src/repositories/otlp_observability_repository.rs:166-178`
  - `src/repositories/local_observability_repository.rs:79-91`
- **Tests:** `src/repositories/otlp_observability_repository.rs::resource_creation`

### OBS-013 Backend selection, validation and the startup banner

- **Behavior:**
  - `OBSERVABILITY_BACKEND` selects `local` (also the default when the variable is unset) or `opentelemetry`, case-insensitively.
  - Before any subscriber exists, the server prints a one-line banner to stderr: endpoint (opentelemetry only), service@version, environment and instance.
- **Kind:** `capability`
- **Trigger:** Startup (`server::run`).
- **Input & validation:**
  - Any other backend value is a fatal config error.
  - With `opentelemetry`, these are fatal:
    - `OTEL_ENDPOINT` missing, empty, unparseable, or not `http`/`https`;
    - `OTEL_TRACE_RATIO` not finite or outside `[0,1]`;
    - `OTEL_METRIC_EXPORT_INTERVAL_SECS` 0 or above 150;
    - empty `OTEL_SERVICE_NAME`, `OTEL_SERVICE_VERSION` or `ENVIRONMENT`.
  - These checks are skipped under `local`.
- **Effects:**
  - The `opentelemetry` feature status is logged `info!` after subscriber init. Enabled: mode `OTLP/gRPC`, with endpoint, service, version, environment, `sampling_percent` and `metric_export_secs`. Under `local`: "opentelemetry DISABLED".
  - Config failures print `FATAL: …` to stderr and exit.
- **Errors:** startup exits on a `ConfigLoadError::InvalidValue`.
- **Source:**
  - `src/config/environment.rs:267-279`, `:589-603`, `:1340-1432`, `:1766-1787`
  - `src/config/observability.rs:137-184`
  - `src/server.rs:842-855`, `:876-924`, `:1045-1090`
- **Tests:** `src/config/environment.rs` inline tests (`config_rejects_otel_trace_ratio_*`, `config_valid_otel_boundary_values_pass`, `config_rejects_otel_endpoint_*`, `config_rejects_otel_metric_export_interval_*`, `config_rejects_empty_environment_when_opentelemetry`).

### OBS-014 Local backend: real trace IDs, nothing exported

- **Behavior:**
  - `local` builds an `SdkTracerProvider` with `ParentBased(AlwaysOff)`, no processor and no exporter, and sets it as the global tracer provider.
  - No meter or logger provider is built. Metrics fall back to the no-op service, and logs go to the console only.
  - Spans still get real trace and span IDs. A caller's sampled `traceparent` makes them recording, but they are still not exported.
- **Kind:** `capability`
- **Trigger:** Startup with `OBSERVABILITY_BACKEND=local` or unset.
- **Effects:** None of `instance_up`, `db_pool_connections` or `sse_active_subscribers` is registered (OBS-034).
- **Source:**
  - `src/repositories/local_observability_repository.rs:47-186`
  - `src/server.rs:904-923`, `:1146-1168`
- **Tests:** `src/repositories/local_observability_repository.rs` inline tests (`initialize_returns_real_tracer…`, `always_off_sampler_still_produces_valid_span_context`, flush and shutdown).

### OBS-015 OTLP backend: three OTLP/gRPC exporters to one endpoint

- **Behavior:** `opentelemetry` builds three providers, all exporting over tonic gRPC to `OTEL_ENDPOINT` with a shared resource (OBS-012):
  - a tracer provider with a batch span exporter;
  - a meter provider with a `PeriodicReader` every `OTEL_METRIC_EXPORT_INTERVAL_SECS` seconds (default 15), cumulative temporality;
  - a logger provider with a batch log exporter.

  The tracer provider becomes global, the composite propagator is registered, and the tracing subscriber gets the OTel trace layer and the log bridge (OBS-023).
- **Kind:** `capability`
- **Trigger:** Startup with `OBSERVABILITY_BACKEND=opentelemetry`.
- **Input & validation:**
  - Batch and queue sizes are the SDK defaults; the app does not set them.
  - `OTEL_EXPORTER_OTLP_HEADERS` is read by the SDK itself, not by app code.
- **Effects:** `info!` "OpenTelemetry unified observability initialized" with booleans `traces`, `metrics`, `logs`.
- **Guarantees:**
  - Export runs in the background. An unreachable collector does not slow or fail requests; failed exports are dropped and the SDK logs to the console.
  - The tonic transport connects lazily, so startup does not need the collector up.
- **Source:**
  - `src/repositories/otlp_observability_repository.rs:56-350`
  - `src/server.rs:880-903`, `:938-1025`
- **Tests:**
  - `src/repositories/otlp_observability_repository.rs` inline tests
  - `tests/otlp_export_tls.rs::https_endpoint_trusts_the_system_store` (all three signals export)

### OBS-016 Head sampling is ParentBased + TraceIdRatioBased

- **Behavior:**
  - Under `opentelemetry`, a root trace is sampled deterministically from its trace ID at `OTEL_TRACE_RATIO` (default 1.0).
  - A request with a parent context uses the parent's sampled flag instead.
  - An unsampled span still has a valid trace ID; it is simply not exported.
- **Kind:** `rule`
- **Trigger:** Creation of every root or remote-parented span.
- **Source:**
  - `src/repositories/otlp_observability_repository.rs:199-209`
  - `src/config/observability.rs:87-107`
- **Tests:** `src/config/environment.rs` ratio-validation tests only.

### OBS-017 OTLP TLS: https endpoints verified against the system trust store

- **Behavior:**
  - When `OTEL_ENDPOINT`'s scheme parses as `https` (any letter case, as `http::Uri` reads it), each exporter gets `ClientTlsConfig::new().with_native_roots()`. That is the platform store, or `SSL_CERT_FILE` / `SSL_CERT_DIR` when set.
  - An `http` endpoint gets no TLS config and never loads the store.
- **Kind:** `guarantee`
- **Trigger:** Exporter construction at startup.
- **Errors:** With https and an empty store, all three exporters fail to build: "no native certs found" (see OBS-018).
- **Source:**
  - `src/repositories/otlp_observability_repository.rs:137-152`
  - `Cargo.toml:59-61` (`tls-ring`, `tls-roots`)
  - `Dockerfile:46` (`ca-certificates`)
- **Tests:** `tests/otlp_export_tls.rs` (`https_endpoint_trusts_the_system_store`, `http_endpoint_does_not_load_the_trust_store`)

### OBS-018 A failed exporter build degrades the signal and keeps the server running

- **Behavior:**
  - An exporter that fails to build drops only its own signal: `ObservabilityInitError { signal, code: exporter_creation_failed, message }`.
  - The server keeps the signals that built and logs one `warn!` per failure after subscriber init ("OpenTelemetry signal initialization failed", fields `signal`, `error_code`, `error_message`).
  - Without a tracer there is no OTel layer, and with no `local` fallback every trace ID becomes the zero sentinel (OBS-006).
  - Without a meter provider, metrics use the no-op service.
- **Kind:** `hardening`
- **Trigger:** Startup.
- **Source:**
  - `src/repositories/otlp_observability_repository.rs:282-350`
  - `src/errors/observability_error.rs:39-121`
  - `src/server.rs:959-973`, `:1007-1016`, `:1146-1168`
- **Tests:**
  - `tests/otlp_export_tls.rs::http_endpoint_does_not_load_the_trust_store` (three errors)
  - `src/errors/observability_error.rs` inline tests

### OBS-019 Two-phase telemetry shutdown, last in the shutdown sequence

- **Behavior:** After the HTTP drain, the background-task waits and the DB pool close (logged "Graceful shutdown complete"):
  1. If any provider exists, the server logs `info!` "Flushing OpenTelemetry telemetry…".
  2. It calls `flush()` (`force_flush` on each provider; the providers stay open).
  3. It logs each failed signal as `error!` "OpenTelemetry flush failed".
  4. It logs `info!` "OpenTelemetry flush complete, closing providers" with each signal's status.
  5. It calls `shutdown()` in LIFO order (logger, then meter, then tracer).
  6. It prints any shutdown failure to stderr as `[shutdown] observability <signal> provider failed: <error>`.

  With no providers at all it logs only `debug!` "No OpenTelemetry providers to flush".
- **Kind:** `guarantee`
- **Trigger:** SIGTERM or SIGINT graceful shutdown.
- **Effects:** Per-signal statuses are `success`, `not_initialized`, or `failed: <error>`. A poisoned provider lock reports `Failed`, never `NotInitialized`.
- **Guarantees:**
  - Logs emitted before step 5 are exported.
  - Nothing after it is, apart from stderr.
- **Source:**
  - `src/server.rs:2204-2357`
  - `src/services/observability_service.rs:103-146`
  - `src/repositories/otlp_observability_repository.rs:352-484`
  - `src/repositories/local_observability_repository.rs:141-185`
  - `src/repositories/observability_repository.rs:60-246`
- **Tests:**
  - `src/repositories/observability_repository.rs` inline tests
  - local and OTLP repository poison tests
  - `src/services/observability_service.rs::service_delegates_flush_to_repository`

### OBS-020 Console logs: JSON lines with the span hierarchy

- **Behavior:**
  - Every enabled event is written to stdout as one JSON object, using tracing-subscriber's `json()` with span list, current span, target, thread IDs, file and line number.
  - The format is JSON in every environment; only the level changes.
  - Span fields recorded later, such as `otel.trace_id` and the status code, show up in later lines' span data.
  - Console logs carry no resource attributes (no instance ID).
- **Kind:** `capability`
- **Trigger:** Any `tracing` event, from startup on.
- **Source:** `src/server.rs:976-993`
- **Tests:** none found.

### OBS-021 Default level filters depend on `ENVIRONMENT`, and `RUST_LOG` replaces them

- **Behavior:** A registry-wide `EnvFilter` applies to both console and OTLP. Defaults by `ENVIRONMENT`:

  | `ENVIRONMENT` | Default filter |
  |---|---|
  | `development` | `app_api=debug,tower_http=debug,opentelemetry=error,opentelemetry-otlp=warn` |
  | anything else, including `staging` | `app_api=info,tower_http=warn,opentelemetry=error,opentelemetry-otlp=warn` |

  A set `RUST_LOG` replaces the default entirely.
- **Kind:** `rule`
- **Trigger:** Subscriber initialisation.
- **Effects:**
  - Crates not named in the filter log nothing by default, including `sqlx`, `hyper` and `axum`.
  - The OTel SDK's errors and the exporter's warnings reach the console. SDK warnings do not; for example, the baggage propagator warns once per malformed client entry and echoes the header.
- **Guarantees:**
  - The filter applies to spans as well as events.
  - A filter that disables `app_api` INFO also removes the request and instrument spans, which breaks trace IDs (journey break d).
- **Source:** `src/server.rs:799-816`, `:975-980`
- **Tests:** `src/server.rs` inline tests (`default_filters_route_sdk_diagnostics_to_the_console_only`, `otel_logs_filter_holds_under_a_permissive_rust_log`)

### OBS-022 Migration CLI logging

- **Behavior:** The `migrate` subcommand installs its own subscriber: JSON console, `app_api=info`, target included, no OTel layers.
- **Kind:** `capability`
- **Trigger:** `app-api migrate` (the `db-migrate` one-shot).
- **Source:** `src/migrate.rs:132-134`
- **Tests:** `tests/migrate_command.rs` (not read in detail).

### OBS-023 OTLP log bridge with its own filter that blocks the feedback loop

- **Behavior:**
  - Under `opentelemetry`, events go through `OpenTelemetryTracingBridge`, which has its own per-layer filter beneath the registry filter:
    - `development`: `debug,hyper=off,tonic=off,h2=off,reqwest=off,opentelemetry=off`
    - any other value: `info,hyper=off,tonic=off,h2=off,reqwest=off,opentelemetry=off`
  - Transport and SDK diagnostics never enter the log pipeline, even under a permissive `RUST_LOG`.
  - Records carry the event's message, fields, level and target; span fields are not carried.
- **Kind:** `guarantee`
- **Trigger:** Every event while a logger provider exists.
- **Source:**
  - `src/server.rs:818-832`, `:964-973`
  - `Cargo.toml:63`
- **Tests:** `src/server.rs` inline tests (as OBS-021).

### OBS-024 PII-safe instrumentation conventions (CI-enforced)

- **Behavior:** Production code follows three rules. The CI lint parses every `src/**/*.rs` and enforces them:
  1. Every one of the 305 `#[tracing::instrument]` attributes in `src/` uses `skip_all`, so no argument is auto-recorded through `Debug`.
  2. No `fields(...)` entry or event or span macro field uses a name on the denylist, in any of tracing's field syntaxes. Dotted paths are checked segment by segment, and macros nested in other macros (such as `select!`) are checked too. The denylist:
     - email-derived: `user_email`, `email`, `recipient_email`, `invitee_email`, `inviter_email`;
     - names: `user_name`, `recipient_name`, `inviter_name`, `display_name`, `full_name`;
     - secrets: `password`, `new_password`, `password_hash`, `access_token`, `refresh_token`, `oauth_token`, `provider_access_token`, `provider_refresh_token`, `auth_code`, `oauth_code`, `reset_code`, `verification_code`;
     - state and fingerprints: `received_state`, `expected_state`, `device_fingerprint`, `fingerprint`;
     - media and contact: `avatar_url`, `picture`, `phone`, `phone_number`;
     - network: `ip_address`, `remote_addr`, `client_ip`, `user_agent`.
  3. No instrument attribute uses `ret` or `err`.

  There is no runtime redaction layer. Safety depends on field names alone.
- **Kind:** `guarantee`
- **Trigger:** CI (`cargo test --test lint_tracing_instrument`); the effect is on every log and span.
- **Source:**
  - `tests/lint_tracing_instrument.rs:72-136`, `:221-285`
  - `documentation/LOGGING.md:45-59`
  - Production compliance: all 305 attributes in `src/` contain `skip_all`.
- **Tests:** `tests/lint_tracing_instrument.rs` (three gate tests plus self-tests)

### OBS-025 Structured-field conventions for every log and span

- **Behavior:**
  - Spans and events carry `component`, one of: `handler`, `service`, `repository`, `middleware`, `infrastructure`, `background_task`, `background_cleanup`.
  - They also carry `subsystem`, usually `operation`, and correlator IDs (`user_id`, `workspace_id`, `session_id`).
  - Instrument span names follow `handler_*` (65), `service_*` (99), `repo_*` (135), plus `github_*` and `google_*`.
  - Levels: 5xx and background failures `error!`; 4xx and degraded paths `warn!`; business outcomes `info!` (one per state-changing service call); infrastructure outcomes and flow `debug!`; per-item filtering `trace!`.
  - Metrics services, no-op services and repositories do not log, except the SSE listener's per-message warnings.
- **Kind:** `rule`
- **Trigger:** Code convention, visible in every emitted record.
- **Source:**
  - `documentation/LOGGING.md:61-223`
  - Examples: `src/services/telemetry_proxy_service.rs:300-311`, `src/handlers/telemetry.rs:62-72`, `src/background.rs:83-89`
- **Tests:** none found beyond the lint (OBS-024), which checks names, not presence.

### OBS-026 Canonical fail-open warning

- **Behavior:** Code that swallows an infrastructure failure and still returns success logs one `warn!` with these fields:
  - `component`, `subsystem` (when not inherited), optional `operation`;
  - `error_type=<tag>`;
  - `error=%e` when an error value exists;
  - `action="failing_open"`, a fixed literal.

  Examples: the rate-limit check, idempotency, the telemetry user budget (OBS-041), SSE event fetch and broadcast receive, and the password-reset email dispatch.
- **Kind:** `rule`
- **Trigger:** Any fail-open branch.
- **Source:**
  - `documentation/LOGGING.md:22-43`
  - `src/middleware/rate_limit.rs:298-313`
  - `src/services/telemetry_proxy_service.rs:265-277`
  - `src/services/sse_notification_service.rs:438-447`
  - `src/services/public_auth_service.rs:1977-1985`
- **Tests:** none found.

### OBS-027 HTTP RED metrics for every request except health and telemetry routes

- **Behavior:**
  - For every request whose raw path does not start with `/health` or `/v1/telemetry/`, the metrics middleware records `http_requests_total` +1 and observes `http_request_duration_seconds` (time to response headers).
  - Both carry the attributes `http.request.method`, `http.route` (template or `/<unmatched>`) and `http.response.status_code`, the last as a string.
  - It also logs `debug!` "HTTP metrics recorded via MetricsService".
- **Kind:** `capability`
- **Trigger:** Any HTTP request passing the global tier.
- **Effects:**
  - Rate-limited, auth-failed and panicked requests are counted. The middleware wraps all route layers and sits outside `catch_panic`.
  - Excluded paths still get spans and error logs.
- **Source:**
  - `src/middleware/metrics.rs:77-209`
  - `src/middleware/mod.rs:29`, `:41-44`
  - `src/services/otel_metrics_service.rs:191-220`
- **Tests:**
  - `tests/panic_response_http.rs` (records a panic as a 500)
  - `src/services/otel_metrics_service.rs::latency_histograms_export_seconds_boundaries`

### OBS-028 HTTP error counter for 4xx and 5xx

- **Behavior:** A 4xx adds 1 to `http_requests_errors_total` with `error.type="client_error"`; a 5xx adds 1 with `error.type="server_error"`. Both also carry `http.request.method`, `http.route` and `http.response.status_code`. Same path exclusions as OBS-027.
- **Kind:** `capability`
- **Trigger:** A response with status 400-599 on a counted path.
- **Source:**
  - `src/middleware/metrics.rs:184-194`
  - `src/services/otel_metrics_service.rs:222-235`
- **Tests:** `tests/panic_response_http.rs`

### OBS-029 Histograms use seconds-scale bucket boundaries

- **Behavior:**
  - `http_request_duration_seconds` buckets: `0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1, 2.5, 5, 7.5, 10` (unit `s`).
  - `email_send_duration_seconds` buckets: `0.1, 0.25, 0.5, 1, 2, 3, 5, 7.5, 10, 15, 30, 60` (unit `s`).
  - The latency alerts use thresholds that sit on these boundaries: 1 s and 5 s.
- **Kind:** `rule`
- **Trigger:** Metric service construction.
- **Source:** `src/services/otel_metrics_service.rs:9-20`, `:115-153`
- **Tests:** `src/services/otel_metrics_service.rs::latency_histograms_export_seconds_boundaries`

### OBS-030 `instance_up` liveness heartbeat

- **Behavior:**
  - Each process with a meter provider registers an observable gauge `instance_up`. It reports the constant 1 on every collection cycle, with no attributes beyond the resource.
  - Every live instance therefore has a series whether or not it serves traffic.
- **Kind:** `guarantee`
- **Trigger:** Each metric collection (every `OTEL_METRIC_EXPORT_INTERVAL_SECS`).
- **Effects:** It drives the `APIMetricsAbsent`, `APIInstanceDown` and `InstanceCountBelowThreshold` alerts and the dashboard's "Instance Up" panel.
- **Source:** `src/services/otel_metrics_service.rs:68-81`, `:155-172`
- **Tests:** `tests/prometheus/alert-rules.test.yml` (alert side only).

### OBS-031 `db_pool_connections` gauge

- **Behavior:**
  - With a meter provider, an observable gauge `db_pool_connections` reports three series each cycle: `state="total"` (`pool.size()`), `state="idle"` (`pool.num_idle()`), and `state="max"` (`DATABASE_MAX_CONNECTIONS`).
  - In use = total - idle; utilisation = (total - idle) / max.
- **Kind:** `capability`
- **Trigger:** Each metric collection.
- **Source:**
  - `src/utils/db_pool_metrics.rs:53-69`
  - `src/server.rs:1170-1186`
- **Tests:** `tests/prometheus/alert-rules.test.yml` (`DatabasePool*` expression tests).

### OBS-032 `sse_active_subscribers` gauge

- **Behavior:** With `SSE_NOTIFICATION_BACKEND=postgres` and a meter provider, an observable up-down counter `sse_active_subscribers` reports the shared broadcast sender's `receiver_count()` each cycle, with no attributes.
- **Kind:** `capability`
- **Trigger:** Each metric collection.
- **Source:**
  - `src/repositories/postgresql/sse_notification_repository.rs:197-209`
  - `src/server.rs:1263-1273`
- **Tests:** none found.

### OBS-033 Email delivery metrics

- **Behavior:** Around each SMTP `transport.send()`:
  - **Success:** `email_sent_total` +1 and `email_send_duration_seconds` observed, both with `email.type`.
  - **Failure:** `email_errors_total` +1 with `email.type` and `error.type`. `permanent` when the error `is_permanent()` or `is_tls()` (500 `EmailFailure`); `transient` otherwise (503 `SmtpTemporaryFailure`).

  Failures before the send, such as building the message or a bad recipient, record nothing. A failed send records no duration.

  `email.type` takes nine values: `password_reset`, `email_verification`, `workspace_invitation`, `password_reset_confirmation`, `password_change_confirmation`, `account_deletion_confirmation`, `email_change_verification`, `email_change_notice`, `email_change_confirmation`.
- **Kind:** `capability`
- **Trigger:** `SmtpEmailService::send_email` (MAIL).
- **Source:**
  - `src/services/email_service.rs:230-298`
  - `src/services/otel_metrics_service.rs:251-277`
  - `src/services/email_template_service.rs:316-635`
  - `src/services/user_service.rs:2244`, `:2258`
- **Tests:**
  - `src/services/email_service.rs::unreachable_smtp_host_yields_transient_service_unavailable` (classification)
  - `src/services/otel_metrics_service.rs` (buckets)

### OBS-034 Business event counter

- **Behavior:**
  - `business_events_total{event_type}` +1 per business event.
  - The only event emitted is `user_signup`, after a credentials or OAuth registration commits.
  - `MetricsService::meter()` exposes the meter for custom instruments, but nothing calls it.
- **Kind:** `capability`
- **Trigger:** User registration (AUTH).
- **Source:**
  - `src/services/otel_metrics_service.rs:237-249`
  - `src/services/public_auth_service.rs:587`, `:686`
  - `src/services/metrics_service.rs:110-124`
- **Tests:** `src/services/noop_metrics_service.rs::noop_service_creation` (no-op only).

### OBS-035 No-op metrics when no meter provider exists

- **Behavior:**
  - If no meter provider exists (the `local` backend, or the metrics exporter failed to build), `MetricsService` is `NoopMetricsService`: every `record_*` does nothing and `meter()` returns a no-op meter.
  - Callers always call the service; they never check whether it is the no-op.
  - Startup logs `info!` naming the metrics backend chosen: `backend="opentelemetry"` with export path, or `backend="local"`.
- **Kind:** `hardening`
- **Trigger:** Startup.
- **Source:**
  - `src/services/noop_metrics_service.rs:21-73`
  - `src/server.rs:1146-1168`
- **Tests:** `src/services/noop_metrics_service.rs::noop_service_creation`

### OBS-036 Telemetry proxy: forward one OTLP/HTTP batch to the collector

- **Behavior:**
  - `POST /v1/telemetry/traces`, `/metrics` and `/logs` take an OTLP/HTTP body.
  - The proxy posts it to `TELEMETRY_PROXY_COLLECTOR_ENDPOINT` (trailing slashes trimmed) plus `/v1/{signal}`. The body is decompressed if gzip and keeps the inbound `Content-Type`.
  - The collector's status code, body and `Content-Type` are relayed to the client unchanged. Without an upstream `Content-Type`, the response defaults to `application/octet-stream`.
- **Kind:** `capability`
- **Trigger:** `POST /v1/telemetry/{traces,metrics,logs}`
- **Auth:** Session (`user_scoped_auth_middleware`). The middleware stack and its policy rules are OBS-037.
- **Input & validation:** `Content-Type` is not checked; JSON or protobuf is up to the collector. Body limits are OBS-038.
- **Effects:**
  - Spans `handler_telemetry_proxy_{traces,metrics,logs}` (`user_id`, `body_bytes`) with child `service_telemetry_proxy_forward` (`user_id`, `signal`, `body_bytes`).
  - No HTTP metrics (OBS-027).
  - A relayed non-2xx collector status is logged by OBS-008 without error context: `warn!` for 4xx, `error!` and span ERROR for 5xx.
- **Errors:**
  - `SERVICE_UNAVAILABLE` → 503 when the collector send fails (unreachable, or past `TELEMETRY_PROXY_UPSTREAM_TIMEOUT_SECS`, default 10) or its response body cannot be read. Logged `error_code` is `dependency_unavailable`.
  - `INTERNAL_SERVER_ERROR` → 500 when the decompression task fails.
  - Byte-budget and size errors: OBS-038 to OBS-040.
- **Guarantees:**
  - `Authorization` is never forwarded: the upstream request is built from scratch.
  - `Content-Encoding` is never forwarded.
  - One `reqwest::Client` is shared, so connections are pooled.
- **Source:**
  - `src/handlers/telemetry.rs:59-185`
  - `src/services/telemetry_proxy_service.rs:235-241`, `:298-375`
  - `src/server.rs:413-433`
  - `src/handlers/mod.rs:321-327`
- **Tests:** `src/services/telemetry_proxy_service.rs` inline tests (`target_url_*`, `unreachable_collector_yields_service_unavailable`, `forward_inflates_gzip_and_passes_status_and_body_without_authorization`, `signal_display_matches_path_segment`)

### OBS-037 Telemetry proxy: session only, with no consent, verification or idempotency check

- **Behavior:**
  - The routes run `user_scoped_auth_middleware`, then `rate_limit_middleware`, then the handler.
  - The proxy does not check the user's stored `telemetry_consent`; it accepts any authenticated batch. Consent is enforced only by the client.
  - There is no email-verification gate and no idempotency.
  - A revoked session stops the flow at auth.
- **Kind:** `rule`
- **Trigger:** `POST /v1/telemetry/*`
- **Auth:** Session. Request-count tier: 600 requests per minute per user (`RATE_LIMIT_TELEMETRY_PROXY_PER_USER`), keyed `telemetry_proxy:user:<id>`, matched on the exact three paths (HTTP).
- **Errors:** 401 from auth; 429 from the request-count tier when `ENFORCE_RATE_LIMITS` is on (HTTP).
- **Source:**
  - `src/server.rs:669-687`
  - `src/handlers/telemetry.rs:25-41`
  - `src/middleware/rate_limit.rs:74-91`, `:175-183`
  - `documentation/TELEMETRY.md:114-118`
- **Tests:**
  - `src/middleware/rate_limit.rs::telemetry_paths_use_dedicated_bucket_and_limit`, `::telemetry_match_is_exact_path`
  - `tests/router_composition.rs`

### OBS-038 Telemetry proxy: body size and encoding limits

- **Behavior:**
  - The raw body is capped at `TELEMETRY_PROXY_MAX_BODY_BYTES` (default 1 MiB) by a per-route `DefaultBodyLimit`, which replaces the global 2 MB limit on these routes.
  - The body is then decoded off the async workers (`spawn_blocking`). `Content-Encoding` absent, empty or `identity` passes the body as is; `gzip` (any case, trimmed) is inflated with a multi-member decoder up to max+1 bytes; anything else is rejected.
  - The decoded size must not exceed the max.
- **Kind:** `rule`
- **Trigger:** `POST /v1/telemetry/*`
- **Input & validation:** See Errors.
- **Errors:**
  - 413 with axum's plain-text body (no envelope) when the raw body exceeds the route limit.
  - `PAYLOAD_TOO_LARGE` → 413 when the decoded body exceeds the max ("Telemetry batch exceeds N bytes after decompression").
  - `VALIDATION_ERROR` → 400 for a non-ASCII `Content-Encoding` or one other than gzip or identity (log code `invalid_header`).
  - `VALIDATION_ERROR` → 400 for a corrupt gzip stream (log code `invalid_request_body`).
- **Guarantees:** A small gzip bomb cannot inflate past max+1 bytes in memory.
- **Source:**
  - `src/services/telemetry_proxy_service.rs:150-198`, `:320-331`
  - `src/handlers/mod.rs:321-327`
- **Tests:** `src/services/telemetry_proxy_service.rs` (`decode_body_bounds_the_decoded_size`, `decode_body_rejects_unsupported_encodings_and_corrupt_gzip`)

### OBS-039 Telemetry proxy: per-user byte budget across instances

- **Behavior:**
  - After decoding, the decoded size, rounded up to whole KiB, is charged to the user's window under key `telemetry_proxy_bytes:user:<user_id>`.
  - The limit is `TELEMETRY_PROXY_USER_BYTES_PER_MINUTE / 1024` (floor) per 60 s, counted through `RateLimitService::check_quota` (the `rate_limits` table, shared by all instances).
  - Over the budget, the batch is refused and not forwarded.
  - If the budget store fails, the proxy logs the canonical fail-open `warn!` (`error_type="telemetry_budget_check_failed"`) and continues.
- **Kind:** `rule`
- **Trigger:** Each telemetry batch.
- **Errors:** `RATE_LIMIT_EXCEEDED` → 429 "Telemetry byte budget of N KiB per minute exceeded", with `Retry-After`.
- **Guarantees:**
  - The budget applies even when `ENFORCE_RATE_LIMITS=false`.
  - It is charged before the instance budget, so a user over their own budget never uses the instance's.
- **Source:**
  - `src/services/telemetry_proxy_service.rs:72-75`, `:243-279`, `:333-334`
  - `src/services/rate_limit_service.rs:187-232`, `:271-279`
- **Tests:** `src/services/telemetry_proxy_service.rs` (`user_budget_is_charged_decoded_kib_and_refuses_over_it`, `user_budget_check_failure_fails_open`)

### OBS-040 Telemetry proxy: per-instance byte token bucket

- **Behavior:**
  - Each API process holds an in-memory token bucket.
  - It refills at `TELEMETRY_PROXY_INSTANCE_BYTES_PER_SECOND` (default 16 KiB/s) up to a capacity of max(rate, `TELEMETRY_PROXY_MAX_BODY_BYTES`), and starts full.
  - A batch takes its decoded size from the bucket.
  - If the bucket is short, the batch is refused and not forwarded; refused bytes are not taken.
- **Kind:** `rule`
- **Trigger:** Each telemetry batch that passed OBS-039.
- **Errors:**
  - `RATE_LIMIT_EXCEEDED` → 429 "Telemetry ingest is at this instance's byte budget".
  - The envelope carries limit = rate / 1024, remaining = 0, and reset = now + ceil(wait), minimum 1 s.
- **Guarantees:**
  - First come, first served across all users of the instance.
  - A poisoned lock is recovered, not panicked on.
- **Source:** `src/services/telemetry_proxy_service.rs:113-148`, `:211-233`, `:281-295`, `:335`
- **Tests:** `src/services/telemetry_proxy_service.rs` (`instance_budget_refuses_then_refills`, `byte_budget_refills_at_rate_up_to_capacity`)

### OBS-041 Collector: server pipelines for the API's own telemetry

- **Behavior:**
  - **Receiver:** `otlp`, gRPC only, `0.0.0.0:4317`, max message 100 MiB. It receives the API's export.
  - **Pipelines:**
    - traces: `memory_limiter → tail_sampling → batch → resource`, then `otlp_grpc/tempo`;
    - metrics: `memory_limiter → batch → resource`, then `prometheus_remote_write`;
    - logs: `memory_limiter → batch → resource`, then `otlp_http/loki`.
  - **Resource processor:** upserts `deployment.environment` from the collector's own `ENVIRONMENT`, replacing the API's value, and sets `deployment.cluster="local"`.
  - **Exporters:**
    - Tempo and Loki go to tenant `fake` over plaintext.
    - Retries back off 5-30 s for up to 5 min.
    - The Tempo exporter has a 64-batch queue with 10 consumers.
    - Remote write turns every resource attribute into a label (`resource_constant_labels: ["*"]`) and adds `source="opentelemetry"`.
  - **Memory:** `memory_limiter` at 400 MiB, spike 100.
- **Kind:** `capability`
- **Trigger:** API export, while the `observability` profile runs.
- **Source:** `otel-collector-config.yml:7-13`, `:51-63`, `:118-125`, `:377-456`, `:468-489`
- **Tests:** `tests/telemetry/run.sh` (server logs, metrics and traces must land)

### OBS-042 Collector: tail sampling of server traces

- **Behavior:**
  - The collector holds each trace for 10 s (up to 10,000 traces, about 100 new per second expected), then keeps it when any of these holds:
    - **(1)** any span has ERROR status;
    - **(2)** the trace lasts longer than `OTEL_TAIL_LATENCY_THRESHOLD_MS` (500 by default);
    - **(3)** it falls in the `OTEL_TAIL_SAMPLING_PERCENTAGE` share (10 by default), and no span's `http.route` is one of `/v1/telemetry/traces`, `/v1/telemetry/metrics`, `/v1/telemetry/logs`, `/health/live`, `/health/ready`.
  - Client traces skip this processor.
- **Kind:** `rule`
- **Trigger:** Every server trace reaching the collector.
- **Guarantees:** The config assumes one collector. With several, a trace's spans would have to be routed to the same one.
- **Source:** `otel-collector-config.yml:72-116`
- **Tests:** `src/middleware/mod.rs::otel_collector_config_excludes_all_infrastructure_routes` (route list matches the Rust constants)

### OBS-043 Collector: client pipelines isolated from server ones

- **Behavior:**
  - **Receiver:** `otlp/client`, HTTP only, `0.0.0.0:4318`, `max_request_body_size` 1,048,576. It feeds only `traces/client`, `metrics/client` and `logs/client`.
  - **Processor order:** each client pipeline runs `memory_limiter/client` (250 MiB, spike 50, so it refuses from 200 MiB) → `filter/client` → `transform/client` first.
  - **Exporters:** client traces and logs go to Tempo and Loki tenant `client` through dedicated exporters with no queue and no retry and a 5 s timeout. The backend's verdict is the client's answer.
  - **Metrics:** client metrics go to `prometheus/client` (OBS-046).
  - **Sharing:** no limiter, exporter or tenant is shared with server pipelines.
- **Kind:** `guarantee`
- **Trigger:** A proxy-forwarded client batch.
- **Source:** `otel-collector-config.yml:15-23`, `:65-70`, `:391-405`, `:426-439`, `:491-508`
- **Tests:**
  - `src/services/telemetry_proxy_service.rs::otel_collector_config_isolates_client_telemetry`
  - `src/middleware/mod.rs::otel_collector_config_accepts_the_proxy_max_body`
  - `tests/telemetry/run.sh` and `battery.py`

### OBS-044 Collector: the client contract filter

- **Behavior:** `filter/client` (`error_mode: propagate`) drops:
  - any resource whose `service.name` is not `CLIENT_SERVICE_NAME` (default `app-desktop`), for traces, metrics and logs;
  - any metric that is neither:
    - a monotonic, unitless, delta sum named `http_client_requests_total`, `http_client_errors_total`, `http_client_retries_total` or `circuit_breaker_transitions_total`;
    - nor a delta histogram with unit `s` named `http_client_request_duration_seconds`;
  - any histogram point whose bounds are not exactly `[0.005 … 10]` (14 bounds, 15 buckets).

  Dropped data still returns 200 to the client.
- **Kind:** `rule`
- **Trigger:** A client batch.
- **Source:** `otel-collector-config.yml:134-159`
- **Tests:** `tests/telemetry/battery.py` (forged, malformed)

### OBS-045 Collector: the client contract transform

- **Behavior:** `transform/client` (`error_mode: propagate`):
  - **Traces:** keeps only resource `service.name`, `service.version` and `service.instance.id`.
  - **Metrics and logs:** keep only resource `service.name`. They blank the scope name, version and schema URL and delete scope attributes.
  - **Exemplars:** nulls every metric exemplar.
  - **Attributes kept per metric:**

    | Metric | Attributes kept |
    |---|---|
    | `http_client_requests_total` | method, route, status |
    | `http_client_request_duration_seconds` | method, route |
    | `http_client_errors_total` | method, route, `error.type` |
    | `http_client_retries_total` | route, `attempt` |
    | `circuit_breaker_transitions_total` | `from_state`, `to_state` |

  - **Values mapped to catch-alls:**
    - A method+route pair the API does not serve becomes `_OTHER` and `other`. The list holds 60 pairs; routes have no `/v1` prefix.
    - An unknown retry route becomes `other`.
    - A status outside `0, 200, 201, 202, 400, 401, 403, 404, 405, 409, 413, 415, 422, 429, 500, 502, 503, 504` becomes `other`.
    - An `error.type` outside the 12 listed values becomes `_OTHER`.
    - An `attempt` outside `0`-`9` and `10+` becomes `other`.
    - A breaker state outside `closed`, `open`, `half_open` becomes `other`.
- **Kind:** `rule`
- **Trigger:** A client batch.
- **Source:** `otel-collector-config.yml:161-353`
- **Tests:**
  - `src/middleware/mod.rs` (`otel_collector_config_client_routes_match_api_routes`, `otel_collector_config_strips_client_exemplars`, `otel_collector_config_client_series_fit_their_limits`: at most 3,641 series)
  - `tests/telemetry/battery.py`

### OBS-046 Collector: client metrics become fleet-wide sums

- **Behavior:**
  - Client metric points are batched (`batch/client_metrics`, 1 s), then re-stamped with the collector's arrival time (time = start = now).
  - They are summed across installs into cumulative streams (`delta_to_cumulative/client`, max 5,000 streams, stale after 1 h).
  - The totals are served for scraping on `:9464` (`prometheus/client`). A series stays for 1 h after its last point.
  - Labels on these series: `service.name`, `deployment.environment` and `deployment.cluster`. There is no per-install identity.
- **Kind:** `capability`
- **Trigger:** Client metric batches; Prometheus job `client-metrics` scrapes every 15 s with `sample_limit` 10,000.
- **Source:**
  - `otel-collector-config.yml:355-375`, `:458-466`
  - `prometheus.yml:47-59`
- **Tests:** `src/middleware/mod.rs::otel_collector_config_client_series_fit_their_limits`, `tests/telemetry/battery.py`

### OBS-047 Collector: Caddy lifecycle log, self-metrics and health

- **Behavior:**
  - **Caddy log:** the collector tails `/var/log/caddy/caddy.log` (the shared `caddy_logs` volume) from the end. It parses JSON lines, takes the timestamp from `ts` (epoch `s.ns`) and the severity from `level`, and moves `msg` to the body. It tags the stream `service.name=caddy` (insert) and sends it to Loki tenant `fake`.
  - **Caddy's own config:** Caddy writes lifecycle JSON (rolled at 50 MB, 5 files kept) and excludes access logs.
  - **Collector self-metrics:** level `detailed`, pulled on `:8888` without type or unit suffixes.
  - **Health:** `health_check` on `:13133`. Its own log level is info.
- **Kind:** `capability`
- **Trigger:** A Caddy log write; Prometheus scrape; health probe.
- **Source:**
  - `otel-collector-config.yml:25-49`, `:127-132`, `:486-489`, `:510-530`
  - `Caddyfile:12-22`
- **Tests:** `tests/telemetry/run.sh` (Caddy line lands)

### OBS-048 Prometheus: storage, scrapes and rules

- **Behavior:**
  - **Inputs:** Prometheus receives API metrics only by remote write (`--web.enable-remote-write-receiver`); it never scrapes the API.
  - **Scrape jobs** (all every 15 s, 10 s timeout):
    - `prometheus`
    - `otel-collector` (`:8888`)
    - `client-metrics` (`:9464`, `sample_limit` 10,000)
    - `caddy` (`:2020`)
    - `ingress-probe` (through blackbox-exporter, OBS-050)
    - `cadvisor` (with label `container_label_restartcount` dropped)
    - `alertmanager`
    - `loki`
    - `tempo`
  - **Evaluation:** every 15 s. Rule groups set a 30 s interval.
  - **Rules:** loaded from `prometheus-alerts.yml` and `generated/*.yml`. The latter holds `api:expected_instances = vector(API_REPLICAS)`, written by `deploy.sh`.
  - **External labels:** `deployment_environment` on outbound traffic only.
  - **Retention:** 30 days or 20 GB.
  - **Exemplars:** exemplar storage is on.
  - **Alerts:** sent to `alertmanager:9093`.
- **Kind:** `capability`
- **Trigger:** The `observability-backends` profile.
- **Source:**
  - `prometheus.yml:1-139`
  - `docker-compose.yml:258-290`
  - `scripts/fleet-rules.sh`
  - `scripts/deploy.sh:258-273`
- **Tests:** `.github/workflows/ci.yml:130-160` (every scrape job has a down alert)

### OBS-049 Alerting: rules, routing, inhibition and heartbeat

- **Behavior:**
  - Prometheus evaluates the alert rules (see the Alert catalog); Loki's ruler evaluates `TLSCertificateIssuanceFailing`.
  - **Routing:** Alertmanager groups by `alertname`, `severity` and `component` (wait 10 s, interval 5 m, repeat 4 h). Routes:

    | Match | Receiver | Repeat |
    |---|---|---|
    | `Watchdog` | `heartbeat` | wait 0 s, interval 1 m, repeat 1 m |
    | `AlertmanagerNotificationsFailing` | `warning-alerts`, then continues to its severity route | 1 h |
    | `severity=critical` | `critical-alerts` | 1 h |
    | `severity=warning` | `warning-alerts` | 12 h |
    | anything else, including `info` | `default-receiver` | default |

  - **Inhibition:** a critical silences warnings with the same `component` and `service_instance_id`. Among `otel-collector` alerts, a critical also silences warnings with the same `instance`.
  - **Receivers:** all four ship with no integration configured, so alerts are visible only in the Alertmanager UI. Credentials go in `alertmanager-secrets/` through `*_file` settings.
  - **Instance:** a single Alertmanager with no clustering.
- **Kind:** `capability`
- **Trigger:** Rule evaluation.
- **Source:**
  - `prometheus-alerts.yml`
  - `loki-rules/tls.rules.yml`
  - `alertmanager-config.yml:19-84`
  - `docker-compose.yml:312-333`
- **Tests:**
  - `tests/prometheus/alert-rules.test.yml`
  - CI `alert-rules` job routing check (`.github/workflows/ci.yml:162-193`)

### OBS-050 Ingress probe through Caddy

- **Behavior:**
  - Every 15 s, blackbox-exporter, module `http_200`, fetches `http://caddy:2021/health/ready`: GET, 8 s timeout, success on 200 only, no redirects, IPv4.
  - The `:2021` site proxies to the API through the same upstream as the public site.
  - The result is `probe_success`.
- **Kind:** `capability`
- **Trigger:** Prometheus scrape of `ingress-probe`.
- **Source:**
  - `blackbox-config.yml`
  - `prometheus.yml:72-93`
  - `Caddyfile:69-79`
  - `docker-compose.yml:295-310`
- **Tests:** `tests/prometheus/alert-rules.test.yml` (`IngressProbeFailing`)

### OBS-051 Loki: two tenants, retention and limits

- **Behavior:** Loki runs with `auth_enabled: true`; the `X-Scope-OrgID` header picks the tenant.

  | Tenant | Holds | Retention | Ingest | Streams |
  |---|---|---|---|---|
  | `fake` | API and Caddy logs | 720 h | 10 MB/s, burst 20 | 5,000 |
  | `client` (the defaults) | client logs | `CLIENT_TELEMETRY_RETENTION` (72 h) | `CLIENT_TELEMETRY_LOGS_RATE_MB` (8 KiB/s), burst 4 MB | 10 |

  - Structured metadata is allowed, which carries `trace_id` and `span_id`.
  - Storage is TSDB v13 on the filesystem with a WAL, flushed on shutdown.
  - The compactor enforces retention.
  - Multi-tenant queries are enabled.
  - The ruler reads `/etc/loki/rules/fake` and sends alerts to Alertmanager.
- **Kind:** `capability`
- **Trigger:** The `observability-backends` profile.
- **Source:**
  - `loki-config.yml:1-125`
  - `loki-overrides.yml:1-11`
  - `loki-rules/tls.rules.yml`
  - `docker-compose.yml:335-359`
- **Tests:** `tests/telemetry/battery.py` (tenant separation, floods refused, largest batch stored)

### OBS-052 Tempo: two tenants, retention and limits

- **Behavior:**
  - Tempo 3 runs single-binary with multitenancy on, receiving OTLP on gRPC 4317 and HTTP 4318.

    | Tenant | Ingest | Max per trace | Retention |
    |---|---|---|---|
    | `fake` (defaults) | 15 MB/s, burst 20 MB | 5 MB | 720 h |
    | `client` | `CLIENT_TELEMETRY_TRACES_RATE_BYTES` (8,192 B/s), burst 2 MiB, 1,000 live traces | 2 MiB | `CLIENT_TELEMETRY_RETENTION` (72 h) |

  - Storage is local; search returns 20 results by default and at most 100.
- **Kind:** `capability`
- **Trigger:** The `observability-backends` profile.
- **Source:**
  - `tempo-config.yml:1-73`
  - `tempo-overrides.yml:1-16`
  - `docker-compose.yml:361-406`
- **Tests:** `tests/telemetry/battery.py` (client trace floods refused)

### OBS-053 Grafana datasources and correlation links

- **Behavior:** Three datasources are provisioned:
  - **App Prometheus** (`app-prometheus`, the default): 15 s time interval, POST, exemplar links `trace_id` → `app-tempo`.
  - **App Loki** (`app-loki`): `X-Scope-OrgID: fake|client`, 5,000 max lines, no derived fields.
  - **App Tempo** (`app-tempo`): `X-Scope-OrgID: fake|client`.
    - `tracesToLogsV2` → `app-loki`, with tag `service.name` mapped to the `service_name` label, filtered by trace ID and not by span ID, with no time shift.
    - It also has a node graph, search, and span-bar filters on status, method and service.
- **Kind:** `capability`
- **Trigger:** Grafana startup.
- **Source:** `grafana/provisioning/datasources/{prometheus,loki,tempo}.yml`
- **Tests:** none found.

### OBS-054 Grafana dashboards

- **Behavior:**
  - Six dashboards are loaded from `grafana/provisioning/dashboards` and reloaded every 10 s. UI edits are allowed; deletion is not disabled.
  - Each dashboard links to the others.
  - Panels:

  | Dashboard | uid | Refresh / range | Variables | Panels |
  |---|---|---|---|---|
  | App API - Observability | `app-api-red` | 5 s / 1 h | `service_api`, `instance`, `slo_target`, `slo_window`, `search` | **RED:** request rate; 4xx and 5xx error rates; latency P50/P90/P99; status-code distribution.<br>**SLO:** availability gauge; error-budget gauge; 1 h and 6 h burn rate; 24 h availability trend.<br>**Logs and traces:** Loki error and warn volume; "Recent Traces" (TraceQL `trace:id =~ "$search.*"`); "Application Logs Stream" (`trace_id=~"(?i).*$search.*"`).<br>**Other:** active SSE subscribers; per-instance 5xx; `instance_up`; DB pool by state; container OOM, memory and CPU. |
  | App Desktop - Observability | `app-desktop-red` | 5 s / 1 h | `service_desktop`, `slo_target`, `slo_window`, `search` | **RED:** client request rate; errors by type; client latency P50/P90/P99.<br>**Resilience:** retries by route; breaker transitions.<br>**SLO:** client SLO gauges and burn rates (errors = network, timeout, server_error, rate_limit); error and warn log volume.<br>**Traces and logs:** "Recent Traces (Desktop → API)"; client log stream with the trace search. |
  | App Email - Observability | `app-email` | 30 s / 7 d | `service_api` | Cumulative sent by type; cumulative errors by type and class; SMTP latency P50/P95; Loki stream filtered `subsystem=~"email\|email_template"`. |
  | App Events | `app-events` | 30 s / 7 d | `service_api` | User signups (`business_events_total{event_type="user_signup"}`). |
  | App Load Balancer (Caddy) | `app-loadbalancer` | 10 s / 1 h | none | Request rate by code; 5xx ratio; P50/P99 latency; in-flight requests (all `handler!="metrics"`); Caddy lifecycle logs. |
  | OpenTelemetry Collector | `otel-collector` | 10 s / 1 h | none | Collector up; RSS; tail-sampling evictions, buffer and decisions by policy; receiver accepted and refused per signal; exporter sent and failed by exporter; queue saturation; processor drops by processor and signal. |

- **Kind:** `capability`
- **Trigger:** Grafana startup, then a reload every 10 s.
- **Source:** `grafana/provisioning/dashboards/*.json`, `grafana/provisioning/dashboards/dashboard.yml:1-15`
- **Tests:** none found.

### OBS-055 Grafana access guard

- **Behavior:**
  - Grafana refuses to start while `GRAFANA_ADMIN_PASSWORD` equals the `.env.example` placeholder (`replace-with-a-strong-password`): exit 1 with instructions. `entrypoint.sh check` runs only that guard; `deploy.sh` uses it.
  - Sign-up and anonymous access are off, and analytics and update checks are off.
  - Grafana is bound to `127.0.0.1:3050`, or published through a Caddy site. `GF_SERVER_ROOT_URL` comes from `CADDY_GRAFANA_SITE_ADDRESS`.
- **Kind:** `guarantee`
- **Trigger:** Grafana container start.
- **Source:**
  - `grafana/entrypoint.sh:14-29`
  - `docker-compose.yml:445-482`
  - `scripts/deploy.sh:311-317`
- **Tests:** none found (deploy tests not read).

### OBS-056 Switching the stack on and off

- **Behavior:** The stack is switched by two Compose profiles plus the backend flag. Shipped default: `COMPOSE_PROFILES=observability,observability-backends,…` with `OBSERVABILITY_BACKEND=opentelemetry`.
  - `observability`: the collector only.
  - `observability-backends`: cAdvisor, Prometheus, blackbox-exporter, Alertmanager, Loki, Tempo (with `tempo-init`) and Grafana.

  What happens in each combination:
  - **Both profiles dropped, `OBSERVABILITY_BACKEND=local`:** the "API only" stack (OBS-014). Trace IDs are still real. No alerts, no dashboards.
  - **`opentelemetry` with no collector reachable:**
    - The API starts and runs normally.
    - Every export fails in the background and logs to the console only, at SDK error level.
    - Each client batch through the proxy gets 503 and logs a server error. The proxy is built regardless of backend.
  - **Collector without `observability-backends`:**
    - The collector's exporters fail and retry for up to 5 min per batch, then drop it.
    - Nothing alerts, because Prometheus is gone.
- **Kind:** `capability`
- **Trigger:** Deploy configuration.
- **Source:**
  - `docker-compose.yml:239`, `:261`, `:298`, `:315`, `:338`, `:364`, `:394`, `:421`, `:448`
  - `.env.example:34-50`, `:1044-1055`
  - `src/server.rs:413-433`
  - `documentation/TELEMETRY.md:133-137`
- **Tests:** none found.

## Metric catalog

Metrics the API emits. All go out by OTLP push and Prometheus remote write, only when a meter provider exists (the `opentelemetry` backend and a successful metrics exporter build). Attribute dots become underscores in Prometheus labels.

Every series also carries these labels, from the resource:
- `service_name`, `service_version`, `service_instance_id`;
- `deployment_environment`, as the collector upserts it;
- `deployment_cluster="local"`;
- plus whatever the remote-write exporter derives itself (not verified).

| Name | Type | Unit | Attributes (Prometheus labels) | Recorded where | When |
|---|---|---|---|---|---|
| `http_requests_total` | Counter u64 | none | `http_request_method`, `http_route`, `http_response_status_code` | `src/middleware/metrics.rs:177` → `src/services/otel_metrics_service.rs:215` | Every request not under `/health*` or `/v1/telemetry/` |
| `http_request_duration_seconds` | Histogram f64 | s | same as above | `otel_metrics_service.rs:218` | same |
| `http_requests_errors_total` | Counter u64 | none | as above, plus `error_type` (`client_error`, `server_error`) | `metrics.rs:188-194` → `otel_metrics_service.rs:234` | 4xx or 5xx on a counted path |
| `business_events_total` | Counter u64 | none | `event_type` (only `user_signup`) | `src/services/public_auth_service.rs:587`, `:686` | After a registration commits |
| `email_sent_total` | Counter u64 | none | `email_type` (9 values, OBS-033) | `src/services/email_service.rs:260` | SMTP send OK |
| `email_send_duration_seconds` | Histogram f64 | s | `email_type` | `email_service.rs:260` | SMTP send OK only |
| `email_errors_total` | Counter u64 | none | `email_type`, `error_type` (`permanent`, `transient`) | `email_service.rs:292` | SMTP transport error |
| `instance_up` | Observable gauge u64 (always 1) | none | none beyond the resource | `otel_metrics_service.rs:162-172` | Every collection |
| `db_pool_connections` | Observable gauge u64 | none | `state` (`total`, `idle`, `max`) | `src/utils/db_pool_metrics.rs:53-69`, registered at `src/server.rs:1174-1186` | Every collection |
| `sse_active_subscribers` | Observable up-down counter i64 | none | none | `src/repositories/postgresql/sse_notification_repository.rs:197-209`, registered at `src/server.rs:1263-1273` | Every collection; only with SSE backend `postgres` |

Under `local`, or when the metrics exporter fails to build, the no-op service records nothing and none of the three observable gauges is registered (`src/services/noop_metrics_service.rs`, `src/server.rs:1146-1186`).

Metrics this repo does not emit but alerts or dashboards query:
- **Desktop client, admitted by the contract (OBS-044, OBS-045):**
  - `http_client_requests_total`, `http_client_errors_total`, `http_client_retries_total`, `circuit_breaker_transitions_total`, all scraped from collector `:9464`;
  - `http_client_request_duration_seconds_bucket`.
- **Caddy:** `caddy_http_requests_total`, `caddy_http_request_errors_total`, `caddy_http_request_duration_seconds_{count,bucket}`, `caddy_http_requests_in_flight`.
- **cAdvisor:** `container_oom_events_total`, `container_start_time_seconds`, `container_memory_working_set_bytes`, `container_cpu_usage_seconds_total`.
- **Blackbox and Prometheus:** `probe_success`, `probe_http_status_code` (runbook text only), `up`.
- **Collector self-metrics** (no suffixes):
  - processors: `otelcol_processor_tail_sampling_sampling_trace_dropped_too_early`, `…_sampling_traces_on_memory`, `…_count_traces_sampled`, `otelcol_processor_{incoming,outgoing}_items`;
  - process: `otelcol_process_memory_rss`;
  - receivers: `otelcol_receiver_{accepted,refused}_{spans,metric_points,log_records}`;
  - exporters: `otelcol_exporter_{sent,send_failed,enqueue_failed}_{spans,metric_points,log_records}`, `otelcol_exporter_queue_{size,capacity}`.
- **Alerting path:** `alertmanager_notifications_failed_total`, `prometheus_notifications_errors_total`, `loki_prometheus_notifications_errors_total`, `loki_ruler_managers_total`, `loki_prometheus_rule_evaluation_failures_total`, `prometheus_target_scrapes_exceeded_sample_limit_total` (runbook only).
- **Recording rule** `api:expected_instances` (generated, OBS-048).

These producers are outside this repo; their names were not checked against the producing software's versions.

## Alert catalog

Prometheus rules are in `prometheus-alerts.yml`, evaluated every 30 s. The Loki rule is in `loki-rules/tls.rules.yml`. "For" is how long the condition must hold before firing; "none" means it fires on the first evaluation.

| Alert (line) | Group | Condition (compact) | For | Severity | Component |
|---|---|---|---|---|---|
| `HighErrorRate` (16) | app_api_health | Fleet 5xx / all requests > 1% per `service_name` over 5 m, and at least 5 5xx in 5 m | 2 m | critical | api |
| `HighP99Latency` (56) | app_api_health | Per-instance P99 of `http_request_duration_seconds` > 1 s (5 m rate) | 5 m | warning | api |
| `APIMetricsAbsent` (89) | app_api_health | `absent(instance_up)` | 5 m | critical | api |
| `APIInstanceDown` (129) | app_api_health | An instance seen in the last 30 m that is not reporting now, unless the live count ≥ `api:expected_instances` | 2 m | warning | api |
| `InstanceCountBelowThreshold` (170) | app_api_health | Count of live `instance_up` < `api:expected_instances` | 5 m | critical | api |
| `PerInstanceHighErrorRate` (200) | app_api_health | Per-instance 5xx ratio > 5%, and at least 5 5xx in 5 m | 5 m | warning | api |
| `IngressProbeFailing` (242) | ingress_health | `probe_success{job="ingress-probe"} == 0`, unless Caddy's `up == 0` | 2 m | critical | caddy |
| `CaddyUpstreamErrors` (282) | ingress_health | Caddy request errors / requests > 1% (`handler!="metrics"`), and at least 5 errors in 5 m | 2 m | warning | caddy |
| `ClientHighErrorRate` (331) | app_desktop_health | Client errors of type network, timeout, server_error or rate_limit / client requests > 1% | 2 m | warning | desktop |
| `ClientHighP99Latency` (373) | app_desktop_health | Fleet P99 of `http_client_request_duration_seconds` > 2.5 s | 5 m | warning | desktop |
| `CircuitBreakerOpen` (407) | app_desktop_health | Increase in breaker transitions to open > 0 in 5 m, or a new to-open series after a successful scrape 5 m earlier | none | warning | desktop |
| `HighRetryRate` (451) | app_desktop_health | Client retries / client requests > 10% | 5 m | warning | desktop |
| `DesktopMetricsAbsent` (491) | app_desktop_health | `(sum(increase(http_client_requests_total[10m])) or vector(0)) == 0` | 5 m | warning | desktop |
| `HighEmailErrorRate` (533) | app_email_health | Email errors / (sent + errors) > 5% | 5 m | warning | email |
| `HighEmailSendLatency` (578) | app_email_health | Per-instance P99 of `email_send_duration_seconds` > 5 s | 5 m | warning | email |
| `TailSamplingTraceEvictionDetected` (620) | otel_collector_health | Rate of `…dropped_too_early` > 0 | 5 m | warning | otel-collector |
| `TailSamplingBufferNearCapacity` (662) | otel_collector_health | `…sampling_traces_on_memory > 8000` | 10 m | info | otel-collector |
| `OTelCollectorExportFailing` (702) | otel_collector_health | Any exporter not named `*client*` with send-failed or enqueue-failed spans, metric points or log records increasing over 5 m | 10 m | warning | otel-collector |
| `OTelCollectorInstanceDown` (736) | otel_collector_health | `up{job="otel-collector"} == 0` | 2 m | critical | otel-collector |
| `CaddyDown` (778) | scrape_target_health | `up{job="caddy"} == 0` | 2 m | critical | caddy |
| `MonitoringTargetDown` (802) | scrape_target_health | `up == 0` for job prometheus, cadvisor, alertmanager, loki, tempo or ingress-probe | 2 m | warning | monitoring |
| `ClientMetricsTargetDown` (831) | scrape_target_health | `up{job="client-metrics"} == 0` | 2 m | warning | otel-collector |
| `ContainerOOMKilled` (864) | container_infrastructure_health | `increase(container_oom_events_total{name!=""}[5m]) > 0` | 0 m | critical | infrastructure |
| `APIContainerRestarted` (909) | container_infrastructure_health | `changes(container_start_time_seconds{…service="api"}[15m]) > 0` per name | none | warning | api |
| `DatabasePoolNearExhaustion` (944) | database_health | (total - idle) / max > 0.8 per instance | 5 m | warning | database |
| `DatabasePoolExhausted` (1000) | database_health | (total - idle) / max > 0.95 per instance | 1 m | critical | database |
| `Watchdog` (1066) | alerting_path_health | `vector(1)`, always firing | none | none | alerting |
| `AlertmanagerNotificationsFailing` (1084) | alerting_path_health | Increase of `alertmanager_notifications_failed_total` > 0 in 15 m, by integration and reason | none | critical | alerting |
| `AlertmanagerUnreachable` (1106) | alerting_path_health | Prometheus or Loki notification errors increasing in 2 m | 5 m | critical | alerting |
| `LokiRulesNotLoaded` (1129) | alerting_path_health | `(sum(loki_ruler_managers_total) or vector(0)) == 0` | 5 m | warning | loki |
| `LokiRuleEvaluationFailing` (1149) | alerting_path_health | Increase of Loki rule evaluation failures > 0 per rule group in 5 m | 10 m | warning | loki |
| `TLSCertificateIssuanceFailing` (loki-rules line 10) | tls (Loki ruler, tenant `fake`) | Caddy logged "could not get certificate from issuer" for an identifier in 6 h 30 m, unless it then logged "certificate (obtained\|renewed) successfully" for the same identifier | none | warning | caddy |

All rules also set `alert_type`. Most annotations link to a `http://localhost:3050/d/<uid>` dashboard.

Not covered by `tests/prometheus/alert-rules.test.yml`:
- `Watchdog`, `AlertmanagerUnreachable`, `LokiRuleEvaluationFailing`;
- the Loki TLS rule, which has no promtool test at all.

## Data owned

- **No tables.** OBS owns no table, row-level security policy, stored procedure or trigger.
- **Trace ID columns:** written through `extract_or_invalid_in`, but owned elsewhere:
  - `workspace_audit_logs.trace_id` and `user_audit_logs.trace_id`, `VARCHAR(32) NOT NULL` (AUDIT, `migrations/0002_tables.sql:386`, `:410`);
  - `idempotency_cache.original_trace_id` (HTTP, `:471`);
  - `sse_events.trace_id`, nullable (RT, `:537`).
- **Rate-limit rows:** in the `rate_limits` table (HTTP/DATA) under keys `telemetry_proxy_bytes:user:<uuid>` (byte budget, KiB) and `telemetry_proxy:user:<uuid>` (request count).
- **In-memory state:**
  - the per-instance telemetry byte bucket;
  - the provider handles held for shutdown;
  - the three observable-gauge callbacks.
- **Background tasks:** none, beyond the OTel SDK's own batch and periodic export threads.
- **Docker volumes:** `prometheus_data`, `alertmanager_data`, `grafana_data`, `loki_data`, `tempo_data`, `caddy_logs` (shared with Caddy).

## Configuration

| Variable | Read by | Default | Validation / notes |
|---|---|---|---|
| `OBSERVABILITY_BACKEND` | API | `local` if unset; `.env.example` ships `opentelemetry` | `local` or `opentelemetry`, case-insensitive; any other value is fatal |
| `OTEL_ENDPOINT` | API | none; Compose injects `http://otel-collector:4317` | Required, non-empty, a URL with http or https scheme, when `opentelemetry` |
| `OTEL_SERVICE_NAME` | API | `app-api` | Non-empty when `opentelemetry`; also the meter and tracer scope name |
| `OTEL_SERVICE_VERSION` | API | `CARGO_PKG_VERSION` | Non-empty when `opentelemetry` |
| `ENVIRONMENT` | API, collector, Prometheus | `development` | Non-empty when `opentelemetry`. Exactly `development` turns on debug filters and dev CORS. The collector overrides `deployment.environment` with its own copy |
| `OTEL_TRACE_RATIO` | API | `1.0` | Finite, between 0.0 and 1.0, when `opentelemetry` |
| `OTEL_METRIC_EXPORT_INTERVAL_SECS` | API | `15` | 1-150 when `opentelemetry` |
| `RUST_LOG` | API | unset | Replaces the console filter registry-wide (OBS-021) |
| `OTEL_EXPORTER_OTLP_HEADERS` | OTel SDK | unset | Not parsed by app code |
| `SSL_CERT_FILE`, `SSL_CERT_DIR` | rustls-native-certs | unset | Replace the trust store for https OTLP |
| `TELEMETRY_PROXY_COLLECTOR_ENDPOINT` | API | `http://otel-collector:4318` | Non-empty; must start with `http://` or `https://` |
| `TELEMETRY_PROXY_MAX_BODY_BYTES` | API | `1048576` | Above 0 |
| `TELEMETRY_PROXY_USER_BYTES_PER_MINUTE` | API | `1048576` | At least the max body size |
| `TELEMETRY_PROXY_INSTANCE_BYTES_PER_SECOND` | API | `16384` | Above 0 |
| `TELEMETRY_PROXY_UPSTREAM_TIMEOUT_SECS` | API | `10` | 1-60 |
| `RATE_LIMIT_TELEMETRY_PROXY_PER_USER` | API (HTTP) | `600` | Above 0 |
| `DATABASE_MAX_CONNECTIONS` | API (DATA) | per DATA | Reported as `db_pool_connections{state="max"}` |
| `CLIENT_SERVICE_NAME` | collector | `app-desktop` | The only `service.name` accepted from clients |
| `OTEL_TAIL_SAMPLING_PERCENTAGE` | collector | `10` | Probabilistic share of normal traces |
| `OTEL_TAIL_LATENCY_THRESHOLD_MS` | collector | `500` | Slow-trace threshold |
| `CLIENT_TELEMETRY_LOGS_RATE_MB` | Loki | `0.0078125` | Client tenant ingest rate |
| `CLIENT_TELEMETRY_TRACES_RATE_BYTES` | Tempo | `8192` | Client tenant ingest rate |
| `CLIENT_TELEMETRY_RETENTION` | Loki, Tempo | `72h` | Client tenant retention |
| `GRAFANA_ADMIN_PASSWORD` | Grafana | placeholder | Compose requires it set; the entrypoint refuses the placeholder |
| `CADDY_GRAFANA_SITE_ADDRESS` | Grafana, Caddy | empty | Sets `GF_SERVER_ROOT_URL` and the public Grafana site |
| `API_REPLICAS` | `deploy.sh` | `1` | Becomes `api:expected_instances` |
| `COMPOSE_PROFILES` | Compose | all profiles | `observability` and `observability-backends` switch the stack (OBS-056) |

## Cross-domain dependencies

- **OBS calls into other domains:**
  - **HTTP:** `RateLimitService::check_quota` for the byte budget; `ApiError` and the envelope for proxy errors.
  - **DATA:** the `PgPool`, for the pool gauge.
  - **RT:** the SSE broadcast sender, for the subscriber gauge.
  - **AUTH:** `user_scoped_auth_middleware` on the proxy routes.
- **Other domains call into OBS:**
  - **Every domain:**
    - `extract_or_invalid_in`, through error contexts;
    - `MetricsService` (AUTH for `user_signup`, MAIL for email metrics);
    - the logging conventions and the PII lint.
  - **AUDIT and RT:** persist the trace ID.
  - **HTTP:** idempotency echoes `X-Original-Trace-Id`.
  - **OPS:**
    - `deploy.sh` generates the fleet rule, validates Alertmanager secrets and the Grafana password, and restarts the monitoring services when their configs change;
    - CI runs the `alert-rules` and `telemetry-gate` jobs and the lint.
- **Client:** the desktop client (outside this repo) produces client metrics, traces and logs under the contract, and is expected to honour `telemetry_consent` (USER).

## Observations

1. **Exporter init failure has no fallback.**
   - In `opentelemetry` mode, a tracer exporter that fails to build leaves no OTel layer and no `local` fallback, so every envelope gets the zero sentinel and every `extract_or_invalid_in` call logs an `error!` (`src/server.rs:959-962`, `src/utils/trace.rs:104-126`).
   - The three exporters share one endpoint and usually fail together (`tests/otlp_export_tls.rs:95-101`), and the server still starts.
   - Why it matters: the trace-ID guarantee in OBS-005 silently fails while the API looks healthy.
2. **Head sampling defeats tail sampling.**
   - The config and docs promise "100% critical trace retention" with 10% head sampling plus tail sampling (`src/config/observability.rs:96-104`, `.env.example:1124-1131`, `:1221`, `documentation/TELEMETRY.md:28`).
   - Head sampling at the SDK means an unsampled error trace is never exported, so tail sampling cannot keep it.
   - Why it matters: the documented guarantee holds only at `OTEL_TRACE_RATIO=1.0`.
3. **Default tail sampling drops most traces users report.**
   - 4xx responses do not set ERROR status (`src/middleware/logging.rs:474-493`), so they are kept only when slow or in the 10% share.
   - Why it matters: most trace IDs users report for 4xx errors resolve to no trace in Tempo by default. `.env.example:1181-1185` recommends 100 for development but ships 10.
4. **No outbound propagation.**
   - Both repositories register a propagator, with comments saying outbound HTTP "needs a well-formed `traceparent`" (`src/repositories/local_observability_repository.rs:120-124`).
   - No code injects context on outbound reqwest calls: OAuth, S3, the avatar fetch, the collector hop.
   - Why it matters: the claim is untrue.
5. **No link from logs to traces.**
   - The Loki datasource defines no `derivedFields` (`grafana/provisioning/datasources/loki.yml`).
   - The comment at `src/middleware/logging.rs:380-385` promises a "View Trace" button in Loki.
   - Why it matters: going from a log line to its trace needs a manual copy and paste.
6. **The exemplar path is dead.**
   - Prometheus has exemplar storage on and the datasource has an exemplar link, but the SDK emits no exemplars (`grafana/provisioning/datasources/prometheus.yml:17-21`, `docker-compose.yml:287`).
   - Why it matters: no metric spike can be followed to a trace.
7. **OTLP logs lack span fields.**
   - The bridge copies event fields only. The trace ID comes only from `tracing-opentelemetry`'s context activation; the appender's `experimental_use_tracing_span_context` feature is off (`Cargo.toml:63`). This was confirmed in the local 0.31 and 0.32 sources, not in the pinned 0.33 and 0.34.
   - `documentation/LOGGING.md:185` says events "inherit span context automatically". Several fail-open sites leave `component` and `subsystem` to the span, for example `src/services/sse_notification_service.rs:438-447`.
   - Why it matters: those events have no `component` or `subsystem` in Loki, and the email dashboard filters on `subsystem`.
8. **SSE delivery spans are unlinked root traces.**
   - They are created when the response stream is polled, outside the request span (`src/services/sse_notification_service.rs:235-284`).
   - Why it matters: a notification's delivery trace cannot be found from the publishing request's trace or from the subscriber's request trace.
9. **The client half of a trace is stored separately.**
   - The comment at `src/middleware/logging.rs:189` describes a single Tempo trace "Desktop span → API span".
   - In practice the client half is in tenant `client` and the API half in `fake`, and the API half may be tail-sampled away.
   - `documentation/TELEMETRY.md:72` says the Tempo datasource's `fake|client` header shows both halves; how Tempo merges a trace across tenants was not verified.
10. **Doc comments describe skip lists the logging middleware does not have.**
    - `src/middleware/mod.rs:19-24`, `:33-35` say both the logging and metrics middleware use `HEALTH_PATH_PREFIX` and `telemetry_path_prefix()`.
    - `src/utils/trace.rs:7-12` says those routes "only suppress the high-volume 2xx completion log".
    - Only metrics uses either prefix, and there is no 2xx log at all (`src/middleware/logging.rs:535-541`).
11. **The metrics skip is a prefix match.**
    - `starts_with("/health")` also skips `/healthz`, `/health-anything` and other unmatched paths (`src/middleware/metrics.rs:106`).
    - Why it matters: unmatched probes under that prefix are not counted in `/<unmatched>`.
12. **An alert comment is wrong.**
    - The `PerInstanceHighErrorRate` comment says `http_requests_total` "has no status-code label".
    - The code adds `http.response.status_code` (`src/services/otel_metrics_service.rs:208-212`), and the App API dashboard groups by it.
    - Location: `prometheus-alerts.yml:197-199`.
13. **A metric comment undercounts email types.**
    - It says "4 types" (`src/services/otel_metrics_service.rs:136`); there are 9 `email_type` values.
14. **Email failures before the send are not counted.**
    - Message-build or recipient failures (500 `EmailFailure`) record no email metric (`src/services/email_service.rs:150-217`).
    - Why it matters: `HighEmailErrorRate` misses them.
15. **Business-event examples promise events that do not exist.**
    - Doc examples cite `workspace_created` (`src/services/metrics_service.rs:72-78`), but only `user_signup` is emitted.
    - `MetricsService::meter()` is never called.
16. **Baggage attributes are spoofable.**
    - `user_id` and `workspace_id` on the request span come from client baggage, unchecked (`src/middleware/logging.rs:357-370`).
    - Why it matters: a TraceQL search by `span.user_id` returns client-asserted values, not the authenticated user.
17. **CORS blocks browser trace propagation.**
    - CORS allows only `Authorization`, `Content-Type` and `Accept`, and exposes no headers (`src/middleware/cors.rs:47`).
    - Why it matters: a browser-origin client cannot send `traceparent` or `baggage` (or `X-Workspace-Id`, `Idempotency-Key`, `Content-Encoding`), nor read `X-Original-Trace-Id` or the rate-limit headers. The Tauri client may not be affected; this was not verified.
18. **The proxy's 429 headers mix two counters.**
    - A byte-budget 429 sets `X-RateLimit-*` in KiB and a `Retry-After`. The outer `rate_limit_middleware` then overwrites `X-RateLimit-Limit`, `X-RateLimit-Remaining` and `X-RateLimit-Reset` with request-count values (`src/middleware/rate_limit.rs:286-292`).
    - Why it matters: the response carries a byte-based `Retry-After` next to request-count limit headers.
19. **Refused bytes still count against the user's minute.**
    - Each charge increments the counter even when it is refused (`src/services/rate_limit_service.rs:197-204`).
    - The user budget is charged before the instance budget (`src/services/telemetry_proxy_service.rs:333-335`), so a batch refused at the instance still spends the user's budget.
20. **One 413 has no envelope.**
    - A raw body over the route limit gets axum's plain-text 413, while the decoded-size 413 uses the JSON envelope (`src/handlers/mod.rs:326`, `src/services/telemetry_proxy_service.rs:157-161`).
21. **Collector refusals produce server error logs and ERROR traces.**
    - The proxy relays collector 5xx statuses (for example the client memory limiter's 503). Logging treats them as server errors: `error!` plus ERROR span status, which the tail sampler keeps (`src/handlers/telemetry.rs:180-184`, `src/middleware/logging.rs:501-514`).
    - Telemetry routes are excluded from HTTP metrics, so these 5xx appear in logs and traces but not in `HighErrorRate`.
22. **The proxy ignores consent.**
    - The proxy never checks `AuthContext.telemetry_consent` (`src/handlers/telemetry.rs:157-170`); `documentation/TELEMETRY.md:118` says so explicitly.
23. **Without `deploy.sh`, `api:expected_instances` does not exist.**
    - The generated rules directory ships empty (`generated/prometheus/.gitignore`).
    - Under a plain `docker compose up`:
      - `InstanceCountBelowThreshold` can never fire;
      - `APIInstanceDown`'s rolling-deploy suppression does nothing, so it fires on every rollover (`prometheus-alerts.yml:139-141`, `:176`).
24. **The collector overrides the environment label.**
    - Its `resource` processor upserts `deployment.environment` from its own `ENVIRONMENT`, replacing the API's resource value (`otel-collector-config.yml:118-125`).
    - Why it matters: the two can differ when set separately.
25. **Server and client route labels differ.**
    - Server `http_route` labels include the `/v1` nest prefix (the tail-sampling list uses `/v1/telemetry/...`).
    - The client contract's route list has no prefix (`otel-collector-config.yml:199-319`).
    - Why it matters: joining server and client series by route needs relabelling.
26. **The PII lint can be bypassed by naming.**
    - It is name-based. `rate_limit_key = %rate_limit_key` logs `ip:<client address>` on public routes (`src/middleware/rate_limit.rs:234`, `:272`, `:306`), while `client_ip` is on the denylist.
    - `error_message` logs internal error text verbatim, such as `SMTP delivery failed: {e}` (`src/services/email_service.rs:277`, `src/middleware/logging.rs:462`). SMTP server replies commonly echo the recipient address.
    - `documentation/LOGGING.md:55-57` also names `code` and `file_name` as forbidden, but neither is on the lint's denylist (`tests/lint_tracing_instrument.rs:75-118`).
27. **The log filter also governs spans.**
    - The registry-wide `EnvFilter` sits above the trace layer (`src/server.rs:976-992`).
    - Why it matters: a `RUST_LOG` that disables `app_api` INFO also removes the INFO-level request and instrument spans, which turns every trace ID into the sentinel.
28. **A stale error message.**
    - The `expect` text at `src/middleware/idempotency.rs:767` mentions a "UUID fallback", which `src/utils/trace.rs:32-35` says was removed.
29. **A doc names a profile wrongly.**
    - The `README.md:51` line for `observability` lists "Grafana / Prometheus / Loki / Tempo / Alertmanager" under that profile.
    - Those run under `observability-backends`; the `observability` profile is the collector alone (`docker-compose.yml:421`).
30. **The comment about OTLP headers is incomplete.**
    - `src/repositories/otlp_observability_repository.rs` sets no headers; `OTEL_EXPORTER_OTLP_HEADERS` works only because the SDK reads the environment itself (`.env.example:1077-1083`).
    - Why it matters: a rewrite on a different SDK could lose vendor authentication silently.
31. **Comments disagree with each other.**
    - Semantic-convention version citations disagree (v1.27.0, v1.28.0 and v1.38.0) within `src/middleware/logging.rs:196`, `:267`, `:274`.
    - `src/middleware/metrics.rs:16-20` says it follows "the same fail-open pattern if service is None", but there is no `Option` (lines 162-168).

## Open questions

1. **Trace retention policy.** Should every trace a user can report be retrievable: every 4xx, and every trace when the head ratio is below 1? Or are the current 10% and the head ratio intended trade-offs for the template?
2. **Exporter failure at startup.** Should a failed OTLP exporter build in `opentelemetry` mode fail startup, fall back to `local` tracing, or keep today's degraded start with sentinel trace IDs?
3. **Trace ID header.** Should the trace ID also be returned in a response header (for example `traceresponse`, CORS-exposed)? Today raw and streaming responses carry none, and replays expose only the original.
4. **Grafana correlation links.** Should Loki derived fields and exemplars be part of the template? Without them "log → trace" and "metric → trace" do not work.
5. **SSE delivery linking.** Should SSE delivery spans link to, or be parented by, the publishing trace ID stored in `sse_events`?
6. **Background job trace IDs.** Should background job traces surface their trace IDs anywhere, such as on errors or in logs?
7. **Outbound propagation.** Is the missing outbound `traceparent` deliberate (for privacy towards OAuth providers and S3), or an oversight?
8. **Baggage attributes.** Should baggage `user_id` and `workspace_id` remain unauthenticated span attributes, or should the authenticated `AuthContext` IDs be recorded instead?
9. **Proxy enforcement of consent.** Should the proxy enforce `telemetry_consent` server-side, given consent is stored and loaded into `AuthContext` on every request?
10. **Telemetry 5xx in HTTP metrics.** Should relayed collector 5xx count as API server errors, or be excluded from error logging and span status the way they are from metrics?
11. **Log redaction.** Is name-based PII protection enough, or should the rewrite redact `error_message` values and the IP inside `rate_limit_key`?
12. **Client and server trace merging.** Does Tempo actually merge a trace split across tenants `fake` and `client` in one view? This needs a live check; the code cannot answer it.
13. **Bypassing `deploy.sh`.** Should the fleet-size alerts work without `deploy.sh`, for a plain `docker compose up`?
