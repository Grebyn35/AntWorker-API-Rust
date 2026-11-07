#!/bin/sh
# The copy of the bundled files bucket, off the server (OPS-15, D101), which the service
# files-copy runs with rclone, in a POSIX shell. The API writes each object it stores a second
# time, under copy-queue/, and this moves what the queue holds to the copy's bucket every few
# seconds, through rclone's crypt, which encrypts each object's bytes before they leave the
# server and keeps its key, so each object leaves the queue once the copy holds it. Once a day,
# and as it starts, it also copies every object of the bucket that the copy lacks, which a
# deployment that held files before it had a copy needs. It never deletes from the copy: the API
# deletes an object from the copy when it deletes it from the bucket, once no restore can need it
# (FILES-011), so a bucket that starts empty, as a new server's does, empties nothing.
#
# While the bucket holds the mark copy-restoring, which restore-files writes, it brings back every
# object of the copy that the bucket lacks, beside the moves, then removes the mark (FILES-015).
#
# How each pass went is the time of a file in /var/lib/files-copy-status, which the collector
# reads, and FilesCopyFailing and FilesRestoreFailing alert on: files-copied and files-copy-failed
# for the copy, files-restored and files-restore-failed for bringing objects back, and
# files-restoring while that runs (docs/operations/runbooks.md).
set -eu

# shellcheck disable=SC2034 # here names the directory of the file it sources, which check-shell inlines.
here=$(cd "$(dirname "$0")" && pwd)
. "$here/remotes.sh"

status=/var/lib/files-copy-status
mark=copy-restoring

# The bundled bucket, and the copy's bucket at its provider, reached with the bundled bucket's key
# pair and the copy's.
bundled_remote
provider_remote "$FILES_COPY_ENDPOINT" "$FILES_COPY_REGION" "$FILES_COPY_URI_STYLE" files_copy_s3_key files_copy_s3_key_secret

# The copy: the provider's bucket through crypt, which encrypts each object's bytes with a key
# from the passphrase, and keeps the object's key, so that the API reaches it by the same key.
export RCLONE_CONFIG_COPY_TYPE=crypt
export RCLONE_CONFIG_COPY_REMOTE="provider:$FILES_COPY_BUCKET"
export RCLONE_CONFIG_COPY_FILENAME_ENCRYPTION=off
export RCLONE_CONFIG_COPY_DIRECTORY_NAME_ENCRYPTION=false
export RCLONE_CONFIG_COPY_SUFFIX=none
RCLONE_CONFIG_COPY_PASSWORD=$(rclone obscure - < /run/secrets/files_copy_cipher_pass)
export RCLONE_CONFIG_COPY_PASSWORD

rclone_environment
authorities=$(authorities "$FILES_COPY_CA_FILE")

# rclone, comparing objects by their sizes. Each pass that moves the queue logs only its errors;
# the longer copies log their progress every minute.
run() {
    # shellcheck disable=SC2086 # $authorities holds one option and its value per pair of words.
    rclone $authorities --size-only "$@"
}
progress="--stats 60s --stats-log-level NOTICE --stats-one-line"

# Moves what the queue holds to the copy, for at most 4 minutes, so a long backlog still records
# its progress; rclone's 10 says that the time ran out, with the rest still queued.
move_queue() {
    code=0
    run move bundled:files/copy-queue copy: --no-traverse --transfers 8 --max-duration 4m --quiet || code=$?
    case $code in
        0 | 10) touch "$status/files-copied" ;;
        *)
            touch "$status/files-copy-failed"
            echo "files-copy: moving the queue to the copy failed, with rclone's error above; it tries again"
            ;;
    esac
}

# Copies every object of the bucket that the copy lacks, the queue and the mark aside.
copy_all() {
    # shellcheck disable=SC2086 # $progress holds one option per word.
    if run copy bundled:files copy: --exclude "/copy-queue/**" --exclude "/$mark" --transfers 8 --checkers 16 $progress; then
        touch "$status/files-copied"
        return 0
    fi
    touch "$status/files-copy-failed"
    echo "files-copy: copying the bucket's objects that the copy lacks failed, with rclone's error above; it tries again"
    return 1
}

# Brings back every object of the copy that the bucket lacks, then removes the mark, in the
# background, so the queue keeps moving meanwhile.
restore_all() {
    touch "$status/files-restoring"
    echo "files-copy: the bucket is restoring from its copy; bringing back each object it lacks"
    # shellcheck disable=SC2086 # $progress holds one option per word.
    if run copy copy: bundled:files --transfers 8 --checkers 16 $progress && run deletefile "bundled:files/$mark"; then
        touch "$status/files-restored"
        rm -f "$status/files-restoring"
        echo "files-copy: the bucket holds every object of its copy again"
    else
        touch "$status/files-restore-failed"
        echo "files-copy: bringing back the copy's objects failed, with rclone's error above; it tries again in a minute"
    fi
}

restoring=""
restore_failed_at=0
copied_all_at=0
trap 'if [ -n "$restoring" ]; then kill "$restoring" 2> /dev/null || true; fi; exit 0' TERM INT
while true; do
    now=$(date +%s)
    if [ -n "$restoring" ] && ! kill -0 "$restoring" 2> /dev/null; then
        wait "$restoring" || true
        restoring=""
        if [ -f "$status/files-restoring" ]; then restore_failed_at=$now; fi
    fi
    if [ -z "$restoring" ] && [ $((now - restore_failed_at)) -ge 60 ] &&
        run lsf --files-only --max-depth 1 bundled:files 2> /dev/null | grep -qx "$mark"; then
        restore_all &
        restoring=$!
    fi
    move_queue
    # A copy of everything that failed tries again in 10 minutes.
    if [ $((now - copied_all_at)) -ge 86400 ]; then
        if copy_all; then copied_all_at=$now; else copied_all_at=$((now - 86400 + 600)); fi
    fi
    sleep 5 &
    wait $! || true
done
