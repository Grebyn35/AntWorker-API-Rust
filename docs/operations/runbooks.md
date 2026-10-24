# Runbooks

What to do when an alert fires: one section per alert in [rules.yaml](../../deploy/observability/prometheus/rules.yaml), whose `runbook` annotation links here (OBS-14). A `page` alert needs someone now; a `ticket` alert needs someone within the working day. [observability.md](observability.md) describes the dashboards and how to follow a request from its trace ID.

## AvailabilityBudgetBurningFast

**What it means.** Enough requests answer with a 5xx that the availability objective's monthly budget would be gone within days: users see failures now.

**Look first.** The API requests dashboard: "Which routes answer with a 5xx, and with which error?" names the routes and the error codes, and the time the failures started.

**What to do,** by error code:
- `request.overloaded`: instances shed load, because more requests arrived than `MAX_CONCURRENT_REQUESTS` lets them serve at once. Add instances, or find what made requests slower, since slower requests hold their places longer.
- `request.timed_out`: requests ran past their timeout, usually waiting on the database. Check the API platform dashboard, and the database's own load.
- `request.internal_error`: a bug or a failing dependency. Find a failed request's trace ID in Loki with `{service_name="antworker-api"} | severity_text="ERROR"`, then follow it as [observability.md](observability.md#following-a-request-from-its-trace-id) says. The `ERROR` line holds the cause chain.

If a deploy started the failures, roll it back first, then investigate.

## AvailabilityBudgetBurningSlowly

**What it means.** Requests answer with a 5xx often enough to spend the availability objective's budget within the month, though slowly enough that no one needs to act now.

**Look first.** The API requests dashboard over the last day: "Which routes answer with a 5xx, and with which error?" usually shows one route or one error code.

**What to do.** Follow the error codes as under [AvailabilityBudgetBurningFast](#availabilitybudgetburningfast), and open an issue for the cause.

## LatencyBudgetBurningFast

**What it means.** More than the budgeted 1% of requests take longer than the latency objective of 0.5 seconds, fast enough to spend the month's budget within days.

**Look first.** The API requests dashboard: "How long do the slowest requests take, by route?" names the routes, and "Which requests took longer than the latency objective?" lists their traces.

**What to do.** Open a few of the slow traces: a trace's spans show where its time went, and its log lines what it did. If every route slowed at once, check the instances' load and the database's; if one route did, its traces show the step that grew.

## LatencyBudgetBurningSlowly

**What it means.** Requests take longer than 0.5 seconds often enough to spend the latency budget within the month.

**Look first.** "How long do the slowest requests take, by route?" over the last week shows whether one route grew slower over time.

**What to do.** Investigate as under [LatencyBudgetBurningFast](#latencybudgetburningfast), and open an issue for the cause.

## DatabaseUnreachable

**What it means.** An instance's own connection to the database stopped answering its checks, which run every 5 seconds. Its readiness probe answers `database_unavailable`, so the load balancer stops sending it requests.

**Look first.** The API platform dashboard: "Can each instance reach its database?" shows whether one instance or all of them lost it. The instance's `WARN` line "the database stopped answering" holds the error.

**What to do.** If every instance lost the database, check Postgres itself: whether it runs, whether it accepts connections, and whether its disk is full. If one instance did, check that instance's network path to the database, and restart the instance if it doesn't recover.

## ApiTelemetryMissing

**What it means.** No instance of the API exported metrics for five minutes. Either no instance runs, or their telemetry doesn't reach Prometheus.

**Look first.** Whether the API answers its health probes. Then the Telemetry pipeline dashboard: "Can Prometheus reach every service of the stack?" shows whether the collector is up.

**What to do.** If the API is down, restart it, and read its last log lines on stdout, which don't depend on the stack. If it runs, check that its `OTEL_EXPORTER_OTLP_ENDPOINT` names the collector, and that the collector runs and passes metrics to Prometheus.

## TelemetryExportFailing

**What it means.** An instance of the API failed to export spans, log records or metrics in the last 10 minutes. The API goes on serving (OBS-10), but the traces and logs of its requests may be missing, so trace IDs from that time may lead nowhere.

**Look first.** The Telemetry pipeline dashboard: "Is the API failing to export telemetry?" names the instance, the signal and the reason, `timeout` or `export_failed`.

**What to do.** Check that the collector runs and accepts data, and that the instance can reach `OTEL_EXPORTER_OTLP_ENDPOINT`. Timeouts point to an overloaded collector or network.

## CollectorDroppingTelemetry

**What it means.** The collector refused telemetry, failed to send it to Tempo, Loki or Prometheus, or dropped traces before tail sampling decided them, in the last 10 minutes.

**Look first.** The Telemetry pipeline dashboard: "Is the collector refusing telemetry?", "Is the collector failing to pass telemetry on?" and "Is tail sampling dropping traces before it decides them?" show which step loses data.

**What to do.** Refused data means the collector reached its memory limit: give it more memory. Failed sends name a store: check that Tempo, Loki or Prometheus runs and has disk space. Traces dropped early mean the collector holds more undecided traces than its `num_traces`: raise it, along with the collector's memory.

## ObservabilityServiceDown

**What it means.** Prometheus can't scrape the collector, Loki, Tempo or Alertmanager. Telemetry that passes through that service may be lost, and alerts may not reach anyone if it is Alertmanager.

**Look first.** The Telemetry pipeline dashboard: "Can Prometheus reach every service of the stack?" names the service.

**What to do.** Check the service's container, its logs and its disk, and restart it.
