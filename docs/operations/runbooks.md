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

**What it means.** More than the budgeted share of requests take longer than the [latency objective](observability.md#service-level-objectives), fast enough to spend the month's budget within days.

**Look first.** The API requests dashboard: "How long do the slowest requests take, by route?" names the routes, and "Which requests took longer than the latency objective?" lists their traces.

**What to do.** Open a few of the slow traces: a trace's spans show where its time went, and its log lines what it did. If every route slowed at once, check the instances' load and the database's; if one route did, its traces show the step that grew.

## LatencyBudgetBurningSlowly

**What it means.** Requests take longer than the latency objective often enough to spend the latency budget within the month.

**Look first.** "How long do the slowest requests take, by route?" over the last week shows whether one route grew slower over time.

**What to do.** Investigate as under [LatencyBudgetBurningFast](#latencybudgetburningfast), and open an issue for the cause.

## DatabaseUnreachable

**What it means.** An instance's own connection to the database stopped answering its [checks](../spec/platform.md#readiness-and-shutdown). Its readiness probe answers `database_unavailable`, so the load balancer stops sending it requests.

**Look first.** The API platform dashboard: "Can each instance reach its database?" shows whether one instance or all of them lost it. The instance's `WARN` line "the database stopped answering" holds the error.

**What to do.** If every instance lost the database, check Postgres itself: whether it runs, whether it accepts connections, and whether its disk is full. If one instance did, check that instance's network path to the database, and restart the instance if it doesn't recover.

## ApiTelemetryMissing

**What it means.** No instance of the API exported metrics for five minutes. Either no instance runs, or their telemetry doesn't reach Prometheus.

**Look first.** Whether the API answers its health probes. Then the Telemetry pipeline dashboard: "Can Prometheus reach every service of the stack?" shows whether the collector is up.

**What to do.** If the API is down, restart it, and read its last log lines on stdout, which don't depend on the stack. If it runs, check that its `OTEL_EXPORTER_OTLP_ENDPOINT` names the collector, and that the collector runs and passes metrics to Prometheus.

## TelemetryExportFailing

**What it means.** An instance of the API failed to export spans, log records or metrics in the last 10 minutes. The API goes on serving (OBS-10), but the traces and logs of its requests may be missing, so trace IDs from that time may lead nowhere.

**Look first.** The Telemetry pipeline dashboard: "Is the API failing to export telemetry?" names the instance and the signal.

**What to do.** Check that the collector runs and accepts data, and that the instance can reach `OTEL_EXPORTER_OTLP_ENDPOINT`. A collector that answers slowly fails exports too, once each outlasts the [export timeout](../spec/platform.md#telemetry-export).

## CollectorDroppingTelemetry

**What it means.** The collector refused telemetry, failed to send it to Tempo, Loki or Prometheus, or dropped traces before tail sampling decided them, in the last 10 minutes.

**Look first.** The Telemetry pipeline dashboard: "Is the collector refusing telemetry?", "Is the collector failing to pass telemetry on?" and "Is tail sampling dropping traces before it decides them?" show which step loses data.

**What to do.** Refused data means the collector reached its memory limit: give it more memory. Failed sends name a store: check that Tempo, Loki or Prometheus runs and has disk space. Traces dropped early mean the collector holds more undecided traces than its `num_traces`: raise it, along with the collector's memory.

## BackupArchivingFailing

**What it means.** The bundled Postgres failed to archive a segment of its write-ahead log to the backup bucket, and no archive has succeeded since. Postgres keeps each segment until it is archived, so its disk fills, and the latest moment a restore can reach stops advancing: losing the server now loses everything since the last archive that succeeded (OPS-17).

**Look first.** On the server, `docker logs deployment-postgres-1` holds pgBackRest's error for each attempt, such as a key pair the bucket refuses, an endpoint that doesn't answer, or a certificate the deployment doesn't trust.

**What to do.** Fix what the error names: the bucket's key pair in `secrets/`, the bucket's settings in `settings.env`, or the bucket itself. Postgres retries on its own, and the alert clears with the next archive that succeeds. Then take a base backup with `just backup`, so a restore needs no segment from before the failure.

## BaseBackupTooOld

**What it means.** The bundled database's daily base backup hasn't completed for more than 26 hours. A restore still reaches every moment the archived write-ahead log covers, but it replays more of the log the older the backup it starts from, so it takes longer.

**Look first.** On the server, `docker logs deployment-backup-1` holds a line for each scheduled backup that failed, after pgBackRest's error. If the backup service isn't running, `docker ps --all` shows why it stopped.

**What to do.** Fix what the error names, often the same as for [BackupArchivingFailing](#backuparchivingfailing), then take a full backup at once with `just backup`. The alert clears when it completes.

## ObservabilityServiceDown

**What it means.** Prometheus can't scrape the collector, Loki, Tempo or Alertmanager. Telemetry that passes through that service may be lost, and alerts may not reach anyone if it is Alertmanager.

**Look first.** The Telemetry pipeline dashboard: "Can Prometheus reach every service of the stack?" names the service.

**What to do.** Check the service's container, its logs and its disk, and restart it.

## DiskFillingUp

**What it means.** A disk of the server is more than 85% full and, at the rate of the last six hours, fills within a day. When the disk that holds the deployment's volumes fills, Postgres stops writing and the stack stops storing telemetry.

**Look first.** The Server dashboard: "How full is each disk?" names the mount point and shows since when it fills. On the server, `docker system df --verbose` shows the size of each volume and image.

**What to do.** Free space or grow the disk. Images that no container runs go with `docker image prune --all`. If Postgres's volume grows while [BackupArchivingFailing](#backuparchivingfailing) fires, Postgres is keeping the write-ahead log it can't archive: fix the archiving, and Postgres removes the segments it kept.

## DiskFull

**What it means.** A disk of the server is more than 95% full and, at the rate of the last hour, fills within four hours. Once it is full, nothing on the server can write to it.

**Look first.** As for [DiskFillingUp](#diskfillingup).

**What to do.** As for [DiskFillingUp](#diskfillingup), now.

## MemoryRunningLow

**What it means.** Less than a tenth of the server's memory has been available for 15 minutes. When memory runs out, the kernel kills a process, often a replica or Postgres.

**Look first.** The Server dashboard: "How much of the server's memory is available?" shows since when it fell, and the API platform dashboard's "How much memory does each instance use?" whether a replica's grows. On the server, `docker stats --no-stream` shows each container's memory.

**What to do.** A replica whose memory only grows points to a leak: deploy the release before it, and report it. Otherwise lower `API_REPLICAS` or `API_MEMORY`, or move to a server with more memory.
