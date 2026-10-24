# Observability rules (`OBS`)

Logs, traces, metrics, dashboards and alerts. The design is built from the operator questions in D19. The guarantee everything serves is D10: any trace ID the API returns leads to that request's logs, and to its trace whenever the request ended in an error.

## Logs

### OBS-01 Logs are structured
Logs are JSON in production and human-readable in development. They go to stdout, and also over the OpenTelemetry Protocol (OTLP) when telemetry is enabled. A log message is a constant string, and all variable data goes in fields.
- Why: logs can be queried by field, and messages can be grouped.
- Check: `ci: check-instrument` (a message with format arguments or `{…}` placeholders fails), plus `review`.

### OBS-02 Every log line can be correlated
Every JSON log line carries `service.name`, `service.version` and `deployment.environment.name`, the last once the configuration has named the environment. A line written inside a span also carries `trace_id` and `span_id`. The readable text of development carries the fields its events write.
- Why: a log line leads to its trace, and a trace leads to its logs (D10). The lines that reach a log store are JSON (OBS-01).
- Check: `test` (the binary's JSON lines, and the logging setup in the end-to-end suite).

### OBS-03 Levels have fixed meanings
| Level | Meaning |
|---|---|
| `ERROR` | A human must look: a 5xx, a job that failed its last attempt, a startup failure |
| `WARN` | Degraded but handled: an ignored error (ERR-11), a job attempt that will be retried |
| `INFO` | Lifecycle events, plus exactly one completion line per request (method, route, status, duration) |
| `DEBUG` | Diagnostics, off in production |

Module code writes events only at `DEBUG`. Every `INFO`, `WARN` and `ERROR` event comes from `platform` or the binary's `main.rs`. A business event that matters is an audit entry (DATA-27), not a log line.
- Why: a level tells the reader what to do, and every request has at least one log line (D10). When the mechanisms write every event above `DEBUG`, each kind of event is written once, in one shape.
- Check: `ci: check-instrument` (the levels of events in `app`'s code outside `main.rs`), plus `review`.

### OBS-04 No personal data or secrets in telemetry
Logs, spans and metric labels never contain personal data or secrets. The `EmailAddress` and `Secret` types redact themselves when formatted. Every `#[instrument]` uses `skip_all` and lists its fields explicitly.
- Why: telemetry is copied widely and kept for a long time (D7).
- Check: `compiler` (the redacting types), `ci: check-instrument` (every `#[instrument]` has `skip_all`), plus `review`.

### OBS-05 Telemetry names come from one list
Every field name in logs and spans, and every metric label name, comes from the list in `platform::telemetry`, and code uses no other name. The list declares no field for personal data or secrets (OBS-04).
- A name is OpenTelemetry's wherever a semantic convention names the same thing, such as `http.request.method` or `http.route`. The list pins the conventions' version.
- Any other name is `<term>.<property>` in lowercase, using the glossary's term for the concept (CODE-10), and never starts with a namespace the conventions define. Where the conventions use the term with another meaning, the owning module's name goes first: `auth.session.id`, since OpenTelemetry's `session` is a client application's session, and `realtime.event.id`, since its `event` names log records.
- `trace_id` and `span_id` are top-level fields, as OpenTelemetry names trace context in JSON logs.
- `otel.name` and `otel.kind`, which the bridge from `tracing` to OpenTelemetry reads to name a span and set its kind, are the bridge's own fields, and the only names in a namespace the conventions define. No JSON log line carries them; the readable text of development shows them with the span they belong to.
- Why: standard names work with standard dashboards and tools, and one list keeps each name the same in every log, span and metric (D19).
- Check: `compiler` (each convention's entry is checked against the conventions' published name), `clippy` (a deprecated convention name fails), plus `ci: check-instrument` (every name the code uses is on the list).

## Traces

### OBS-06 One server span per request, child spans for work
Each request has one server span. Database transactions, outbound calls and job runs get child spans. Spans come only from `platform` and the binary's `main.rs`; module code opens none. CPU-heavy work runs through the `platform` helper that adds its span (CODE-05), so slow steps such as password hashing still show in traces.
- Why: a trace shows where a request spent its time, and every span has the same shape wherever it comes from.
- Check: `ci: check-instrument` (no spans in `app`'s code outside `main.rs`), plus `review`.

### OBS-07 Trace context carries across work that happens later
Inbound W3C `traceparent` from clients is accepted. Every job and event row stores the trace context that created it. The span for running that job or delivering that event links to the stored context. Outbound requests send trace context only to our own infrastructure, never to third parties.
- Why: background work leads back to the request that caused it (D10).
- Check: `test`.

### OBS-08 Errors and slow requests are always kept
The SDK samples every trace except the health probes (OBS-15). The collector's tail sampling keeps every trace with `http.response.status_code >= 400` or an error status, every trace slower than the latency objective, and a configured share of the rest. Logs are never sampled. The dashboard of request latency lists kept traces slower than the objective in the time range it shows, as many as its limit holds, each leading to its trace.
- Why: every trace ID a user can report as a problem resolves to a trace, a latency spike leads to the requests behind it, and every request's logs are kept (D10).
- Check: `ci: check-telemetry-config` (the sampling policies in the collector's configuration, and the same objective in them, the alerts and the dashboard), plus `test` (end to end).

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
Every metric is declared once, as a `static` of one of the declaration types in `platform::telemetry::metrics`, with its name, unit, description and label names, and a histogram's bucket boundaries. Each module declares its metrics in `metrics.rs`, and `platform` declares its own in the area that records them. A name is OpenTelemetry's wherever a semantic convention defines the metric, checked against the conventions' published name as OBS-05's are. Any other name is `<module>.<name>`: the declaring module's name, `platform` for its own, then what it counts or measures in `snake_case`, such as `platform.panics`. Label names are the constants of the list in `platform::telemetry` (OBS-05), and a recording passes one value for each, in order. Labels come from bounded sets and never include IDs or personal data.
- Why: every metric is known, documented and cheap to store, and an own name can't pass for one of the conventions' (D19). The conventions use `app.` for client applications.
- Check: `compiler` (the declaration types, and the conventions' names), `clippy` (getting an OpenTelemetry meter outside the declaration types), plus `ci: check-metrics` (own names, and label names from the list).

### OBS-12 The metric catalog is generated
`docs/operations/metrics.md` is generated from the declarations, and from `deploy/observability/metrics.yaml`, which lists the metrics that other parts of the stack produce, such as the collector's own. CI fails if the catalog is out of date, if a dashboard or alert uses a metric that neither declares, or if a metric either declares isn't used by any dashboard or alert.
- Why: dashboards, alerts and code can't drift apart (D19).
- Check: `ci: check-metrics`.

## Dashboards and alerts

### OBS-13 Dashboards answer named questions
Dashboards are provisioned from `deploy/observability/grafana/` and read-only in Grafana. One overview dashboard links to one dashboard per area. Each panel's title is the question it answers.
- Why: a dashboard is read under pressure, and must say what it shows (D19).
- Check: `ci: check-metrics` (queries, titles that end in a question mark, read-only provisioning, and an overview that links to every other dashboard), plus `review` (what each title asks, and one dashboard per area).

### OBS-14 Alerts fire on symptoms and have runbooks
Alerts are of two kinds:
- Burn-rate alerts against the service-level objectives for availability and latency.
- Cause alerts for a dependency that is down, dead-lettered jobs, event delivery lag, and telemetry that is dropped or that a service of the stack can't receive because it is down.

Every alert has a severity (`page` or `ticket`), a runbook section in `docs/operations/runbooks.md` and a `promtool` test.
- Why: an alert means action, and the action is written down (D19).
- Check: `ci: alert-rules` (each rule has a test and a runbook anchor).

### OBS-15 Health probes stay out of telemetry
Liveness and readiness requests are excluded from request metrics, are not sampled into traces, and carry no trace ID (API-16).
- Why: probes would drown out the signal.
- Check: `test`.
