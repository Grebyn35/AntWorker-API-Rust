# Observability rules (`OBS`)

Logs, traces, metrics, dashboards and alerts. The design is built from the operator questions in D19. The guarantee everything serves is D10: any trace ID the API returns leads to that request's logs, and to its trace whenever the request ended in an error, but for a failure that came after the collector's wait (OBS-08).

## Logs

### OBS-01 Logs are structured
Logs are JSON in production and human-readable in development. They go to stdout, and also over the OpenTelemetry Protocol (OTLP) when telemetry is enabled. A log message is a constant string, and all variable data goes in fields.
- Why: logs can be queried by field, and messages can be grouped.
- Check: `ci: check-instrument` (a message with format arguments or `{…}` placeholders fails), plus `review`.

### OBS-02 Every log line can be correlated
Every JSON log line carries its time, `timestamp`, in UTC, and `service.name`, `service.version` and `deployment.environment.name`, the last once the configuration has named the environment. A line written inside a span also carries `trace_id` and `span_id`. The readable text of development carries the fields its events write.
- Why: a log line leads to its trace, where the collector kept it (OBS-08), and a trace leads to its logs (D10). The lines that reach a log store are JSON (OBS-01).
- Check: `test` (the binary's JSON lines), plus `ci: e2e` (each span and log line of the proxy that reaches a backend names its service's version).

### OBS-03 Levels have fixed meanings
| Level | Meaning |
|---|---|
| `ERROR` | A human must look: a 5xx other than a disabled feature's (OPS-04) or a health probe's (OPS-05), a job that failed its last attempt, a startup failure |
| `WARN` | Degraded but handled: an ignored error (ERR-11), a job attempt that will be retried |
| `INFO` | Lifecycle events, plus exactly one completion line per request (method, route, status, duration), with `error.type` in place of the status for one that ends before any response (PLATFORM-044), but for the health probes (OBS-15) and a request that the HTTP server refuses itself (PLATFORM-099) |
| `DEBUG` | Diagnostics, off in production |

Module code writes events only at `DEBUG`. Every `INFO`, `WARN` and `ERROR` event comes from `platform` or the binary's `main.rs`. A business event that matters is an audit entry (DATA-22), not a log line.
- Why: a level tells the reader what to do, and every request but a health probe and one that the HTTP server refuses itself has at least one log line (D10). When the mechanisms write every event above `DEBUG`, each kind of event is written once, in one shape.
- Check: `ci: check-instrument` (the levels of events in `app`'s code outside `main.rs`), plus `review`.

### OBS-04 No personal data or secrets in telemetry
Logs, spans and metric labels never contain personal data or secrets. `EmailAddress` redacts itself when formatted, as `Secret` does (SEC-01), and a type that holds a client's IP address or user agent either redacts its `Debug`, as `platform::types::Client` does, or has none, as an audit entry's types have none. Every `#[instrument]` uses `skip_all` and lists its fields explicitly. The reference deployment's proxy writes no field of a log line that holds personal data, and the collector keeps only the attributes of its spans whose names `platform::telemetry` lists (D53). The deployment's bundled Postgres writes an error to its log without the error's detail, which holds the values that a constraint refused, such as an address that another account holds.
A file's name is personal data: `FileName` redacts it in its `Debug`, and `files`' records of an export's files, `ExportedFile`, `OwnRecord` and `WorkspaceRecord`, whose paths hold names, have no `Debug`. The `Debug` of an upload's body, `Upload`, prints neither its `Content-Disposition` nor its bytes, and that of `Uploaded`, the bytes it read, prints how many they are. <!-- part: files -->
Of the telemetry that a user's client sends through the telemetry proxy, the collector keeps of the resource of a span or of a log record only what names the client, its version and, on spans, its installation (CLIENT_TELEMETRY-011), and of its metrics only the client's name and the attributes and values that its configuration lists (CLIENT_TELEMETRY-015); what its spans and its log records hold besides is the client's to keep free of personal data, and its user's to consent to (D7). The `Debug` of `ReadBatch`, the bytes that `Batch::read` read, prints how many they are. <!-- part: client_telemetry -->
- Why: telemetry is copied widely and kept for a long time (D7).
- Check: `compiler` (the types that have no `Debug`), `test` (the `Debug` of each redacting type that the rule names prints none of what it redacts, `Secret`'s by SEC-01's test), `ci: check-instrument` (every `#[instrument]` that [its row in ci.md](../operations/ci.md#checks-that-exist) names has `skip_all`), plus `ci: e2e` (the proxy's span, its log line and its own log hold no client address or user agent, under any name, and the bundled Postgres's log holds no value that a unique constraint refused), plus `review`.

### OBS-05 Telemetry names come from one list
Every field name in the logs and spans that the code of `crates/platform/src/` and `crates/app/src/` writes, and every metric label name, comes from the list in `platform::telemetry`, and that code uses no other name, a JSON line's own keys aside. The list declares no field for personal data or secrets (OBS-04).
- A name is OpenTelemetry's wherever a semantic convention names the same thing, such as `http.request.method` or `http.route`. The list pins the conventions' version.
- Any other name is `<term>.<property>` in lowercase, using the glossary's term for the concept (CODE-11), and never starts with a namespace the conventions define. Where the conventions use the term with another meaning, the owning module's name goes first: `<module>.<term>.<property>`.
- `trace_id` and `span_id` are top-level fields, as OpenTelemetry names trace context in JSON logs.
- `timestamp`, `level` and `message`, under which a JSON line holds its time (OBS-02), its level and its event's message, are the line's own keys, and the list doesn't hold them.
- `otel.name` and `otel.kind`, which the bridge from `tracing` to OpenTelemetry reads to name a span and set its kind, are the bridge's own fields, and the only names in a namespace the conventions define. No JSON log line carries them; the readable text of development shows them with the span they belong to.
- Why: standard names work with standard dashboards and tools, and one list keeps each name the same in every log, span and metric (D19).
- Check: `compiler` (each convention's entry is checked against the conventions' published name), `clippy` (a deprecated convention name fails), plus `ci: check-instrument` (each field name that an event, a span or a `record` call names is on the list), and the label names as OBS-11 checks them.

## Traces

### OBS-06 One server span per request, child spans for work
Each request has one server span in the API, but for the health probes (OBS-15) and a request that the HTTP server refuses itself (PLATFORM-099), and each run of a job a span of its own (OBS-07). Inside either, each database transaction and each outbound call gets a child span. A transaction that runs outside every span, such as the hub's load of new events or a dispatcher's claim of jobs, gets none: its span would have no parent, and so would start a trace of its own each time (D218). Behind the reference deployment's proxy, the server span is a child of the proxy's span for the request, so a trace starts at the proxy (D53). Spans come only from `platform` and the binary's `main.rs`; module code opens none. CPU-heavy work runs through the `platform` helper that adds its span (CODE-05), so slow steps such as password hashing still show in traces.
- Why: a trace shows where a request spent its time, and every span has the same shape wherever it comes from.
- Check: `ci: check-instrument` (no spans in `app`'s code outside `main.rs`), plus `test` (PLATFORM-104, a database transaction's span inside a request's span and a job run's, and none outside every span), plus `review`.

### OBS-07 Trace context carries across work that happens later
Inbound W3C `traceparent` from clients is accepted. Every job and event row stores the trace context that created it. The span for running that job or delivering that event links to the stored context. Outbound requests send trace context only to our own infrastructure, never to third parties.
- Why: background work leads back to the request that caused it (D10).
- Check: `test` (the inbound `traceparent` by PLATFORM-041's tests, the link of a job's run by PLATFORM-020's, and the rest by the rule's own).

### OBS-08 Errors and slow requests are kept
The SDK samples every trace except the health probes (OBS-15). The collector's tail sampling keeps every trace with `http.response.status_code >= 400` or an error status, every trace slower than the latency objective, and a configured share of the rest. It decides a trace once its wait, 15 seconds in the reference deployment, has passed since the trace's first span arrived, on the spans that have arrived, and a span that arrives later takes that decision: a request or a job's run whose failure comes after the wait leaves a trace only when the decision kept it, and its log lines and its counts stay (D222). Logs are never sampled. The dashboard of request latency lists kept traces slower than the objective in the time range it shows, as many as its limit holds, each leading to its trace.
The spans that users' clients send through the telemetry proxy pass no tail sampling: the collector keeps each one that it admits (CLIENT_TELEMETRY-011). <!-- part: client_telemetry -->
- Why: every trace ID a user can report as a problem resolves to a trace, or, for a failure that came after the collector's wait, to its log lines; a latency spike leads to the requests behind it, and every request's logs are kept (D10).
- Check: `ci: check-telemetry-config` (the sampling policies in the collector's configuration, and the same objective in them, the alerts and the dashboard), plus `test` (end to end).

### OBS-09 Identity on the API's spans comes from the server
User and workspace attributes on the API's spans come from `Principal` and `Authorized<P>`, never from client-supplied baggage.
The spans of users' clients, which the telemetry proxy forwards, keep the attributes that their client set, a `user.id` among them, since the collector writes only their resource anew: a trace search by user that is meant to find the API's spans names the API's `service.name` too (D178). <!-- part: client_telemetry -->
- Why: a trace search by user among the API's spans returns the truth.
- Check: `review`.

### OBS-10 Telemetry setup fails loudly
With telemetry enabled, startup fails if an exporter can't be built. Export failures at runtime are counted, never fatal.
- Why: no silent fallback to all-zero trace IDs (D10).
- Check: `test`.

## Metrics

### OBS-11 Metrics are declared, named and bounded
Every metric but those that the API's OpenTelemetry SDK counts itself is declared once, as a `static` of one of the declaration types in `platform::telemetry::metrics`, with its name, unit, description and label names, and a histogram's bucket boundaries. Each module declares its metrics in `metrics.rs`, and `platform` declares its own in the area that records them. A name is OpenTelemetry's wherever a semantic convention defines the metric, checked against the conventions' published name as OBS-05's are. Any other name is `<module>.<name>`: the declaring module's name, `platform` for its own, then what it counts or measures in `snake_case`, such as `platform.panics`. Label names are the constants of the list in `platform::telemetry` (OBS-05), and a recording passes one value for each, in order. Labels come from bounded sets and never include IDs or personal data.
- Why: every metric is known, documented and cheap to store, and an own name can't pass for one of the conventions' (D19). The conventions use `app.` for client applications.
- Check: `compiler` (the declaration types, and the conventions' names), `clippy` (getting an OpenTelemetry meter outside the declaration types), plus `ci: check-metrics` (own names, and label names from the list).

### OBS-12 The metric catalog is generated
`docs/operations/metrics.md` is generated from the declarations, and from `deploy/observability/metrics.yaml`, which lists the metrics that other parts of the stack produce, such as the collector's own, and those that the API's OpenTelemetry SDK counts itself. CI fails if the catalog is out of date, if a dashboard or alert uses a metric that neither declares, or if a metric either declares isn't used by any dashboard or alert.
- Why: dashboards, alerts and code can't drift apart (D19).
- Check: `ci: check-metrics`.

## Dashboards and alerts

### OBS-13 Dashboards answer named questions
Dashboards are the files in `deploy/observability/grafana/`, and read-only in Grafana: the self-hosted Grafana provisions them, and a managed Grafana, which can't read provisioning files, imports them into a folder that only the import and the managed Grafana's administrators may change (D42). One overview dashboard links to one dashboard per area, by the tag `area` that each of them carries.
So a dashboard that a part of the template owns leaves no link behind once a fork leaves the part out (D119). <!-- part: removal -->
Each panel's title is the question it answers. A panel or an alert that counts the events of a metric that the API declares within a window, with `increase()`, also counts a series that appears within the window, by its first value.
- Why: a dashboard is read under pressure, and must say what it shows (D19). Whichever Grafana shows them, the files are the dashboards, and the checks read the files.
- Check: `ci: check-metrics` (queries, the form of an `increase()` of a metric that the API declares, which [ci.md](../operations/ci.md) gives, titles that end in a question mark, read-only provisioning, and an overview that links to every other dashboard, by its address or by its tags), `ci: e2e` (the import into a managed Grafana), plus `review` (what each title asks, one dashboard per area, and what a query does with the count of that form).

### OBS-14 Alerts fire on symptoms and have runbooks
Alerts are of two kinds:
- Burn-rate alerts against the service-level objectives for availability and latency.
- Cause alerts for a dependency that is down, dead-lettered jobs, jobs that wait long past their time, periodic jobs that keep failing, events delivered late or not loaded at all, telemetry that is dropped, that stops arriving, or that a service of the stack can't receive because it is down, backups whose archiving of the write-ahead log fails or whose newest base backup is older than their schedule allows, and a server whose disk fills or whose memory runs low.
  Also a malware scanner whose signatures are older than their updates allow. <!-- part: files -->
  Also batches of the telemetry of users' clients that the collector neither takes nor refuses, which the telemetry proxy answers with 503 (CLIENT_TELEMETRY-009), and those that a replica's budget refuses (CLIENT_TELEMETRY-008). No alert reads a metric that users' clients send, since a client reports its numbers itself. <!-- part: client_telemetry -->

Every alert has a severity (`page` or `ticket`), a runbook section in `docs/operations/runbooks.md` and a `promtool` test. A summary or a runbook that says for how long a state has lasted names how long it has when the alert fires: the rule's `for`, and, where the rule holds while its window shows none of something, such as no answer of a bucket's, the window's time added to it, as "for 7 minutes" of a window of 5 minutes and a `for` of 2. While no series of what the window shows none of exists yet, as before the first answer since the replicas started, a rule that also counts something, such as unanswered calls, holds from the first of them, and the alert fires once its `for` has passed, though the summary and the runbook name the longer time. A test of each rule whose summary names such a time expects no alert at one evaluation and the alert at the next. The rules are the file `deploy/observability/prometheus/rules.yaml`, which the self-hosted Prometheus reads and a managed Grafana imports as rules of its own.
- Why: an alert means action, and the action is written down (D19). A backup that fails silently is found only when it is needed (D30).
- Check: `ci: alert-rules` (each rule has a test and a runbook anchor), plus `ci: e2e` (an alert reaches the integration a release names, and a managed Grafana imports the rules), plus `review` (what each alert reads and fires on, the time that its summary and its runbook name, and that a runbook's command names the service that runs in each kind of deployment, as a command that shows a collector's log names the collector of a deployment whose telemetry is self-hosted and that of one whose telemetry is managed).

### OBS-15 Health probes stay out of telemetry
Liveness and readiness requests are excluded from request metrics, are not sampled into traces, and carry no trace ID (API-16). The reference deployment's proxy doesn't trace them either.
- Why: probes would drown out the signal.
- Check: `test`, plus `ci: e2e` (a probe through the proxy leaves no span).
