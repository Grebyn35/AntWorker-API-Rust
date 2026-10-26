# Backups

The reference deployment's bundled Postgres is backed up continuously, to a bucket off its server, so that losing the server loses at most about a minute of data, and a restore can return the database to any moment before a mistake (D44, OPS-17). This document says what is backed up, the recovery targets, what the bucket needs, how to take a backup and restore one, and what to keep off the server. With a managed database, its provider backs it up instead ([below](#a-managed-database)). [deploy.md](deploy.md) describes the deployment.

## What is backed up

pgBackRest, in the release's Postgres image, backs up the bundled database in two ways:

- **The write-ahead log**, Postgres's record of every change, is archived to the bucket as each segment of it, 16 megabytes, fills, and at least once a minute while the database changes: Postgres runs `pgbackrest archive-push` for each segment, and keeps the segment until the archive succeeds.
- **Base backups**, copies of the whole database, are taken by the `backup` service every day at `BACKUP_HOUR` o'clock UTC: a full backup on Sundays, and a differential one, of what changed since the last full backup, on the other days. The first deploy that runs the bundled database takes the first full backup.

A restore starts from the newest base backup before the moment it restores to, and replays the archived log up to that moment. The bucket keeps the backups and the log that reach back `BACKUP_RETENTION_DAYS` days: a full backup expires only once a newer one is that old, so the full backups that `just backup` and the restores take never shorten the reach.

pgBackRest compresses everything with zstd, and encrypts it with AES-256 before it leaves the server, with the passphrase in `secrets/backup_cipher_pass` (OPS-20). The bucket's provider sees nothing it can read.

The backups don't hold the deployment's settings and secrets, which the next sections say to keep elsewhere, nor the observability stack's metrics, logs and traces, which a new server starts without. Caddy obtains new certificates on its own.

## Recovery targets

| Target | The reference deployment's |
|---|---|
| Data lost when the server is lost | At most the changes of the last minute, plus however long the last archive took to reach the bucket |
| Moments a restore reaches | Any moment from `BACKUP_RETENTION_DAYS` days ago, or from the first deploy that ran the bundled database if that came later, to the last archived change |
| Time a restore takes | The time to download the newest base backup before the moment, and to replay the log from it to the moment, which the daily base backup bounds to a day of changes. The database is unavailable meanwhile |

The time a restore takes grows with the database and with its rate of change, so measure it on the server, as [Restoring onto a new server](#restoring-onto-a-new-server) does.

## The bucket

The bucket is any provider's that speaks the S3 protocol over HTTPS, on another machine than the server. Give it:

- encryption at rest, which most providers apply to every bucket;
- a key pair that reaches only this bucket, in `secrets/backup_s3_key` and `secrets/backup_s3_key_secret`;
- the settings `BACKUP_S3_ENDPOINT`, `BACKUP_S3_REGION` and `BACKUP_S3_BUCKET`, and `BACKUP_S3_URI_STYLE=path` for a provider whose bucket names don't appear in its host names, which [deploy/settings.env.example](../../deploy/settings.env.example) describes.

pgBackRest manages what the bucket holds, and deletes the backups that retention lets go, so set no lifecycle rule that deletes the files a bucket shows. Versioning adds nothing: it keeps each deleted backup as an old version, which costs storage and restores nothing. A provider that keeps versions whatever a bucket's settings, as Backblaze B2 does, only hides a file that pgBackRest deletes: give its bucket a lifecycle rule that deletes a hidden file's versions, B2's "Keep only the last version of the file", or the bucket goes on storing every backup that retention let go. A bucket of the operator's own, whose certificate a private authority signed, is trusted through that authority's certificate in `certs/backup-ca.crt` of the deployment's directory.

## Taking a backup

```
just backup deploy@203.0.113.7
```

takes a full base backup at once, such as before a change that's hard to undo. It doesn't stop the API.

## Restoring

```
just restore deploy@203.0.113.7 "2026-10-25 10:41:31+00"
```

restores the database as it stood at that moment, a time with its offset from UTC. Without a moment, `just restore deploy@203.0.113.7` restores the latest state the backups hold. A restore runs on the server, through the live release's `deploy.sh`:

1. It stops the API's replicas and the `backup` service, then Postgres. The API is unavailable until the restore ends.
2. It moves the data directory aside, to `before-restore-<time>` in the volume `deployment_postgres`, and restores into a new, empty one. Postgres starts only after the restore succeeds; on an empty directory it would create an empty database.
3. It starts Postgres, which replays the archived log up to the moment, then begins a new timeline, Postgres's name for a history of the database that branches off at a restore, so the log it archives from then on never mixes with the log it replaced.
4. It takes a full backup of the restored database, runs `migrate` with the live release, and starts the live replicas again.

CI restores to a moment between two writes, then checks that the first one is there, that the second isn't, and that the API serves through Caddy (OPS-17).

The data that the restore replaced stays in the volume, so a restore can be undone. Once the restored database is right, remove it on the server, with the name the restore printed:

```
docker run --rm --volume deployment_postgres:/data --entrypoint rm "$(sed -n 's/^POSTGRES_IMAGE=//p' ~/deployment/state/deploy.env)" -r /data/18/before-restore-20261025T104131Z
```

A restore that fails once it moved the data directory aside leaves Postgres stopped, and the API with it; its output says why. Fix the cause, such as a moment that the backups don't reach, and run the restore again, which moves aside what it finds in turn. To go back to the data as it was instead, put the directory back, and deploy the live release again, which starts Postgres:

```
docker run --rm --volume deployment_postgres:/data --entrypoint sh "$(sed -n 's/^POSTGRES_IMAGE=//p' ~/deployment/state/deploy.env)" -c 'rm -rf /data/18/docker && mv /data/18/before-restore-20261025T104131Z /data/18/docker'
```

### Restoring onto a new server

After losing the server, set up a new one as [deploy.md](deploy.md#setting-up-a-server) says, with the same `settings.env` and the copies of `secrets/`, and deploy the release that ran. The deploy stops before its replicas start, because the new server's database, which Postgres has just created empty, isn't the one whose backups the bucket holds: pgBackRest refuses it. Then run `just restore` without a moment, and `just deploy` again. CI does the same, on a new server of its own set up from a lost one's settings, secrets and bucket, and checks that the first deploy stops before any replica starts, and that the deploy after the restore serves the lost server's data (OPS-17).

## What to keep off the server

Keep a copy of these where the server's loss doesn't reach them, such as a password manager:

- **`secrets/backup_cipher_pass`.** Without it, no backup can be read, by you or anyone else.
- **The rest of `secrets/`**: the bucket's key pair, and the bundled database's passwords, which the restored database's roles keep.
- **`settings.env`.**

## Alerts

Two alerts watch the backups, from the times that the `pgbackrest` wrapper and the `backup` service set on files of the volume `deployment_backup-status`, which the collector reads (OBS-14). With `TELEMETRY=off`, no collector runs, so nothing watches the backups: read `docker compose logs backup` on the server instead.

- [BackupArchivingFailing](runbooks.md#backuparchivingfailing) pages when an archive of the write-ahead log has failed more recently than one succeeded, for five minutes.
- [BaseBackupTooOld](runbooks.md#basebackuptooold) opens a ticket when no base backup has completed for 26 hours.

## A managed database

With `DATABASE=managed`, the deployment runs no Postgres, and backs nothing up. The provider must provide point-in-time recovery, for as long as you need to reach back, which every major provider offers, such as Amazon Relational Database Service (RDS), Google Cloud SQL, Azure Database for PostgreSQL and Neon (OPS-17). It must also encrypt what it stores (OPS-20): where a provider makes that a choice, such as when an RDS instance is created, choose it. A provider restores into a new database, with a new address: write its URLs into `secrets/`, and deploy again.
