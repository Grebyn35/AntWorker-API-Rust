#!/bin/sh
# The move of the files' objects to the bucket that a change of FILES chooses (D104), which the
# service files-move runs with rclone, in a POSIX shell. It copies every object of the bucket that
# FILES left that the one it chooses lacks, comparing them by size, since an object never changes,
# and leaves the bundled bucket's queue and its mark aside. A deploy that changes FILES runs it
# before its replicas start, and again once the replicas that used the bucket it leaves have
# stopped, for the objects they stored meanwhile. It deletes nothing, so the bucket it leaves keeps
# every object until the operator removes it (docs/operations/deploy.md#changing-files).
#
# FILES_MOVE_FROM names the bucket it leaves, bundled or managed, and FILES_MOVE_ENDPOINT,
# FILES_MOVE_REGION, FILES_MOVE_BUCKET, FILES_MOVE_URI_STYLE and FILES_MOVE_CA_FILE the provider's
# bucket, whichever side it is on.
set -eu

# shellcheck disable=SC2034 # here names the directory of the file it sources, which check-shell inlines.
here=$(cd "$(dirname "$0")" && pwd)
. "$here/remotes.sh"

bundled_remote
provider_remote "$FILES_MOVE_ENDPOINT" "$FILES_MOVE_REGION" "$FILES_MOVE_URI_STYLE" provider_files_s3_key provider_files_s3_key_secret
rclone_environment
authorities=$(authorities "$FILES_MOVE_CA_FILE")

case $FILES_MOVE_FROM in
    bundled) from=bundled:files to="provider:$FILES_MOVE_BUCKET" ;;
    managed) from="provider:$FILES_MOVE_BUCKET" to=bundled:files ;;
    *)
        echo "files-move: FILES_MOVE_FROM is bundled or managed" >&2
        exit 1
        ;;
esac

# shellcheck disable=SC2086 # $authorities holds one option and its value per pair of words.
rclone $authorities copy "$from" "$to" --size-only --exclude "/copy-queue/**" --exclude /copy-restoring \
    --transfers 8 --checkers 16 --stats 60s --stats-log-level NOTICE --stats-one-line
