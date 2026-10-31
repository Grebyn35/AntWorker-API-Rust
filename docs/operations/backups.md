# Backups

The reference deployment's bundled Postgres is backed up continuously, to a bucket off its server, so that losing the server loses at most about a minute of data, and a restore can return the database to any moment before a mistake (D44, OPS-17). This document says what is backed up, the recovery targets, what the bucket needs, how to take a backup and restore one, and what to keep off the server. With a managed database, its provider backs it up instead ([below](#a-managed-database)). [deploy.md](deploy.md) describes the deployment.

## What is backed up

pgBackRest, in the release's Postgres image, backs up the bundled database in two ways:

- **The write-ahead log**, Postgres's record of every change, is archived to the bucket as each segment of it, 16 megabytes, fills, and at least once a minute while the database changes: Postgres runs `pgbackrest archive-push` for each segment, and keeps the segment until the archive succeeds.
- **Base backups**, copies of the whole database, are taken by the `backup` service every day at `BACKUP_HOUR` o'clock UTC: a full backup on Sundays, and a differential one, of what changed since the last full backup, on the other days. The first deploy that runs the bundled database takes the first full backup.

A restore starts from a base backup that ended before the moment it restores to, the latest on the timeline the database had then, as [Restoring](#restoring) says, and replays the archived log up to that moment. The bucket keeps the backups and the log that reach back `BACKUP_RETENTION_DAYS` days: a full backup expires only once a newer one is that old, so the full backups that `just backup` and the restores take never shorten the reach.

pgBackRest compresses everything with zstd, and encrypts it with AES-256 before it leaves the server, with the passphrase in `secrets/backup_cipher_pass` (OPS-20). The bucket's provider sees nothing it can read.

The backups don't hold the deployment's settings and secrets, which the next sections say to keep elsewhere, nor the observability stack's metrics, logs and traces, which a new server starts without. Caddy obtains new certificates on its own.

## Recovery targets

| Target | The reference deployment's |
|---|---|
| Data lost when the server is lost | At most the changes of the last minute, plus however long the last archive took to reach the bucket |
| Moments a restore reaches | Every moment from `BACKUP_RETENTION_DAYS` days ago, or from the first deploy that ran the bundled database if that came later, until the restore begins, those while an earlier restore ran included. A moment after the last change that the bucket holds on its timeline, such as one after the server's loss, is reached as that change, which the restore names: what the server hadn't archived is lost, as the row above says |
| Time a restore takes | The wait for the second check of step 1 of [Restoring](#restoring), as long as one segment of the log takes to reach the bucket, then the time to download the base backup it starts from, and to replay the log from it to the moment, which the daily base backup bounds to a day of changes. A moment after the last change that the bucket holds on its timeline takes a second restore of the backup and a second replay, as step 3 says. The database is unavailable meanwhile. The first check waits as long before that, while the API serves |

The time a restore takes grows with the database and with its rate of change, so measure it on the server, as [Restoring onto a new server](#restoring-onto-a-new-server) does. For scale: on a server with 2 virtual CPUs and 4 gigabytes of memory, a restore of the latest state of a new database of 30 megabytes, from a bucket at Backblaze B2, took 45 seconds, and the API was unavailable for about 16 of them: restoring the data took 6 seconds, and stopping and starting the services the rest. The checks' waits add to that, the first while the API still serves. The full backup that ends a restore took 20 seconds, while the API served.

## The bucket

The bucket is any provider's that speaks the S3 protocol over HTTPS, on another machine than the server. Give it:

- encryption at rest, which most providers apply to every bucket;
- a key pair that reaches only this bucket, in `secrets/backup_s3_key` and `secrets/backup_s3_key_secret`;
- the settings `BACKUP_S3_ENDPOINT`, `BACKUP_S3_REGION` and `BACKUP_S3_BUCKET`, and `BACKUP_S3_URI_STYLE=path` for a provider whose bucket names don't appear in its host names, which [deploy/settings.env.example](../../deploy/settings.env.example) describes.

pgBackRest manages what the bucket holds, and deletes the backups that retention lets go, so set no lifecycle rule that deletes the bucket's current files. The bucket must keep no noncurrent versions, S3's name for the old versions that a bucket with versioning keeps of a file deleted or replaced: they restore nothing, and the bucket would go on storing every backup that retention let go. Turn versioning off, or, with a provider that keeps versions whatever a bucket's settings, as Backblaze B2 does, give the bucket a lifecycle rule that expires noncurrent versions, such as B2's "Keep only the last version of the file". A bucket of the operator's own, whose certificate a private authority signed, is trusted through that authority's certificate in `certs/backup-ca.crt` of the deployment's directory.

## Taking a backup

```
just backup production
```

takes a full base backup at once, such as before a change that's hard to undo. It doesn't stop the API. `production` is the server's `Host` in `~/.ssh/config`, as [deploy.md](deploy.md#setting-up-a-server) says, or its `user@address`.

## Restoring

```
just restore production "2026-10-25 10:41:31+00"
```

restores the database as it stood at that moment, a time with its offset from UTC. Without a moment, `just restore production` restores the latest state the backups hold.

Each restore begins a timeline, Postgres's name for a history of the database that branches off at a restore, so a database that was restored has had several timelines, and a moment belongs to the timeline that was live then: the newest one that had begun. A restore first reads, on your machine, what the bucket records of them, and picks the timeline live at its moment, or the latest one for the latest state, and the latest backup that ended before the moment on that timeline, or on a timeline it descends from, before it branched off, and says which; a bucket it can't read, a moment that no such backup reaches, and one that the server's clock hasn't reached are refused before anything stops, whatever the restore's options. The restore then runs on the server, through the `deploy.sh` that the last deploy installed:

1. Before anything stops, pgBackRest's check ends the segment of the log that Postgres writes, and waits until it reaches the bucket. If the check fails on the database the bucket backs up, as when its log doesn't reach the bucket, the restore shows pgBackRest's error and refuses: restoring would leave the changes that the bucket lacks only in the data that step 2 moves aside, while the API serves the restored database, and no restore could undo it. Fix what the error names, as for [BackupArchivingFailing](runbooks.md#backuparchivingfailing), and restore again; or restore anyway with `--allow-unarchived`, as `just restore production --allow-unarchived`. The restore then stops the API's replicas and the `backup` service. Once the check has passed, it commits a transaction that changes nothing, which a second check archives: a replay to a moment stops before the first commit after the moment, so that commit lets a restore to any moment before it, one after the last change included, replay the log once, rather than twice, as step 3 does for a moment that no commit follows. If the transaction or the second check fails, as when the log stops reaching the bucket after the first check, the restore starts the replicas and the `backup` service again, with the database as it found it, and refuses, unless `--allow-unarchived` lets it go on. On a database that the bucket doesn't back up, as on a new server, the restore goes on, and nothing commits; with Postgres not running, as after a restore whose database couldn't start, nothing is checked, and nothing commits. It then stops Postgres. Until the replicas start again, in step 4, Caddy answers each request with the 503 problem `request.unavailable` and `Retry-After` (OPS-05).
2. It moves the data directory aside, to `before-restore-<time>` in the volume `deployment_postgres`, and restores into a new, empty one. Postgres starts only after the restore succeeds; on an empty directory it would create an empty database.
3. It starts Postgres, which replays the archived log from that backup up to the moment, along that timeline, then begins a new timeline, so the log it archives from then on never mixes with the log it replaced. If no change that the bucket holds follows the moment on that timeline, as for a moment while an earlier restore ran, or one after a lost server's last archived change, Postgres ends the replay with an error, without reaching the moment. Nothing the bucket holds changed after the timeline's last change, so the database stood at the moment as at that change: the restore restores the backup again, has Postgres replay the timeline to its end, and names the time of that change.
4. It runs `migrate` with the live release, and starts the live replicas again.
5. It takes a full backup of the restored database while the API serves, as the scheduled backups do, so a later restore starts from the restored database's timeline; Postgres archives what the replicas write meanwhile. If that backup fails, the restore says so, with the API serving: take one with `just backup`.

CI runs each restore that OPS-17 names.

A restore that went back too far, or shouldn't have run, is undone by another once it has named a moment: the backups still reach every moment that it went back past, up to the moment its transaction of step 1 began, and a restore to that moment returns the database to how the restore found it. Once the second check of step 1 passes, the restore names that moment, and names it again once it has restored the database:

```
deploy: a restore to 2026-10-25 10:41:31.913582+00 undoes this one
```

A restore to a moment while this one ran, after the moment it names, returns the database to how the restore found it as well: nothing commits on the database it replaces after its transaction, so step 3 replays that database's timeline to the transaction. A restore on a new server, one that `--allow-unarchived` let go on, and one that finds Postgres not running name no moment. The data that the restore replaced also stays in the volume, and `just status` lists it, with its size, until it's removed: no deploy removes it (D57). Once the restored database is right, remove it on the server, with the name the restore printed:

```
docker run --rm --volume deployment_postgres:/data --entrypoint rm "$(sed -n 's/^POSTGRES_IMAGE=//p' ~/deployment/state/deploy.env)" -r /data/18/before-restore-20261025T104131Z
```

A restore that fails once it moved the data directory aside leaves the API down, and its output says where and why:

- **Before the restored database starts**, as when pgBackRest can't restore it, Postgres is stopped, or keeps failing to start. Fix the cause, such as a disk too full for the restored data, and run the restore again, which moves aside what it finds in turn. Or go back to the data as it was before the restore: stop Postgres, put the directory back, and deploy the live release again, which starts Postgres:

  ```
  docker stop deployment-postgres-1 && docker run --rm --volume deployment_postgres:/data --entrypoint sh "$(sed -n 's/^POSTGRES_IMAGE=//p' ~/deployment/state/deploy.env)" -c 'rm -rf /data/18/docker && mv /data/18/before-restore-20261025T104131Z /data/18/docker'
  ```
- **While the database still recovers after 10 minutes,** Postgres goes on recovering. Once its log, which `just logs production postgres` shows, says it accepts connections, deploy the live release again, which runs `migrate` and starts the replicas.
- **In `migrate`, or while the replicas start,** Postgres runs the restored database. Fix the cause, and deploy the live release again.

Putting the data back by hand is for a restore whose database didn't start only: once Postgres runs the restored database, the backups follow it, so undo the restore with another, as above.

### Restoring onto a new server

After losing the server, set up a new one with `just server-setup`, as [deploy.md](deploy.md#setting-up-a-server) says, answering with the lost server's settings, its bucket's key pair, and its copies of `backup_cipher_pass` and `postgres_password`, which the restored database keeps, and deploy the release that ran. The deploy stops before its replicas start, because the new server's database, which Postgres has just created empty, isn't the one whose backups the bucket holds: pgBackRest refuses it. Then run `just restore` without a moment, and `just deploy` again. CI does the same, on a new server of its own set up from a lost one's settings, secrets and bucket, and checks that the first deploy stops before any replica starts, and that the deploy after the restore serves the lost server's data (OPS-17).

## What to keep off the server

Keep a copy of these where the server's loss doesn't reach them, such as a password manager:

- **`secrets/backup_cipher_pass`.** Without it, no backup can be read, by you or anyone else.
- **`secrets/postgres_password`**, the database's superuser password, which a restored database keeps. `bootstrap` sets the other roles' passwords from the URLs that the deploy writes.
- **The bucket's key pair**, and **`settings.env`**, unless you can make them again.

## Alerts

Two alerts watch the backups, from the times that the `pgbackrest` wrapper and the `backup` service set on files of the volume `deployment_backup-status`, which the collector reads (OBS-14). With `TELEMETRY=off`, no collector runs, so nothing watches the backups: `just status <host>` shows when they last succeeded, and `just logs <host> backup` their errors.

- [BackupArchivingFailing](runbooks.md#backuparchivingfailing) pages when an archive of the write-ahead log has failed more recently than one succeeded, for five minutes.
- [BaseBackupTooOld](runbooks.md#basebackuptooold) opens a ticket when no base backup has completed for 26 hours.

## A managed database

With `DATABASE=managed`, the deployment runs no Postgres, and backs nothing up. The provider must provide point-in-time recovery, for as long as you need to reach back, which every major provider offers, such as Amazon Relational Database Service (RDS), Google Cloud SQL, Azure Database for PostgreSQL and Neon (OPS-17). It must also encrypt what it stores (OPS-20): where a provider makes that a choice, such as when an RDS instance is created, choose it. A provider restores into a new database, with a new address: give its URLs to `just server-setup`, and deploy again.
