# The retention, in days, of the next expire of the bundled database's backups (OPS-15), from
# pgBackRest's info in JSON: the deployment's BACKUP_RETENTION_DAYS, `$days`, raised to keep each
# full backup that a restore started from until a backup that started after the restore ended
# `$days` days before `$now`, in seconds since the epoch. The moments of a restored timeline before
# its first backup ends replay from the backup the restore started from, whose next full backup
# may lie on the timeline the restore abandoned, and be old already; once a backup taken after the
# restore is `$days` days old, so are those moments. A restore marks the full backup with the
# annotation `restore-<time>`, its time in seconds since the epoch.
#
# pgBackRest expires a full backup once the next full backup ended more than the retention ago,
# so a retention of a day more than that backup's age, in whole days, keeps it. The filter prints
# the retention, then each full backup that it keeps beyond `$days`, with the time of the restore
# that marked it.
(.[0].backup // []) as $backups
| ($backups | map(select(.type == "full"))) as $fulls
| ($now - $days * 86400) as $cutoff
| [
    $fulls[]
    | . as $full
    | (.annotation // {}) | to_entries[]
    | select(.key | test("^restore-[0-9]+$"))
    | (.key | ltrimstr("restore-") | tonumber) as $restored
    | select(any($backups[]; .timestamp.start > $restored and .timestamp.stop < $cutoff) | not)
    | [$fulls[] | .timestamp.stop | select(. > $full.timestamp.stop)]
    | min
    | select(. != null)
    | {label: $full.label, restored: $restored, retention: ((($now - .) / 86400 | floor) + 1)}
    | select(.retention > $days)
  ]
| ([$days] + map(.retention) | max), (.[] | "\(.label) \(.restored)")
