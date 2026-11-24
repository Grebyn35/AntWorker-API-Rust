# Runbooks

What to do when an alert fires: one section per alert in [rules.yaml](../../deploy/observability/prometheus/rules.yaml), whose `runbook` annotation links here (OBS-14). A `page` alert needs someone now; a `ticket` alert needs someone within the working day. [observability.md](observability.md) describes the dashboards and how to follow a request from its trace ID. An ID in parentheses names what a sentence follows: a rule of the [standards](../standards/README.md), such as OBS-14, a behavior of a [spec](../spec/README.md), such as AUTH-077, or a decision, such as D57, whose records [architecture.md](../architecture.md) says where to find. The [glossary](../architecture.md#11-glossary) defines the terms these documents share.

Each `just` command here runs on your machine, in your checkout of the repository, as [deploy.md](deploy.md#deploying-a-release) says: `<host>` is the server as Secure Shell (SSH) names it, such as `production`, and `<version>` the release that serves, which `just status <host>` shows. To change a setting, run `just server-setup <host>` again, and the next deploy applies it ([deploy.md](deploy.md#setting-up-a-server)).

## AvailabilityBudgetBurningFast

**What it means.** Enough requests answer with a 5xx that the availability objective's monthly budget would be gone within days: users see failures now.

**Look first.** The API requests dashboard: "Which routes answer with a 5xx, and with which error?" names the routes and the error codes, and the time the failures started.

**What to do,** by error code:
- `request.overloaded`: instances shed load, because more requests arrived than `MAX_CONCURRENT_REQUESTS` lets them serve at once. Add instances, by raising `API_REPLICAS` ([deploy.md](deploy.md#scaling)), or find what made requests slower, since slower requests hold their places longer.
- `request.timed_out`: requests ran past their timeout, usually waiting on the database. Check the API platform dashboard, and the database's own load: the bundled database's, which runs on the server, on the Server dashboard's "How busy are the server's CPUs?", and a managed one's in its provider's console.
- `request.internal_error`: a bug or a failing dependency. Find a failed request's trace ID in Loki with `{service_name="antworker-api"} |= "request failed"`, which selects the API's `ERROR` lines of its 5xx responses by their message, then follow it as [observability.md](observability.md#following-a-request-from-its-trace-id) says. That line holds the cause chain. A panic writes the line `panicked` instead, which the API platform dashboard counts under "Did anything panic?".
- A module's code starts with its module's name, and that module's spec says when it answers it: `auth.provider_unavailable`, for one, means that an OAuth provider gave no identity, for a reason that [SignInProviderUnavailable](#signinproviderunavailable) lists.

If the failures started when a deploy did, roll it back first ([deploy.md](deploy.md#rolling-back)), then investigate: `just status <host>` shows the version that serves, and how long its replicas have run.

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

<!-- part: files -->
## StorageUnreachable

**What it means.** For 7 minutes, the bucket of files' bytes answered none of the API's calls, while some went unanswered: each wait for its answer ran out, the connection to it failed, or it answered with a 5xx or a 429, that it can't serve now ([platform spec](../spec/platform.md#storage)). Uploads and downloads answer 503 `files.storage_unavailable` meanwhile (FILES-012), an avatar's upload `accounts.avatar_unavailable` (ACCOUNTS-009), and a workspace's picture's `workspaces.picture_unavailable` (WORKSPACES-020), while every other route serves.

**Look first.** The Files dashboard: "Does the bucket answer?" shows when the calls stopped being answered. The `ERROR` lines of the 503s name the S3 operation and the failure, such as a connection's or a status.

**What to do.** Check the provider's status page, and that the server reaches the bucket's endpoint: `just server-check <host>` reads the bucket with the deployment's key pair. A provider's outage clears with it; uploads that failed meanwhile were stored nowhere, so their clients send them again.

## ScannerUnreachable

**What it means.** For 7 minutes, the malware scanner answered none of the API's scans, while some went unanswered: the connection to it failed, it closed the connection before its answer, or it took more than its 30 seconds ([platform spec](../spec/platform.md#scanner)). Uploads answer 503 `files.scanner_unavailable` meanwhile, an avatar's `accounts.avatar_unavailable` and a workspace's picture's `workspaces.picture_unavailable`, and store nothing (FILES-005, ACCOUNTS-009, WORKSPACES-020), while every other route serves.

**Look first.** `just status <host>`, which shows whether the `scanner` container runs and is healthy, then `just logs <host> scanner`. A scanner that restarted loads its signatures again first ([deploy.md](deploy.md#the-scanner)); one that has none yet, as on a new server, downloads them first, and its log says so. The Files dashboard: "Does the scanner answer?" shows when the scans stopped being answered, and the `ERROR` lines of the 503s name the failure.

**What to do.** A scanner that runs but answers late is short of CPU: a burst of large Word, Excel or PowerPoint files, which scan the slowest ([files spec](../spec/files.md#scanning)), shares the scanner's `SCANNER_CPUS`, so each of their scans takes longer ([deploy.md](deploy.md#the-scanner)). Raise `SCANNER_CPUS` on a server with processors to spare, and deploy with `just deploy <host> <version> --allow-restart`. A scanner that stopped, such as one the kernel killed for its memory while it loaded new signatures, restarts on its own; give the server the memory that [deploy.md](deploy.md#what-a-server-needs) asks for. Uploads that failed meanwhile were stored nowhere, so their clients send them again.

## ScannerSignaturesOutdated

**What it means.** The malware scanner's daily signatures haven't changed for 2 days and 10 minutes, though ClamAV publishes new ones several times a day, so it misses the malware found since. freshclam, beside clamd in the `scanner` container, checks for them several times a day ([deploy.md](deploy.md#the-scanner)).

**Look first.** `just logs <host> scanner`: freshclam's lines name each check that failed, such as a name it couldn't resolve, a connection it couldn't make, or ClamAV's mirrors refusing the server for checking too often, which they answer with 429 and a time to wait.

**What to do.** A server that can't reach `database.clamav.net` needs its firewall or its DNS fixed. A server that ClamAV's mirrors refuse waits out the time they gave, and checks again on its own; one whose address a provider shares with many others may stay refused, and a fork then runs a private mirror, as [ClamAV's documentation](https://docs.clamav.net/manual/Usage/Configuration.html) describes, and names it in deploy/scanner/freshclam.conf. Uploads go on being scanned with the signatures the scanner has meanwhile.

<!-- end part: files -->
## SignInProviderUnavailable

**What it means.** For 20 minutes, an OAuth provider, which the alert's `auth_provider` names, gave no identity for any sign-in through it, while some went unanswered: it failed, answered what the API can't read, didn't answer in time, or refused the API's own client, such as a secret that the deployment set wrong (AUTH-077). Sign-ins and re-authentications through it answer 503 `auth.provider_unavailable` meanwhile, while those with a password, and through any other provider, go on.

**Look first.** The Sign-in dashboard: "How do exchanges of providers' codes end?" shows when the provider stopped answering. The `ERROR` lines of the 503s name the provider and the failure, such as a connection's, a status, or the refusal of the client.

**What to do.** A refusal of the client is the deployment's to fix: compare the client's ID and secret with the provider's console, and set them again with `just server-setup <host>` ([deploy.md](deploy.md#signing-in-through-a-provider)).
For `openid_connect`, compare the addresses in the settings with the provider's discovery document too, which a provider may change; and an `ERROR` line that says its token endpoint's answer doesn't say when its token lapses clears only once the provider gives access tokens that lapse, which its own settings decide (AUTH-103). <!-- part: openid_connect -->
For any other failure, check the provider's status page, and that the server reaches the provider's endpoints; a provider's outage clears with it, and its users sign in again once it does.

## MailServerUnreachable

**What it means.** For 20 minutes, the mail server took or refused none of the API's sends, while some of them failed otherwise, such as a connection that didn't open, TLS that failed, a reply of 421, by which the server says that its service isn't available, a reply of 454, which a server gives to `STARTTLS` when TLS isn't available for now, a server that doesn't offer `STARTTLS` while `SMTP_SECURITY` is `starttls`, or a send that took longer than its 30 seconds (PLATFORM-105). A reply of 454 counts among them whichever command it answers, such as `AUTH` or `RCPT`. No request waits on the mail server, so every route serves, but each email, a registration's verification code and a password reset's code among them, waits for its job's next attempt, and dead-letters once its 12 attempts, whose waits between them come to about 1.7 to 3.4 hours, have all failed ([platform spec](../spec/platform.md#sending-email)).

**Look first.** The API platform dashboard: "Does the mail server answer?" shows since when the sends go unanswered. Each failed attempt that its job runs again writes the `WARN` line "job failed, and runs again later", whose `job.kind` is `platform.send_email`, or the job of a module that sends an email at once, such as `auth.send_verification_code`, and whose `exception.message` names the failure, such as "the connection to the mail server failed" or "the mail server replied 421", by its kind or its reply's code, never the reply's text.

**What to do.** Check the provider's status page, and that the server reaches `SMTP_HOST` on `SMTP_PORT`, through its firewall and its name servers. A failure of TLS, or a server that can't upgrade the connection to it, is the settings' or the provider's: compare `SMTP_SECURITY` and the port with what the provider offers ([deploy.md](deploy.md#email)), and set them with `just server-setup <host>`. The emails that wait go out at their jobs' next attempts once the server answers; one whose last attempt failed meanwhile is a dead letter, for [JobsDeadLettered](#jobsdeadlettered). The alert clears once the server takes or refuses a send, and also once 15 minutes pass without a send that failed otherwise: the attempts of an email that waits come further apart, up to an hour, so while few emails are sent, the alert may clear and fire again within one outage.

## ApiTelemetryMissing

**What it means.** No instance of the API exported metrics for five minutes. Either no instance runs, or their telemetry doesn't reach Prometheus.

**Look first.** Whether the API answers its health probes. Then the Telemetry pipeline dashboard: "Can Prometheus reach every service of the stack?" shows whether the collector is up.

**What to do.** If the API is down, restart it, and read its last log lines on stdout, which don't depend on the stack. If it runs, check that its `OTEL_EXPORTER_OTLP_ENDPOINT` names the collector, and that the collector runs and passes metrics to Prometheus.

## TelemetryExportFailing

**What it means.** An instance of the API failed to export spans, log records or metrics in the last 10 minutes. The API goes on serving (OBS-10), but the traces and logs of its requests may be missing, so trace IDs from that time may lead nowhere.

**Look first.** The Telemetry pipeline dashboard: "Is the API failing to export telemetry?" names the instance and the signal.

**What to do.** Check that the collector runs and accepts data, and that the instance can reach `OTEL_EXPORTER_OTLP_ENDPOINT`. A collector that answers slowly fails exports too, once each outlasts the [export timeout](../spec/platform.md#telemetry-export); within it, it fills the instance's queues, which [ApiDroppingTelemetry](#apidroppingtelemetry) reports.

## ApiDroppingTelemetry

**What it means.** An instance of the API dropped spans or log records in the last 10 minutes, which arrived while its queue for them held 2,048 already, as each queue holds ([platform spec](../spec/platform.md#telemetry-export), PLATFORM-106): its exports didn't carry them away as fast as they came, as when the collector answers each export slowly, or not before the export's timeout. The API goes on serving, but the traces and logs of its requests from that time may be missing, so their trace IDs may lead nowhere.

**Look first.** The Telemetry pipeline dashboard: "Is the API dropping telemetry from full queues?" names the instance and the signal, and "Is the API failing to export telemetry?" shows whether its exports fail too, which [TelemetryExportFailing](#telemetryexportfailing) reports.

**What to do.** Follow [TelemetryExportFailing](#telemetryexportfailing)'s checks of the collector, whether or not it fires: a collector that answers each export within the timeout, but slowly, fails none and still fills the queues. If the collector answers promptly, the instance ended more spans or wrote more lines than its exports carried, such as in a burst of requests beyond the capacity at which its queues keep up ([platform spec](../spec/platform.md#telemetry-export)): add replicas ([deploy.md](deploy.md#scaling)). The alert clears once 10 minutes have passed without a drop.

## CollectorDroppingTelemetry

**What it means.** The collector refused telemetry, failed to send it to Tempo, Loki or Prometheus, or to a managed backend, or dropped traces before tail sampling decided them, in the last 10 minutes. The alert's `receiver` names the receiver that refused, and its `exporter` the exporter that failed.

**Look first.** The Telemetry pipeline dashboard: "Is the collector refusing telemetry?", "Is the collector failing to pass telemetry on?" and "Is tail sampling dropping traces before it decides them?" show which step loses data.

**What to do.** Refused data means the collector reached its memory limit: give it more memory. Failed sends name where they go. An exporter of [managed.yaml](../../deploy/observability/collector/managed.yaml) sends to the managed backend: check its provider's status page, and run `just server-check <host>`, which says whether the backend accepts an export signed in with the deployment's token. Any other exporter names a store: check that Tempo, Loki or Prometheus runs and has disk space. Traces dropped early mean the collector holds more undecided traces than its `num_traces`: raise it, along with the collector's memory.
The receiver `otlp/clients` takes the batches that the telemetry proxy forwards for users' clients, and the exporters whose names end in `clients` pass them on. Its memory limit is the lower one, so it stops taking batches before the API's own receiver does: the proxy answers each batch that it doesn't take with 503, which has a client send the batch again later, and the Clients' telemetry dashboard shows what the clients send ([ClientTelemetryBudgetSpent](#clienttelemetrybudgetspent)). <!-- part: client_telemetry -->

<!-- part: client_telemetry -->
## ClientTelemetryNotForwarded

**What it means.** For 15 minutes, the collector took none of the batches of telemetry that the API sent it for users' clients, and neither took nor refused some of them: no connection to its receiver opened, it didn't answer within 5 seconds, or it answered neither with a 2xx nor with a refusal of the batch (CLIENT_TELEMETRY-009). The telemetry proxy answers such a batch with 503 `client_telemetry.collector_unavailable`, which has a client send it again later, so the clients' spans, log records and metrics of that time reach no store unless a client still holds them once the collector takes batches again. Every other route serves. A batch that the collector refuses, with a 4xx other than 404, 405 and 429, isn't among those, so refusals alone raise no alert: the proxy answers each with 400 `client_telemetry.batch_refused`, and "How do the clients' batches end?" on the Clients' telemetry dashboard shows them. A 404 or a 405 is among those: the proxy answers each with 503, so the alert fires while `CLIENT_TELEMETRY_ENDPOINT` names an address that answers every batch so.

**Look first.** The Clients' telemetry dashboard: "How do the clients' batches end?" shows since when. The `ERROR` lines of the 503s name the failure, such as a connection's, or the status that the collector answered.

**What to do.** If the API's own telemetry arrives, the collector runs, and the failure is its receiver of clients' batches or the way to it: check that `CLIENT_TELEMETRY_ENDPOINT` names that receiver, port 4319 of the collector in the reference deployment, and read the collector's log, with `just logs <host> collector`, or `just logs <host> collector-managed` with a managed backend, for what its pipelines of clients' telemetry report. A [CollectorDroppingTelemetry](#collectordroppingtelemetry) whose `receiver` is `otlp/clients` means that the collector refuses the batches at its memory limit. If the API's own telemetry is missing too, follow [ObservabilityServiceDown](#observabilityservicedown). The alert clears once the collector takes a batch.

## ClientTelemetryBudgetSpent

**What it means.** A replica's budget refused batches of clients' telemetry in the last 10 minutes: the batches of consenting users' clients that the replica read took one of its budget's windows of a minute beyond `CLIENT_TELEMETRY_REPLICA_BYTES_PER_MINUTE` bytes (CLIENT_TELEMETRY-008). Until that window ends, the replica answers with 429 each batch that would count in it, a consenting user's that it reads, which has a client send it again later. No other request counts in the budget.

**Look first.** The Clients' telemetry dashboard: "How many bytes of clients' telemetry does each replica forward a minute?" shows whether the volume grew over days, as it does with the product's users, or at once. On the API platform dashboard, "Which rate limits refuse requests?" shows whether `telemetry_user`, a user's own budget, refuses too, as it does while single accounts send all that one may.

**What to do.** A volume that grew with the users needs a larger budget: raise the setting, and give the stores the disk that the new budget can fill ([observability.md](observability.md#what-the-clients-telemetry-adds)). A volume that came at once is a client that sends more than it should, or accounts that someone registered to send it: the budget keeps what they add to the stores within its bound, and each request that a budget refused has a trace, whose span of the request names its user in `user.id`. The alert clears once a replica's budget has refused nothing for 10 minutes.
<!-- end part: client_telemetry -->

## EventsDelayed

**What it means.** Events reach their streams more than 5 seconds after their units of work committed, for the slowest one in a hundred ([platform spec](../spec/platform.md#events-and-the-real-time-hub)). Clients then show changes late. Either the hubs' loads of new events are slow, each replica's hub running one at a time, or units of work that publish wait on each other's commits, which Postgres serializes for every transaction that notifies.

**Look first.** The API platform dashboard: "How long after commit do events reach the streams?" shows since when, and "How many event streams are open?" whether streams grew with it. The Requests dashboard shows whether the database is slow for every request too. A hub whose load fails writes the `WARN` line "the hub couldn't load events", with the database's error.

**What to do.** If the database is slow for everything, follow [DatabaseUnreachable](#databaseunreachable)'s look at its load. If only events are late while streams grew, the replicas hold more streams than their hubs serve: add replicas, so the streams spread over more hubs (OPS-16). The alert clears once the lag falls.

## EventsStalled

**What it means.** A replica's hub has heard of events that committed, but for 5 minutes it couldn't load them ([platform spec](../spec/platform.md#events-and-the-real-time-hub), PLATFORM-035). The streams on that replica receive nothing new, though their clients' requests still answer: a reload shows what the stream left out. The hub records no lag for events it doesn't load, so [EventsDelayed](#eventsdelayed) stays silent.

**Look first.** The API platform dashboard: "How many events does each hub owe its streams?" shows which replica, and since when. That replica writes the `WARN` line "the hub couldn't load events" at every check, every 5 seconds, with the database's error: a pool whose every connection is taken shows as a timeout acquiring one, and then the requests that need one fail too, which "Which routes answer with a 5xx, and with which error?" on the API requests dashboard shows.

**What to do.** If the pool is exhausted, find what holds its connections, as [DatabaseUnreachable](#databaseunreachable)'s look at the database's load does, or raise `DATABASE_MAX_CONNECTIONS` within the budget that [deploy.md](deploy.md#scaling) works out. Any other error names its cause in the line. Restarting the replica closes its streams, whose clients reconnect to another and replay what they missed (REALTIME-004). The alert clears once the hub owes nothing.

## JobsDelayed

**What it means.** A background job that is due has waited more than 10 minutes without a replica claiming it, for 5 minutes ([platform spec](../spec/platform.md#the-job-queue)). Every email waits with it. Either no replica's dispatcher claims jobs, every replica's 4 runs are taken by long runs, or the job's kind is one that no running replica knows, as after a rollback to a release that doesn't have it.

**Look first.** The API platform dashboard: "Are background jobs keeping up?" shows since when the delay grows, and "How do job runs end?" whether runs end at all. A replica that can't claim writes the `WARN` line "can't claim jobs", with the database's error.

**What to do.** If no replica runs, follow [ApiTelemetryMissing](#apitelemetrymissing); if claims fail, [DatabaseUnreachable](#databaseunreachable). If runs end but slowly, "How long do job runs take?" names the kind, and its handler's logs say what it waits on. If the waiting job's kind is one the release doesn't know, the query of [JobsDeadLettered](#jobsdeadlettered), with `state = 'waiting'`, shows it: deploy a release that knows it. The alert clears once the queue catches up.

## JobsDeadLettered

**What it means.** A job failed on each of its attempts, or with a failure that every attempt would meet, and moved to the dead-letter state, where nothing retries it ([platform spec](../spec/platform.md#the-job-queue)). For `platform.send_email`, an email didn't go out: a mail server that refused it for good, one that didn't answer for the hours its retries span, which [MailServerUnreachable](#mailserverunreachable) pages for first, or an email that the API couldn't build, such as for a recipient that isn't an address a mail server takes ([platform spec](../spec/platform.md#sending-email)).

**Look first.** The API's `ERROR` line "job dead-lettered" names the kind and the failure. With the bundled database, this lists the dead letters, without their payloads, which hold personal data such as an email's recipient:

```sh
ssh <host> "docker exec deployment-postgres-1 psql --username=postgres --dbname=api --command=\"SELECT id, kind, attempts, dead_at, failure FROM jobs WHERE state = 'dead' ORDER BY dead_at DESC\""
```

With a managed database, run the same query in the provider's console. Add `payload` to the columns only when the failure doesn't say enough: it holds what the job needed, such as an email's recipient.

**What to do.** Fix what the failure names, such as the mail server's settings or credentials with `just server-setup <host>`, or the sender's domain at the provider. A dead letter's remedy is the operation that caused it, done again, such as the request that sent an email, which sends a new one. The sweep deletes dead letters after `JOB_DEAD_LETTER_RETENTION_DAYS`, and the alert clears an hour after the last one.

## PeriodicJobFailing

**What it means.** A periodic job failed at least twice within 3 hours, as an hourly job that keeps failing does, or twice within 49 hours with no run that succeeded, as a daily job that keeps failing does from an hour after its second failure. Each failed run waits for its next interval, so what it deletes stays meanwhile, and a request meets it as its module's spec says of a row that waits for its job, such as an expired session's token, which answers as expired rather than unknown until `auth.expire_sessions` deletes it ([auth spec](../spec/auth.md#a-users-sessions), [platform spec](../spec/platform.md#the-job-queue)).

**Look first.** The API's `WARN` line "periodic job failed, and runs again at its next interval" names the job and the failure. When `auth.reencrypt_secrets` fails to open a stored secret, `ENCRYPTION_KEYS` holds another key under that secret's version than the one that encrypted it, as after a restore onto a server whose setup generated a new ring: give the server the ring that the backups' secrets were stored under ([backups](backups.md#what-to-keep-off-the-server)).

**What to do.** A failure of the database clears with it: follow [DatabaseUnreachable](#databaseunreachable).
One of the files bucket clears with it: follow [StorageUnreachable](#storageunreachable). <!-- part: files -->
A run that passes its time limit, which its job's declaration sets, stops where it was. The sweeps delete in batches, so the next run continues where it stopped.
So does `files.delete_unreferenced_objects`. `files.find_unreferenced_objects` keeps what it recorded, but its next run lists the bucket from its start again, so a bucket whose listing takes longer than the hour has objects that it never reaches, which no deletion then removes: its runs fail each day, and the job needs a longer limit or a listing that resumes. <!-- part: files -->
The alert clears once neither of its conditions holds: the first ends 3 hours after the failure before the last, and the second once a run succeeds, or 49 hours after the failure before the last. So the alert of an hourly job that kept failing clears 2 hours after its last failure, if a run has succeeded by then, and a daily job's once a run succeeds.

## BackupArchivingFailing

**What it means.** The bundled Postgres failed to archive a segment of its write-ahead log to the backup bucket, and no archive has succeeded since. Postgres keeps each segment until it is archived, so its disk fills, and the latest moment a restore can reach stops advancing: losing the server now loses everything since the last archive that succeeded (OPS-15). Meanwhile a restore refuses, unless it finds Postgres not running or still replaying the log, as [Restoring](backups.md#restoring) says.

**Look first.** `just logs <host> postgres` shows pgBackRest's error for each attempt, such as a key pair the bucket refuses, an endpoint that doesn't answer, or a certificate the deployment doesn't trust.

**What to do.** Fix what the error names: the bucket itself, after which Postgres retries on its own, or the bucket's key pair or its settings, with `just server-setup <host>`, then `just deploy <host> <version> --allow-restart`, since Postgres reads them only as it starts ([deploy.md](deploy.md#restarting-a-service-that-runs-once)); `just server-check <host>` reads the bucket as Postgres does. A bucket whose certificate a private authority signed needs that authority's certificate, `certs/backup-ca.crt`, which `just server-setup <host>` asks for, and the same deploy then applies ([backups.md](backups.md#the-bucket)). The alert clears with the next archive that succeeds. Then take a base backup with `just backup <host>`, so a restore needs no segment from before the failure.

## BaseBackupTooOld

**What it means.** The bundled database's daily base backup hasn't completed for more than 26 hours and 10 minutes. A restore still reaches every moment the archived write-ahead log covers, but it replays more of the log the older the backup it starts from, so it takes longer.

**Look first.** `just logs <host> backup` shows a line for each scheduled backup that failed, after pgBackRest's error. If the backup service isn't running, `just status <host>` shows how it exited, and its logs why.

**What to do.** Fix what the error names, often the same as for [BackupArchivingFailing](#backuparchivingfailing), then take a full backup at once with `just backup <host>`. The alert clears when it completes. A backup service that stopped starts again with the next deploy, such as `just deploy <host> <version>`, which starts the services that the replicas use ([deploy.md](deploy.md#deploying-a-release)).

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

**What to do.** A replica whose memory only grows points to a leak: deploy the release before it ([deploy.md](deploy.md#rolling-back)), and report it. Otherwise lower `API_REPLICAS` or `API_MEMORY`, or move to a server with more memory.
