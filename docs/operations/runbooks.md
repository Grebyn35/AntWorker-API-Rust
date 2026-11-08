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

**What it means.** An instance's listener, its own connection to the database, stopped answering its [checks](../spec/platform.md#readiness-and-shutdown). Its readiness probe answers `database_unavailable`, so the load balancer stops sending it requests.

**Look first.** The API platform dashboard: "Can each instance reach its database?" shows whether one instance or all of them lost it. The instance's `WARN` line "the database stopped answering" holds the error.

**What to do.** If every instance lost the database, check Postgres itself: whether it runs, whether it accepts connections, and whether its disk is full. If one instance did, check that instance's network path to the database, and restart the instance if it doesn't recover.

## StorageUnreachable

**What it means.** For 5 minutes, the bucket of files' bytes answered none of the API's calls, while some went unanswered: each wait for its answer ran out, the connection to it failed, or it answered with a 5xx or a 429, that it can't serve now ([platform spec](../spec/platform.md#storage)). Uploads and downloads answer 503 `files.storage_unavailable` meanwhile (FILES-012), while every other route serves.

**Look first.** The Files dashboard: "Does the bucket answer?" shows when the calls stopped being answered. The `ERROR` lines of the 503s name the S3 operation and the failure, such as a connection's or a status.

**What to do.** Check the provider's status page, and that the server reaches the bucket's endpoint: `just server-check` reads the bucket with the deployment's key pair. A provider's outage clears with it; uploads that failed meanwhile were stored nowhere, so their clients send them again.

## ScannerUnreachable

**What it means.** For 5 minutes, the malware scanner answered none of the API's scans, while some went unanswered: the connection to it failed, it closed the connection before its answer, or it took more than its 30 seconds ([platform spec](../spec/platform.md#scanner)). Uploads answer 503 `files.scanner_unavailable` meanwhile, and store nothing (FILES-005), while every other route serves.

**Look first.** `just status`, which shows whether the `scanner` container runs and is healthy, then `just logs <host> scanner`. A scanner that restarted loads its signatures again for about half a minute; one that has none yet, as on a new server, downloads them first, and its log says so. The Files dashboard: "Does the scanner answer?" shows when the scans stopped being answered, and the `ERROR` lines of the 503s name the failure.

**What to do.** A scanner that runs but answers late is short of CPU: a burst of large Word, Excel or PowerPoint files takes about 0.9 seconds a MiB each, one processor apiece, up to `SCANNER_CPUS` at once ([deploy.md](deploy.md#the-scanner)). Raise `SCANNER_CPUS` on a server with processors to spare, and deploy with `--allow-restart`. A scanner that stopped, such as one the kernel killed for its memory while it loaded new signatures, restarts on its own; give the server the memory that [deploy.md](deploy.md#what-a-server-needs) asks for. Uploads that failed meanwhile were stored nowhere, so their clients send them again.

## ScannerSignaturesOutdated

**What it means.** The malware scanner's daily signatures haven't changed for 2 days, though ClamAV publishes new ones several times a day, so it misses the malware found since. freshclam, beside clamd in the `scanner` container, checks for them 12 times a day ([deploy.md](deploy.md#the-scanner)).

**Look first.** `just logs <host> scanner`: freshclam's lines name each check that failed, such as a name it couldn't resolve, a connection it couldn't make, or ClamAV's mirrors refusing the server for checking too often, which they answer with 429 and a time to wait.

**What to do.** A server that can't reach `database.clamav.net` needs its firewall or its DNS fixed. A server that ClamAV's mirrors refuse waits out the time they gave, and checks again on its own; one whose address a provider shares with many others may stay refused, and a fork then runs a private mirror, as [ClamAV's documentation](https://docs.clamav.net/manual/Usage/Configuration.html) describes, and names it in deploy/scanner/freshclam.conf. Uploads go on being scanned with the signatures the scanner has meanwhile.

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

## EventsDelayed

**What it means.** Events reach their streams more than 5 seconds after their units of work committed, for the slowest one in a hundred ([platform spec](../spec/platform.md#events-and-the-real-time-hub)). Clients then show changes late. Either the hubs' loads of new events are slow, each replica's hub running one at a time, or units of work that publish wait on each other's commits, which Postgres serializes for every transaction that notifies.

**Look first.** The API platform dashboard: "How long after commit do events reach the streams?" shows since when, and "How many event streams are open?" whether streams grew with it. The Requests dashboard shows whether the database is slow for every request too. A hub whose load fails writes the `WARN` line "the hub couldn't load events", with the database's error.

**What to do.** If the database is slow for everything, follow [DatabaseUnreachable](#databaseunreachable)'s look at its load. If only events are late while streams grew, the replicas hold more streams than their hubs serve: add replicas, so the streams spread over more hubs (OPS-16). The alert clears once the lag falls.

## EventsStalled

**What it means.** A replica's hub has heard of events that committed, but for 5 minutes it couldn't load them ([platform spec](../spec/platform.md#events-and-the-real-time-hub), PLATFORM-035). The streams on that replica receive nothing new, though their clients' requests still answer: a reload shows what the stream left out. The hub records no lag for events it doesn't load, so [EventsDelayed](#eventsdelayed) stays silent.

**Look first.** The API platform dashboard: "How many events does each hub owe its streams?" shows which replica, and since when. That replica writes the `WARN` line "the hub couldn't load events" at every check, every 5 seconds, with the database's error: a pool whose every connection is taken shows as a timeout acquiring one, and then the requests that need one fail too, which "Which routes answer with a 5xx, and with which error?" on the API requests dashboard shows.

**What to do.** If the pool is exhausted, find what holds its connections, as [DatabaseUnreachable](#databaseunreachable)'s look at the database's load does, or raise `DATABASE_MAX_CONNECTIONS` within the budget that [deploy.md](deploy.md#scaling) works out. Any other error names its cause in the line. Restarting the replica closes its streams, whose clients reconnect to another and replay what they missed (REALTIME-004). The alert clears once the hub owes nothing.

## JobsDelayed

**What it means.** A background job that is due has waited more than 10 minutes without a replica claiming it ([platform spec](../spec/platform.md#the-job-queue)). Every email waits with it. Either no replica's dispatcher claims jobs, every replica's 4 runs are taken by long runs, or the job's kind is one that no running replica knows, as after a rollback to a release that doesn't have it.

**Look first.** The API platform dashboard: "Are background jobs keeping up?" shows since when the delay grows, and "How do job runs end?" whether runs end at all. A replica that can't claim writes the `WARN` line "can't claim jobs", with the database's error.

**What to do.** If no replica runs, follow [ApiTelemetryMissing](#apitelemetrymissing); if claims fail, [DatabaseUnreachable](#databaseunreachable). If runs end but slowly, "How long do job runs take?" names the kind, and its handler's logs say what it waits on. If the waiting job's kind is one the release doesn't know, the query of [JobsDeadLettered](#jobsdeadlettered), with `state = 'waiting'`, shows it: deploy a release that knows it. The alert clears once the queue catches up.

## JobsDeadLettered

**What it means.** A job failed on each of its attempts, or with a failure that every attempt would meet, and moved to the dead-letter state, where nothing retries it ([platform spec](../spec/platform.md#the-job-queue)). For `platform.send_email`, an email didn't go out: a mail server that refused it for good, or one that didn't answer for the hours its retries span ([platform spec](../spec/platform.md#sending-email)).

**Look first.** The API's `ERROR` line "job dead-lettered" names the kind and the failure. With the bundled database, this lists the dead letters, without their payloads, which hold personal data such as an email's recipient:

```sh
ssh <host> "docker exec deployment-postgres-1 psql --username=postgres --dbname=api --command=\"SELECT id, kind, attempts, dead_at, failure FROM jobs WHERE state = 'dead' ORDER BY dead_at DESC\""
```

With a managed database, run the same query in the provider's console. Add `payload` to the columns only when the failure doesn't say enough: it holds what the job needed, such as an email's recipient.

**What to do.** Fix what the failure names, such as the mail server's settings or credentials with `just server-setup`, or the sender's domain at the provider. A dead letter's remedy is the operation that caused it, done again, such as the request that sent an email, which sends a new one. The sweep deletes dead letters after `JOB_DEAD_LETTER_RETENTION_DAYS`, and the alert clears an hour after the last one.

## PeriodicJobFailing

**What it means.** A periodic job failed at least twice within 3 hours, as an hourly job that keeps failing does, or twice within 49 hours with no run that succeeded, as a daily job that keeps failing does from an hour after its second failure, such as `files.find_unreferenced_objects`. Each failed run waits for its next interval, so what it deletes stays meanwhile; nothing a request reads changes ([platform spec](../spec/platform.md#the-job-queue)).

**Look first.** The API's `WARN` line "periodic job failed, and runs again at its next interval" names the job and the failure.

**What to do.** A failure of the database clears with it: follow [DatabaseUnreachable](#databaseunreachable); one of the files bucket, with it: follow [StorageUnreachable](#storageunreachable). A run that passes its time limit, which its job's declaration sets, stops where it was. The sweeps and `files.delete_unreferenced_objects` delete in batches, so the next run continues where it stopped. `files.find_unreferenced_objects` keeps what it recorded, but its next run lists the bucket from its start again, so a bucket whose listing takes longer than the hour has objects that it never reaches, which no deletion then removes: its runs fail each day, and the job needs a longer limit or a listing that resumes. The alert clears 3 hours after the last failure, or, for a daily job, once a run succeeds.

## BackupArchivingFailing

**What it means.** The bundled Postgres failed to archive a segment of its write-ahead log to the backup bucket, and no archive has succeeded since. Postgres keeps each segment until it is archived, so its disk fills, and the latest moment a restore can reach stops advancing: losing the server now loses everything since the last archive that succeeded (OPS-15). Meanwhile a restore refuses, unless it finds Postgres not running or still replaying the log, as [Restoring](backups.md#restoring) says.

**Look first.** `just logs <host> postgres` shows pgBackRest's error for each attempt, such as a key pair the bucket refuses, an endpoint that doesn't answer, or a certificate the deployment doesn't trust.

**What to do.** Fix what the error names: the bucket itself, after which Postgres retries on its own, or the bucket's key pair or its settings, with `just server-setup`, then `just deploy <host> --allow-restart`, since Postgres reads them only as it starts ([deploy.md](deploy.md#restarting-postgres-caddy-or-the-scanner)); `just server-check` reads the bucket as Postgres does. The alert clears with the next archive that succeeds. Then take a base backup with `just backup`, so a restore needs no segment from before the failure.

## BaseBackupTooOld

**What it means.** The bundled database's daily base backup hasn't completed for more than 26 hours. A restore still reaches every moment the archived write-ahead log covers, but it replays more of the log the older the backup it starts from, so it takes longer.

**Look first.** `just logs <host> backup` shows a line for each scheduled backup that failed, after pgBackRest's error. If the backup service isn't running, `just status <host>` shows how it exited, and its logs why.

**What to do.** Fix what the error names, often the same as for [BackupArchivingFailing](#backuparchivingfailing), then take a full backup at once with `just backup`. The alert clears when it completes.

## ObservabilityServiceDown

**What it means.** Prometheus can't scrape the collector, Loki, Tempo or Alertmanager. Telemetry that passes through that service may be lost, and alerts may not reach anyone if it is Alertmanager.

**Look first.** The Telemetry pipeline dashboard: "Can Prometheus reach every service of the stack?" names the service.

**What to do.** Check the service's container, its logs and its disk, and restart it.

## DiskFillingUp

**What it means.** A disk of the server is more than 85% full and, at the rate of the last six hours, fills within a day. When the disk that holds the deployment's volumes fills, Postgres stops writing and the stack stops storing telemetry.

**Look first.** The Server dashboard: "How full is each disk?" names the mount point and shows since when it fills. `just status <host>` lists the data that restores moved aside, with its size. On the server, `docker system df --verbose` shows the size of each volume and image.

**What to do.** Free space or grow the disk. Each deploy removes the images and the release files that nothing uses, and each container's log is capped (D57), so what grows is data. Data that a restore moved aside goes as [backups.md](backups.md#restoring) says. Images of other repositories, which no deploy removes, go with `docker image prune --all`, which also removes the tools' images and the previous release's, so the next deploy, or a rollback, pulls them again. If Postgres's volume grows while [BackupArchivingFailing](#backuparchivingfailing) fires, Postgres is keeping the write-ahead log it can't archive: fix the archiving, and Postgres removes the segments it kept.

## DiskFull

**What it means.** A disk of the server is more than 95% full and, at the rate of the last hour, fills within four hours. Once it is full, nothing on the server can write to it.

**Look first.** As for [DiskFillingUp](#diskfillingup).

**What to do.** As for [DiskFillingUp](#diskfillingup), now.

## MemoryRunningLow

**What it means.** Less than a tenth of the server's memory has been available for 15 minutes. When memory runs out, the kernel kills a process, often a replica or Postgres.

**Look first.** The Server dashboard: "How much of the server's memory is available?" shows since when it fell, and the API platform dashboard's "How much memory does each instance use?" whether a replica's grows. On the server, `docker stats --no-stream` shows each container's memory.

**What to do.** A replica whose memory only grows points to a leak: deploy the release before it, and report it. Otherwise lower `API_REPLICAS` or `API_MEMORY`, or move to a server with more memory.
