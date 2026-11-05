#!/bin/sh
# The reference deployment's tasks on its server (D45): deploys a release with no downtime
# (OPS-14), and with a managed backend, imports its dashboards and alert rules into the backend's
# Grafana through import-grafana.sh (D42), takes and restores the bundled database's backups
# (OPS-15), and shows the deployment's state and its services' logs (OPS-19). `just deploy`,
# `just backup`, `just restore`, `just status` and `just logs` run it over SSH from a developer's
# machine; docs/operations/deploy.md describes the deployment. Each task's code is a file of
# tasks/, which this script sources.
#
#   deploy.sh deploy <version> [--allow-restart]
#       Verifies the release <version>, then deploys its images. Once its replicas serve, it
#       removes the other releases' files and the images that nothing uses.
#   deploy.sh apply <API image> <Postgres image> [--allow-restart]
#       Deploys the two images, each named by digest, such as ghcr.io/owner/name@sha256:..., as
#       deploy does once the release verifies.
#   deploy.sh backup
#       Takes a full base backup of the bundled database.
#   deploy.sh restore <moment> <backup> <timeline> [--allow-unarchived]
#       Restores the bundled database, as it stood at <moment>, a time with its offset from UTC
#       as Postgres writes it, or, with <moment> empty, at the latest moment its backups hold,
#       from the backup <backup>, along the timeline <timeline>, which `just restore` plans from
#       restore-inputs. It restores into a fresh data directory, and keeps the one it replaces
#       beside it. When it finds Postgres running the database the bucket backs up, with its log
#       reaching the bucket, it prints the moment that a restore to undoes it; when that log
#       doesn't reach the bucket, it refuses before anything stops, or, when its transaction or
#       the check that archives it fails once the replicas have stopped, starts them again and
#       refuses, unless --allow-unarchived lets it go on. Postgres still replaying the log, as
#       after a restore that failed during its replay, isn't checked. Before it stops Postgres, it
#       records in the bucket when the timeline it begins begins, and refuses if it can't.
#   deploy.sh restore-inputs
#       Prints what a restore plans from: the server's clock, and what the backup bucket records
#       of the bundled database's timelines: pgBackRest's info, whose annotations record when
#       each timeline began, and each history file's branches. It changes nothing.
#   deploy.sh status
#       Shows whether a deploy, backup or restore runs, or a deploy stopped before its replicas
#       served, which color serves, each of the deployment's containers with its state and its
#       image's version, when the bundled database's backups last succeeded, the data that each
#       restore moved aside, and the server's free disk and memory. It changes nothing, so it
#       runs beside any other task.
#   deploy.sh logs [--follow] [<service>...]
#       Shows the last 100 lines of each service's log, of every service without one, and with
#       --follow, the lines that follow. `api` names both colors' replicas. With caddy, or every
#       service, Caddy's lines of the requests it answered itself follow too (D53).
#   deploy.sh check <version>
#   deploy.sh check-images <API image> <Postgres image>
#       Checks what a deploy of the release <version>, or of the two images, each named by
#       digest, needs, and changes nothing of the deployment, though it pulls the tools' images
#       and the Postgres image, as a deploy does. Prints a line for each check, and fails if any
#       fails.
#
# A deploy restarts Postgres or Caddy, which each run once, only with --allow-restart, because
# restarting either interrupts requests for a few seconds. DEPLOYMENT names the deployment's
# directory, which holds settings.env, secrets/ and state/. The script first installs the files of
# its own release there, so the deployment runs the release's configuration.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
if [ ! -d "${DEPLOYMENT:?DEPLOYMENT names the deployment directory}" ]; then
    printf '%s\n' "deploy: $DEPLOYMENT doesn't exist; just server-setup writes it (docs/operations/deploy.md)" >&2
    exit 1
fi
deployment=$(cd "$DEPLOYMENT" && pwd)
state=$deployment/state
project=deployment

# Each task's code, a file of tasks/ that shares this script's variables (CODE-08).
. "$here/tasks/common.sh"
. "$here/tasks/apply.sh"
. "$here/tasks/backups.sh"
. "$here/tasks/status.sh"
. "$here/tasks/check.sh"

# Prints yes when $2, a command's last argument, is the option $1, and no when it's empty.
allowed() {
    case ${2:-} in
        '') echo no ;;
        "$1") echo yes ;;
        *) fail "unknown option $2" ;;
    esac
}

command=${1:-}
case $command in
    deploy)
        if [ "$#" -lt 2 ]; then fail "usage: deploy.sh deploy <version> [--allow-restart]"; fi
        # An assignment, so that the script stops when allowed refuses.
        restart=$(allowed --allow-restart "${3:-}")
        deploy_release "$2" "$restart"
        ;;
    apply)
        if [ "$#" -lt 3 ]; then fail "usage: deploy.sh apply <API image> <Postgres image> [--allow-restart]"; fi
        restart=$(allowed --allow-restart "${4:-}")
        apply "$2" "$3" "$restart"
        ;;
    backup) backup ;;
    restore)
        unarchived=$(allowed --allow-unarchived "${5:-}")
        restore "${2:-}" "${3:-}" "${4:-}" "$unarchived"
        ;;
    restore-inputs) restore_inputs ;;
    status) status ;;
    logs)
        shift
        logs "$@"
        ;;
    check)
        if [ "$#" -ne 2 ]; then fail "usage: deploy.sh check <version>"; fi
        check "$2"
        ;;
    check-images)
        if [ "$#" -ne 3 ]; then fail "usage: deploy.sh check-images <API image> <Postgres image>"; fi
        check "$2" "$3"
        ;;
    *) fail "usage: deploy.sh deploy|apply|backup|restore|restore-inputs|status|logs|check|check-images (see the comment at the top of deploy.sh)" ;;
esac
