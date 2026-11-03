# The bundled database's backups and restores (OPS-17): a backup on demand, a restore to a
# moment or to the latest state, and what a restore plans from.

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

restore() {
    moment=$1
    set=$2
    timeline=$3
    anyway=$4
    if [ -z "$set" ] || [ -z "$timeline" ]; then
        fail "a restore takes the backup and the timeline that just restore plans from restore-inputs"
    fi
    lock
    read_settings
    if [ "$database" != bundled ]; then fail "the database is managed: restore it through its provider"; fi
    live=$(live_color)
    # The live replicas' image, which deploy.env keeps for their color, while its image for
    # `migrate` is that of the last deploy, which may have failed.
    case $live in
        blue) live_image=$(sed -n 's/^API_IMAGE_BLUE=//p' "$state/deploy.env") ;;
        green) live_image=$(sed -n 's/^API_IMAGE_GREEN=//p' "$state/deploy.env") ;;
    esac
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
    if compose exec -T --user postgres postgres pg_isready --quiet > /dev/null 2>&1 &&
        [ "$(compose exec -T --user postgres postgres psql -qAt -c 'SELECT pg_is_in_recovery()' 2> /dev/null)" != t ]; then
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
    say "stopping the API and the backups"
    compose stop api-blue api-green backup
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
                say "starting the API and the backups again, with the database as the restore found it"
                if [ -n "$live" ]; then
                    compose up --detach --wait --quiet-pull --no-deps --scale "api-$live=$replicas" "api-$live"
                fi
                compose up --detach --wait --quiet-pull backup
                fail "refused the restore, and started the API and the backups again"
            fi
            say "$failed; restoring anyway, as --allow-unarchived asks, so what the bucket lacks of the database stays only in the data that moves aside"
        fi
    fi
    compose stop postgres
    stamp=$(date -u +%Y%m%dT%H%M%SZ)
    say "restoring the database${moment:+ as it stood at $moment}; the data it replaces moves to /var/lib/postgresql/18/before-restore-$stamp"
    # As root, which owns the data directory's parent. Postgres starts only after a restore that
    # succeeded: on an empty data directory, it would create an empty database. A restore starts
    # from the backup, and replays along the timeline, that the plan names
    # (crates/xtask/src/restore.rs): pgBackRest would pick a backup by time alone, and Postgres
    # replay along the latest timeline, which together miss some moments, and the latest state
    # when the newest backup is on a timeline that a restore abandoned. The latest state's plan
    # names the latest timeline, which Postgres replays along without being told.
    # shellcheck disable=SC2016 # The script expands its arguments itself.
    compose run --rm --no-deps --quiet-pull --user root --entrypoint sh postgres -c '
        set -e
        mv /var/lib/postgresql/18/docker "/var/lib/postgresql/18/before-restore-$1"
        install -d -o postgres -g postgres -m 0700 /var/lib/postgresql/18/docker
        if [ -n "$2" ]; then
            gosu postgres pgbackrest --stanza=db restore --set="$3" --type=time --target="$2" --target-timeline="$4" --target-action=promote
        else
            gosu postgres pgbackrest --stanza=db restore --set="$3"
        fi' sh "$stamp" "$moment" "$set" "$timeline"
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
        compose run --rm --no-deps --quiet-pull --user postgres --entrypoint pgbackrest postgres \
            --stanza=db restore --delta --set="$set" --type=default --target-timeline="$timeline"
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
    if [ -n "$live" ]; then
        API_IMAGE_NEW=$live_image compose --profile tasks run --rm --no-deps --quiet-pull migrate
        compose up --detach --wait --quiet-pull --no-deps --scale "api-$live=$replicas" "api-$live"
    fi
    # Online, as the scheduled backups are, while the replicas serve, so a later restore starts
    # from the restored database's timeline. Postgres archives what they write meanwhile.
    say "taking a full backup of the restored database, while the API serves"
    if ! full_backup; then
        compose up --detach --wait --quiet-pull backup
        fail "restored the database${moment:+ as it stood at $moment}${undo:+, which a restore to $undo undoes}, and the API serves, but the full backup after it failed, with the error above; take one with just backup, so a later restore starts from the restored database's timeline"
    fi
    compose up --detach --wait --quiet-pull backup
    say "restored the database${moment:+ as it stood at $moment}${undo:+; a restore to $undo undoes it}"
}

# Prints what the backup bucket records of the bundled database's timelines, for a restore to
# plan from, one item per line: `backups` with pgBackRest's info in JSON, `histories`
# with the timelines' history files that the bucket lists in JSON, each with the time it reached
# the bucket, which is when its timeline began, and `branch` for each line of each history file,
# with the file's name, the timeline it descends from and the position where it branched off;
# and first, `now` with the server's clock, in seconds since the Unix epoch, the clock that
# Postgres stamps each commit with. It reads the bucket from Postgres's image, whether Postgres
# runs or not, as on a new server.
restore_inputs() {
    read_settings
    if [ "$database" != bundled ]; then fail "the database is managed: restore it through its provider"; fi
    printf 'now %s\n' "$(date +%s)"
    # shellcheck disable=SC2016 # The script expands its own variables.
    compose run --rm --no-deps --quiet-pull --user postgres --entrypoint sh postgres -c '
        set -e
        printf "backups %s\n" "$(pgbackrest --stanza=db info --output=json | tr -d "\n")"
        printf "histories %s\n" "$(pgbackrest --stanza=db repo-ls --recurse --filter="\.history\$" --output=json archive/db | tr -d "\n")"
        for name in $(pgbackrest --stanza=db repo-ls --recurse --filter="\.history\$" archive/db); do
            pgbackrest --stanza=db repo-get "archive/db/$name" |
                awk -v name="$name" "\$1 ~ /^[0-9]+\$/ { print \"branch\", name, \$1, \$2 }"
        done'
}
