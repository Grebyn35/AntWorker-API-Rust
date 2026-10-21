# Observability rules (`OBS`)

Logs, traces, metrics, dashboards and alerts. The design is built from the operator questions in D19. The guarantee everything serves is D10: any trace ID the API returns leads to that request's logs, and to its trace whenever the request ended in an error.

## Logs

### OBS-01 Logs are structured
Logs are JSON in production and human-readable in development. They go to stdout, and also to OTLP when telemetry is enabled. A log message is a constant string, and all variable data goes in fields.
- Why: logs can be queried by field, and messages can be grouped.
- Check: `review`.

### OBS-02 Every log line can be correlated
Every log line carries `service.name`, `service.version` and `deployment.environment`. A line written inside a span also carries `trace_id` and `span_id`.
- Why: a log line leads to its trace, and a trace leads to its logs (D10).
- Check: `test` (the logging setup is asserted in the end-to-end suite).

### OBS-03 Levels have fixed meanings
| Level | Meaning |
|---|---|
| `ERROR` | A human must look: a 5xx, a job that failed its last attempt, a startup failure |
| `WARN` | Degraded but handled: an ignored error (ERR-11), a job attempt that will be retried |
| `INFO` | Lifecycle events, plus exactly one completion line per request (method, route, status, duration) |
| `DEBUG` | Diagnostics, off in production |
- Why: a level tells the reader what to do, and every request has at least one log line (D10).
- Check: `review`.

### OBS-04 No personal data or secrets in telemetry
Logs, spans and metric labels never contain personal data or secrets. The `Email` and `Secret` types redact themselves when formatted. Every `#[instrument]` uses `skip_all` and lists its fields explicitly.
- Why: telemetry is copied widely and kept for a long time (D7).
- Check: `compiler` (the redacting types), `ci: check-instrument` (every `#[instrument]` has `skip_all`), plus `review`.

### OBS-05 Field names follow OpenTelemetry semantic conventions
Where a semantic convention exists, logs and spans use its attribute name, for example `http.request.method` or `http.route`. The convention version is pinned in `platform::telemetry`.
- Why: standard names work with standard dashboards and tools.
- Check: `review`.

## Traces

### OBS-06 One server span per request, child spans for work
Each request has one server span. Database transactions, outbound calls and job runs get child spans.
- Why: a trace shows where a request spent its time.
- Check: `review`.

### OBS-07 Trace context carries across work that happens later
Inbound W3C `traceparent` from clients is accepted. Every job and event row stores the trace context that created it. The span for running that job or delivering that event links to the stored context. Outbound requests send trace context only to our own infrastructure, never to third parties.
- Why: background work leads back to the request that caused it (D10).
- Check: `test`.

### OBS-08 Errors are always kept
The SDK samples every trace except the health probes (OBS-15). The collector's tail sampling keeps every trace with `http.response.status_code >= 400` or an error status, plus a configured share of the rest.
Logs are never sampled. Latency histograms record exemplars that link to their traces.
- Why: every trace ID a user can report as a problem resolves to a trace, and every request's logs are kept (D10).
- Check: `ci: check-telemetry-config` (the sampling policy in the collector config), plus `test` (end to end).

### OBS-09 Identity on spans comes from the server
User and workspace attributes on spans come from `Principal` and `Authorized<P>`, never from client-supplied baggage.
- Why: trace searches by user return the truth.
- Check: `review`.

### OBS-10 Telemetry setup fails loudly
With telemetry enabled, startup fails if an exporter can't be built. Export failures at runtime are counted, never fatal.
- Why: no silent fallback to all-zero trace IDs (D10).
- Check: `test`.

## Metrics

### OBS-11 Metrics are declared, named and bounded
Each module declares its metrics in `metrics.rs`, and `platform` declares its own in the area that records them, all with the one declaration macro: name, type, unit, labels and description. Names follow OpenTelemetry semantic conventions where one exists, and `app.<module>.<name>` otherwise, with `platform` as the module name for its own metrics. Labels come from bounded sets and never include IDs or personal data.
- Why: every metric is known, documented and cheap to store (D19).
- Check: `compiler` (the macro), plus `ci: check-metrics`.

### OBS-12 The metric catalog is generated
`docs/operations/metrics.md` is generated from the declarations. CI fails if it is out of date, if a dashboard or alert uses a metric that isn't declared, or if a declared metric isn't used anywhere.
- Why: dashboards, alerts and code can't drift apart (D19).
- Check: `ci: check-metrics`.

## Dashboards and alerts

### OBS-13 Dashboards answer named questions
Dashboards are provisioned from `deploy/observability/grafana/` and read-only in Grafana. One overview dashboard links to one dashboard per area. Each panel's title is the question it answers.
- Why: a dashboard is read under pressure, and must say what it shows (D19).
- Check: `ci: check-metrics` (queries), plus `review` (titles).

### OBS-14 Alerts fire on symptoms and have runbooks
Alerts are of two kinds:
- Burn-rate alerts against the service-level objectives for availability and latency.
- Cause alerts for a dependency that is down, dead-lettered jobs, event delivery lag and dropped telemetry.

Every alert has a severity (`page` or `ticket`), a runbook section in `docs/operations/runbooks.md` and a `promtool` test.
- Why: an alert means action, and the action is written down (D19).
- Check: `ci: alert-rules` (each rule has a test and a runbook anchor).

### OBS-15 Health probes stay out of telemetry
Liveness and readiness requests are excluded from request metrics, are not sampled into traces, and carry no trace ID (API-16).
- Why: probes would drown out the signal.
- Check: `test`.
