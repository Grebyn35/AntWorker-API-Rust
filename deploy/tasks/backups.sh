# The bundled database's backups and restores (OPS-15): a backup on demand, a restore to a
# moment or to the latest state, from the backups that the bucket holds or from those it held at
# a time (OPS-22), and what a restore plans from.

backup() {
    lock
    read_settings
    if [ "$database" != bundled ]; then fail "the database is managed, and its provider backs it up"; fi
    full_backup
    say "took a full backup"
}

# Starts Postgres on the data directory that a restore wrote, and waits until it has replayed the
# log and left recovery. It returns 1 when Postgres ended the replay without reaching the moment
# it replays to, as its log since this start says, and fails the restore when Postgres couldn't
# fetch a segment of the log from the bucket, which ends its start, when Postgres doesn't
# become healthy, as Compose's wait would, or when its replay hasn't advanced for 10 minutes: a
# replay of a day of changes may take longer than that, and only its end tells whether it reaches
# the moment. Its caller tests it as a condition, where the shell ignores -e, so it checks each
# command itself.
recovered() {
    compose up --detach --quiet-pull postgres || fail "couldn't start Postgres on the restored data, with the error above"
    # The start's time in nanoseconds, after the last line that an earlier run of the container
    # wrote, which a clock read in whole seconds might not be.
    if ! container=$(service_container postgres) || ! started=$(docker inspect --format '{{.State.StartedAt}}' "$container"); then
        fail "couldn't find when Postgres started, with the error above"
    fi
    waited=0
    replayed=""
    until [ "$(compose exec -T --user postgres postgres psql -qAt -c 'SELECT pg_is_in_recovery()' 2> /dev/null)" = f ]; do
        # Docker's state before the log: once Docker reports that a run of the container ended,
        # its log holds every line the run wrote, such as the one that ends a replay that falls
        # short of its moment. Docker reports the container unhealthy as soon as a run ends,
        # whatever its checks found, so only checks that fail as many times as the health check
        # allows, while Postgres runs, say that it fails them.
        if ! run_state=$(docker inspect --format '{{.State.Status}} {{if and (eq .State.Health.Status "unhealthy") (ge .State.Health.FailingStreak .Config.Healthcheck.Retries)}}failing{{else}}checked{{end}}' "$container"); then
            fail "couldn't find whether Postgres runs, with the error above"
        fi
        if ! logged=$(docker logs --since "$started" "$container" 2>&1); then
            printf '%s\n' "$logged" >&2
            fail "couldn't read Postgres's log, with the error above"
        fi
        case $logged in
            *'recovery ended before configured recovery target was reached'*) return 1 ;;
            *'could not restore file'*)
                compose logs --tail 30 postgres >&2 || true
                fail "Postgres couldn't fetch a segment of the log from the bucket, as the end of its log, above, shows with pgBackRest's error; once the bucket answers, run the restore again"
                ;;
        esac
        case $run_state in
            'restarting '* | 'exited '* | 'dead '* | *' failing')
                compose logs --tail 30 postgres >&2 || true
                fail "Postgres doesn't start on the restored data; the end of its log, above, says why"
                ;;
        esac
        # How far the replay has come, once Postgres accepts connections during it.
        position=$(compose exec -T --user postgres postgres psql -qAt -c 'SELECT pg_last_wal_replay_lsn()' 2> /dev/null) || position=""
        if [ "$position" != "$replayed" ]; then
            replayed=$position
            waited=0
        fi
        waited=$((waited + 1))
        if [ "$waited" -gt 600 ]; then
            compose logs --tail 30 postgres >&2 || true
            fail "the replay of the restored database hasn't advanced for 10 minutes, as the end of its log, above, shows"
        fi
        sleep 1
    done
}

# Starts again what a restore that refuses stopped, when Postgres runs the database it found:
# the live replicas, if any ran, and the backups. When Postgres doesn't, as after a restore that
# failed, it starts nothing: the backups would start Postgres on the data that restore left.
serve_again() {
    if [ "$serving" != yes ]; then return; fi
    say "starting ${live:+the API and }the backups again, with the database as the restore found it"
    if [ -n "$live" ]; then
        compose up --detach --wait --quiet-pull --no-deps --scale "api-$live=$replicas" "api-$live"
    fi
    compose up --detach --wait --quiet-pull backup
}

# Fails unless the buckets keep what is deleted in them (OPS-22): one that keeps nothing holds
# nothing of an earlier time.
keeps_deleted() {
    if [ "$(setting BUCKETS_KEEP_DELETED_DAYS 7)" -eq 0 ]; then
        fail "BUCKETS_KEEP_DELETED_DAYS is 0, so the backups' bucket keeps nothing of what it held at an earlier time; a restore without --bucket-as-of reads the backups that it holds"
    fi
}

# Restores the bundled database as it stood at the moment $1, or, with none, at the latest moment
# that the backups hold, from the backup $2, along the timeline $3, which `just restore` plans.
# $4 is yes when --allow-unarchived lets it restore although the database's log doesn't reach
# the bucket. $5, when --bucket-as-of gives one, is the time at which the bucket held the backups
# that the restore reads, as pgBackRest takes a time, whatever the bucket holds now (OPS-22).
restore() {
    moment=$1
    set=$2
    timeline=$3
    anyway=$4
    held=$5
    if [ -z "$set" ] || [ -z "$timeline" ]; then
        fail "a restore takes the backup and the timeline that just restore plans from restore-inputs"
    fi
    lock
    read_settings
    if [ "$database" != bundled ]; then fail "the database is managed: restore it through its provider"; fi
    if [ -n "$held" ]; then keeps_deleted; fi
    live=$(live_color)
    # The live replicas' image, which deploy.env keeps for their color, while its image for
    # `migrate` is that of the last deploy, which may have failed.
    case $live in
        blue) live_image=$(sed -n 's/^API_IMAGE_BLUE=//p' "$state/deploy.env") ;;
        green) live_image=$(sed -n 's/^API_IMAGE_GREEN=//p' "$state/deploy.env") ;;
    esac
    if [ -n "$held" ]; then
        # The backups that the restore reads are none that the bucket holds now: the database
        # that it replaces has no backups there to check, the backup that it starts from is none
        # that an expire reaches, and the bucket's backups begin anew after it, so it checks
        # nothing, marks nothing, records no timeline's beginning, and no restore undoes it.
        say "restoring from the backups as the bucket held them at $held, whatever it holds now: no restore returns to the database that this one replaces"
        say "stopping the backups and the API's replicas"
        compose stop backup
        compose stop api-blue api-green
        undo=""
    else
        prepare_restore
    fi
    restore_data
}

# What a restore from the backups that the bucket holds does before Postgres stops: it checks
# that the database's log reaches the bucket, marks the full backup that it starts from, stops
# the backups and the replicas, commits the transaction that a later restore returns to, and
# records when its timeline begins.
prepare_restore() {
    # Before anything stops, pgBackRest's check ends the log's segment, and waits until it reaches
    # the bucket. Its error 28 is a database the bucket doesn't back up, such as the empty one
    # Postgres creates on a new server, where the restore goes on. On the database the bucket
    # backs up, any other error, such as a log that doesn't reach the bucket within the check's
    # timeout, refuses the restore, unless --allow-unarchived lets it go on: the changes that the
    # bucket lacks would stay only in the data that moves aside, while the API serves the
    # restored database, and no restore would return to the database this one replaces. With
    # Postgres not running, as after a restore whose database couldn't start, or still replaying
    # the log, as after a restore that failed during its replay, there's nothing to check: a
    # database in recovery takes no writes, and holds only what the bucket gave it.
    refusal="the bucket may lack the database's latest changes, which a restore would leave only in the data it moves aside, and no restore could undo it. Fix what the error names, and restore again; or, to restore anyway, add --allow-unarchived"
    archived=no
    serving=no
    if compose exec -T --user postgres postgres pg_isready --quiet > /dev/null 2>&1 &&
        [ "$(compose exec -T --user postgres postgres psql -qAt -c 'SELECT pg_is_in_recovery()' 2> /dev/null)" != t ]; then
        serving=yes
        status=0
        checked=$(compose exec -T --user postgres postgres pgbackrest --stanza=db check 2>&1) || status=$?
        case $status in
            0) archived=yes ;;
            28)
                printf '%s\n' "$checked"
                say "the bucket backs up another database, as on a new server, so no restore returns to the database this one replaces"
                ;;
            *)
                printf '%s\n' "$checked"
                if [ "$anyway" != yes ]; then fail "pgBackRest's check failed, with the error above, so $refusal"; fi
                say "pgBackRest's check failed, with the error above; restoring anyway, as --allow-unarchived asks, so what the bucket lacks of the database stays only in the data that moves aside"
                ;;
        esac
    fi
    # The full backup that the restore starts from gets the annotation `restore-<time>`, a time
    # before any backup of the restored database starts, by the server's clock, which pgBackRest
    # stamps the backups with too, through Postgres: the moments of the restored timeline before
    # its first backup ends replay from the backup the restore starts from, and each expire keeps
    # it until a backup that started after that time is as old as the retention
    # (deploy/postgres/retention.jq). It's written once the backups stop, so no expire runs
    # meanwhile, and before the replicas stop, so a mark that fails, as when the bucket stops
    # answering, or when an expire removed the backup after the restore planned it, refuses the
    # restore before the replicas stop. It starts the backups again when Postgres runs the database
    # the restore found, as serve_again does: otherwise their start would start Postgres on the
    # data that a restore that failed left. A restore that ends later, refused or failed, leaves
    # the mark, which keeps the backup, as a restore's does, until a backup taken after it is as
    # old as the retention.
    say "stopping the backups, and marking the full backup ${set%%_*}, which the restore starts from"
    compose stop backup
    at=$(date +%s)
    if ! compose run --rm --no-deps --quiet-pull --user postgres --entrypoint pgbackrest postgres \
        --stanza=db annotate --set="${set%%_*}" --annotation="restore-$at=$at"; then
        unmarked="couldn't mark the full backup ${set%%_*}, which the restore starts from, with pgBackRest's error above, so refused the restore before the replicas stopped"
        if [ "$serving" != yes ]; then
            fail "$unmarked, and started nothing, since Postgres doesn't run the database that the restore found; once the bucket answers, restore again"
        fi
        compose up --detach --wait --quiet-pull backup
        fail "$unmarked${live:+, with the API serving}, and started the backups again; once the bucket answers, restore again"
    fi
    say "the bucket keeps the full backup ${set%%_*}, which the restore starts from, until a backup taken after the restore is as old as the retention"
    say "stopping the API's replicas"
    compose stop api-blue api-green
    # A replay to a moment stops before the first commit or rollback after it, and Postgres ends
    # one that neither follows, such as one to a moment after the last change, with an error,
    # which only a second replay, below, gets past. So, once the replicas stop, and when the
    # check passed, a transaction that changes nothing commits, given an ID by
    # pg_current_xact_id(), without which it would write no commit: a restore to any moment
    # before it replays the log once, and one to the moment it began returns the database to how
    # this restore found it. A second check archives it. If the transaction or that check fails,
    # the bucket may lack what the replicas wrote since the first check, which refuses the
    # restore as the first check would have: Postgres still runs the database the restore found,
    # so the replicas and the backups start again first.
    undo=""
    if [ "$archived" = yes ]; then
        checked=""
        if began=$(compose exec -T --user postgres postgres psql -qAt -c 'SELECT now() WHERE pg_current_xact_id() IS NOT NULL' 2>&1) &&
            checked=$(compose exec -T --user postgres postgres pgbackrest --stanza=db check 2>&1); then
            undo=$began
            say "a restore to $undo undoes this one"
        else
            printf '%s\n' "${checked:-$began}"
            failed="the restore's transaction, or pgBackRest's check that archives it, failed, with the error above, once the replicas had stopped"
            if [ "$anyway" != yes ]; then
                say "$failed: $refusal"
                serve_again
                fail "refused the restore, and started ${live:+the API and }the backups again"
            fi
            say "$failed; restoring anyway, as --allow-unarchived asks, so what the bucket lacks of the database stays only in the data that moves aside"
        fi
    fi
    # The bucket records when the restore's timeline begins, for a later restore's plan to read
    # which timeline was live at a moment (crates/xtask/src/restore.rs): now, by the server's
    # clock, which Postgres stamps each commit with, to the microsecond, once nothing commits on
    # the database it replaces, and before Postgres stops. Postgres numbers the timeline one past
    # the newest that the bucket holds a history of, or than the one it replays along. The record
    # is the annotation `timeline-<id>` of the newest full backup: pgBackRest expires the oldest
    # full backups first, each with the backups that depend on it, so that one outlives every
    # backup that reaches a moment before the timeline began. A record that fails, as when the
    # bucket stops answering, refuses the restore, whatever its options: without it, a later
    # plan would put the moments before this restore on its timeline.
    # shellcheck disable=SC2016 # The script expands its own variables.
    if ! recorded=$(compose run --rm --no-deps --quiet-pull --user postgres --entrypoint sh postgres -c '
        set -e
        histories=$(pgbackrest --stanza=db repo-ls --recurse --filter="\.history\$" archive/db)
        newest=$(printf "%s\n" "$histories" | sed -n "s|^.*/\([0-9A-F]\{8\}\)\.history\$|\1|p" | sort | tail -n 1)
        id=$1
        if [ -n "$newest" ] && [ "$((0x$newest))" -gt "$id" ]; then id=$((0x$newest)); fi
        id=$((id + 1))
        info=$(pgbackrest --stanza=db info --output=json)
        full=$(printf "%s\n" "$info" | jq -r "[.[0].backup[] | select(.type == \"full\")] | max_by(.timestamp.stop) | .label")
        at=$(date -u "+%s%6N %Y-%m-%d %H:%M:%S.%6N+00")
        pgbackrest --stanza=db annotate --set="$full" --annotation="timeline-$id=${at%% *}" >&2
        printf "%s %s %s\n" "$id" "$full" "$at"' sh "$timeline"); then
        say "couldn't record in the bucket when the restore's timeline begins, with pgBackRest's error above"
        serve_again
        fail "refused the restore; once the bucket answers, restore again"
    fi
    # The timeline, the full backup that holds the record, and when it begins, in microseconds and
    # as Postgres writes a time.
    new_timeline=${recorded%% *}
    recorded=${recorded#* }
    record_set=${recorded%% *}
    recorded=${recorded#* }
    began_micros=${recorded%% *}
    say "the bucket records that timeline $new_timeline, which the restore begins, begins at ${recorded#* }"
}

# Stops Postgres, restores the data that the plan names into a fresh data directory, has Postgres
# replay the log, and serves the restored database.
restore_data() {
    compose stop postgres
    stamp=$(date -u +%Y%m%dT%H%M%SZ)
    say "restoring the database${moment:+ as it stood at $moment}; the data it replaces moves to /var/lib/postgresql/18/before-restore-$stamp"
    # As root, which owns the data directory's parent. Postgres starts only after a restore that
    # succeeded: on an empty data directory, it would create an empty database. A restore starts
    # from the backup, and replays along the timeline, that the plan names
    # (crates/xtask/src/restore.rs): pgBackRest would pick a backup by time alone, and Postgres
    # replay along the latest timeline, which together miss some moments, and the latest state
    # when the newest backup is on a timeline that a restore abandoned. The latest state's plan
    # names the latest timeline, which Postgres replays along without being told. With a time of
    # the bucket, pgBackRest reads the bucket as it stood then, and writes the time into
    # Postgres's restore_command, so the replay reads the log as the bucket held it too.
    # shellcheck disable=SC2016 # The script expands its arguments itself.
    compose run --rm --no-deps --quiet-pull --user root --entrypoint sh postgres -c '
        set -e
        mv /var/lib/postgresql/18/docker "/var/lib/postgresql/18/before-restore-$1"
        install -d -o postgres -g postgres -m 0700 /var/lib/postgresql/18/docker
        if [ -n "$5" ]; then set -- "$@" --repo=1 "--repo-target-time=$5"; fi
        moment=$2 set=$3 timeline=$4
        shift 5
        if [ -n "$moment" ]; then
            gosu postgres pgbackrest --stanza=db "$@" restore --set="$set" --type=time --target="$moment" --target-timeline="$timeline" --target-action=promote
        else
            gosu postgres pgbackrest --stanza=db "$@" restore --set="$set"
        fi' sh "$stamp" "$moment" "$set" "$timeline" "$held"
    if [ -n "$held" ]; then
        # The restored database's backups begin anew in the bucket: Postgres begins a timeline one
        # past the newest that the bucket held at that time, which a later restore may have
        # numbered the same in what the bucket holds now, and pgBackRest refuses a segment of the
        # log whose name the bucket holds with other bytes. So, before Postgres starts on the
        # restored data, pgBackRest deletes its stanza by name, which the bucket keeps as it kept
        # what it held at the time, and once the replay ends, creates it again.
        say "deleting by name what the bucket holds now, which it keeps for $(setting BUCKETS_KEEP_DELETED_DAYS 7) days more: the restored database's backups begin anew"
        # shellcheck disable=SC2016 # The script expands its own variable.
        compose run --rm --no-deps --quiet-pull --user postgres --entrypoint sh postgres -c '
            pgbackrest --stanza=db stop
            status=0
            pgbackrest --stanza=db stanza-delete || status=$?
            pgbackrest --stanza=db start
            exit "$status"'
    fi
    if ! recovered; then
        # Postgres ended the replay where the log that the bucket holds along the timeline ends,
        # without reaching the moment: no commit or rollback that the bucket holds follows the
        # moment there, as for one while an earlier restore ran, after the moment it named and
        # before its timeline began, or one after a lost server's last archived change. The
        # database stood at the moment as at the timeline's end, as far as the bucket holds it,
        # which a replay along the timeline to its end reaches, from the same backup. --delta
        # keeps each file that still matches the backup, and restores the others.
        say "no commit or rollback that the bucket holds follows $moment on timeline $timeline, so the restore replays the timeline to its end, which is how the database stood then"
        compose stop postgres
        if [ -n "$held" ]; then
            compose run --rm --no-deps --quiet-pull --user postgres --entrypoint pgbackrest postgres \
                --stanza=db --repo=1 "--repo-target-time=$held" restore --delta --set="$set" --type=default --target-timeline="$timeline"
        else
            compose run --rm --no-deps --quiet-pull --user postgres --entrypoint pgbackrest postgres \
                --stanza=db restore --delta --set="$set" --type=default --target-timeline="$timeline"
        fi
        if ! recovered; then
            compose logs --tail 30 postgres >&2 || true
            fail "Postgres couldn't replay timeline $timeline to its end; the end of its log, above, says why"
        fi
        # The time of the last commit that the replay found, or none when it found none after the
        # backup it started from.
        if ! last=$(compose exec -T --user postgres postgres psql -qAt -c 'SELECT pg_last_xact_replay_timestamp()'); then
            say "the restored database ends where timeline $timeline ends in the bucket"
        elif [ -n "$last" ]; then
            say "the restored database ends where timeline $timeline ends in the bucket, with its last commit at $last"
        else
            say "the restored database ends where timeline $timeline ends in the bucket, which holds no commit on it after the backup $set that the restore started from"
        fi
    fi
    if [ -n "$held" ]; then
        # The stanza that the restore deleted, anew, for the database that Postgres now runs,
        # whose log Postgres keeps until its archiving succeeds. A deploy creates it too.
        if ! compose exec -T --user postgres postgres pgbackrest --stanza=db stanza-create; then
            fail "restored the database from the backups as the bucket held them at $held, but pgBackRest couldn't begin its backups in the bucket, with the error above, so the API stays down; once the bucket answers, deploy the live release again, which begins them"
        fi
    else
        confirm_timeline
    fi
    if [ -n "$live" ]; then
        # The roles of a database restored from what the bucket held have the passwords of the
        # server that backed it up, which a new server's URLs don't hold once its first deploy
        # has served a database of its own: bootstrap sets them from the URLs, as a deploy does.
        if [ -n "$held" ]; then
            API_IMAGE_NEW=$live_image compose --profile tasks run --rm --no-deps --quiet-pull bootstrap
        fi
        API_IMAGE_NEW=$live_image compose --profile tasks run --rm --no-deps --quiet-pull migrate
        compose up --detach --wait --quiet-pull --no-deps --scale "api-$live=$replicas" "api-$live"
    fi
    # Online, as the scheduled backups are, while the replicas, if any started, serve, so a later
    # restore starts from the restored database's timeline. Postgres archives what they write
    # meanwhile.
    say "taking a full backup of the restored database${live:+, while the API serves}"
    if ! full_backup; then
        compose up --detach --wait --quiet-pull backup
        fail "restored the database${moment:+ as it stood at $moment}${undo:+, which a restore to $undo undoes}${live:+, and the API serves}, but the full backup after it failed, with the error above; take one with just backup, so a later restore starts from the restored database's timeline"
    fi
    compose up --detach --wait --quiet-pull backup
    say "restored the database${moment:+ as it stood at $moment}${undo:+; a restore to $undo undoes it}"
    if [ -n "$held" ]; then
        say "the bucket's backups begin with the full backup that the restore just took, which later restores reach back to; what the bucket held before stays readable, with --bucket-as-of and a time before this restore, for the $(setting BUCKETS_KEEP_DELETED_DAYS 7) days that it keeps what is deleted in it"
    fi
}

# Checks that the timeline that Postgres began is the one whose beginning the bucket records.
# It is, unless the bucket's histories changed while the restore ran, as when another server
# restores into it: then the restore records the same beginning for the timeline Postgres began,
# so that no timeline it begins lacks a record.
confirm_timeline() {
    if ! postgres_timeline=$(compose exec -T --user postgres postgres psql -qAt -c "SELECT ('x' || left(pg_walfile_name(pg_current_wal_lsn()), 8))::bit(32)::int"); then
        fail "couldn't find which timeline Postgres began, with the error above"
    fi
    if [ "$postgres_timeline" != "$new_timeline" ]; then
        say "Postgres began timeline $postgres_timeline, not $new_timeline: the bucket's histories changed while the restore ran, as when another server restores into it; recording the beginning for timeline $postgres_timeline"
        if ! compose exec -T --user postgres postgres pgbackrest --stanza=db annotate --set="$record_set" --annotation="timeline-$postgres_timeline=$began_micros" >&2; then
            fail "couldn't record in the bucket when timeline $postgres_timeline begins, with pgBackRest's error above, so the API stays down; once the bucket answers, restore the latest state, which records it"
        fi
    fi
}

# Prints what the backup bucket records of the bundled database's timelines, for a restore to
# plan from, one item per line: `backups` with pgBackRest's info in JSON, whose annotations
# record when each timeline began, and `branch` for each line of each history file that the
# bucket holds, with the file's name, the timeline it descends from and the position where it
# branched off; and first, `now` with the server's clock, in microseconds since the Unix epoch,
# the clock that Postgres stamps each commit with. It reads the bucket from Postgres's image,
# whether Postgres runs or not, as on a new server, and the clock there too, with GNU's date.
# With a time as $1, as pgBackRest takes one, it prints what the bucket recorded at that time,
# from the versions that the bucket keeps (OPS-22).
restore_inputs() {
    read_settings
    if [ "$database" != bundled ]; then fail "the database is managed: restore it through its provider"; fi
    if [ -n "${1:-}" ]; then keeps_deleted; fi
    # shellcheck disable=SC2016 # The script expands its own variables.
    compose run --rm --no-deps --quiet-pull --user postgres --entrypoint sh postgres -c '
        set -e
        if [ -n "$1" ]; then set -- --repo=1 "--repo-target-time=$1"; else shift; fi
        printf "now %s\n" "$(date +%s%6N)"
        printf "backups %s\n" "$(pgbackrest --stanza=db "$@" info --output=json | tr -d "\n")"
        for name in $(pgbackrest --stanza=db "$@" repo-ls --recurse --filter="\.history\$" archive/db); do
            pgbackrest --stanza=db "$@" repo-get "archive/db/$name" |
                awk -v name="$name" "\$1 ~ /^[0-9]+\$/ { print \"branch\", name, \$1, \$2 }"
        done' sh "${1:-}"
}
