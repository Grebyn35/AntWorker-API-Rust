#!/bin/sh
# The reference deployment's tasks on its server (D45): deploys a release with no downtime
# (OPS-14), and with a managed backend, imports its dashboards and alert rules into the backend's
# Grafana through import-grafana.sh (D42), takes and restores the bundled database's backups
# (OPS-15), shows the deployment's state and its services' logs, checks what a deploy needs, and
# writes a new key in place of one that a deploy generated (OPS-19), removes a lost second factor
# from an account (AUTH-072), and ends an account's sessions, or every account's (AUTH-094).
# `just deploy`, `just backup`, `just restore`, `just status`, `just logs`, `just server-check`,
# `just replace-key`, `just remove-second-factor` and `just end-sessions` run it over SSH from a
# developer's machine; docs/operations/deploy.md describes the deployment. Each task's code is a
# file of tasks/, which this script sources.
# It also sets up a workspace (WORKSPACES-023), which `just setup-workspace` runs. # part: workspace_setup
# It also makes an account a superadmin, ends one, or lists them (SUPERADMINS-001), which `just superadmin` runs. # part: superadmins
# It also writes back the files' bytes that were deleted or replaced in their bucket since a moment (FILES-028), which `just restore-files` runs. # part: files
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
#   deploy.sh restore <moment> <backup> <timeline> --bucket-as-of <time>
#       Restores the bundled database from the backups as their bucket held them at <time>, a
#       time in UTC, to the second, as pgBackRest takes one and `just restore` writes it,
#       whatever the bucket holds now (OPS-22). It checks nothing of the database it replaces,
#       and records nothing in the bucket before it stops Postgres. Once it has restored the
#       data, it deletes pgBackRest's stanza in the bucket by name, and creates it again once
#       Postgres has replayed the log, so the full backup that ends it is the bucket's first;
#       it runs bootstrap before migrate. It refuses while BUCKETS_KEEP_DELETED_DAYS is 0.
#   deploy.sh restore-inputs [<time>]
#       Prints what a restore plans from: the server's clock, and what the backup bucket records
#       of the bundled database's timelines, or, with <time>, recorded then: pgBackRest's info,
#       whose annotations record when each timeline began, and each history file's branches. It
#       changes nothing.
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
#   deploy.sh replace-key <hmac_key|grafana_secret_key>
#       Writes a new key in place of one that a deploy generated: the key of the API's keyed
#       hashes, or the one under which Grafana encrypts what it stores. The services that run
#       keep the key they started with, until the next deploy. Fails, changing nothing, while
#       the deployment holds no such key yet.
#   deploy.sh remove-second-factor <email>
#       Removes the authenticator app and the recovery codes of the account that holds <email>,
#       ends every session of the account, and tells the address, through the API's subcommand of
#       the same name, with the image of the color that serves. Fails, changing nothing, when no
#       account holds the address or it has no app.
#   deploy.sh end-sessions <email>
#   deploy.sh end-sessions --all
#       Ends every session of the account that holds <email>, or, with --all, of every account,
#       through the API's subcommand of the same name, with the image of the color that serves.
#       Fails, changing nothing, when no account holds the address, or no session is there to end.
# part: files
#   deploy.sh restore-files <moment>
#       Writes again each object of the files bucket whose version at <moment> isn't its current
#       one, as one deleted or replaced since, from the version that the bucket kept, through the
#       API's subcommand of the same name, with the image of the color that serves, and deletes
#       nothing. <moment> is a time in UTC as a request gives one (API-11), such as
#       2026-10-25T10:30:00.000000Z, which is how `just restore-files` writes the time it is given.
# end part: files
# part: workspace_setup
#   deploy.sh setup-workspace <name> <email> [<owner's name>]
#       Creates the workspace <name>, owned by the account that holds <email>, through the API's
#       subcommand of the same name, with the image of the color that serves. With email on, it
#       creates that account, named <owner's name>, when no account holds the address, and tells
#       the address. Fails, changing nothing, when an earlier run was given the name, or the
#       account may not own the workspace.
# end part: workspace_setup
# part: superadmins
#   deploy.sh superadmin <add|remove> <email>
#   deploy.sh superadmin list
#       Makes the account that holds <email> a superadmin of the deployment, or ends its being
#       one, or shows the deployment's superadmins, each with the address of their account,
#       through the API's subcommand of the same name, with the image of the color that serves.
#       A change fails, changing nothing, when no account holds the address, when, with email
#       on, the account hasn't proved it, or when the account is one already, or is none to end.
# end part: superadmins
#   deploy.sh check <version>
#   deploy.sh check-images <API image> <Postgres image>
#       Checks what a deploy of the release <version>, or of the two images, each named by
#       digest, needs, and changes nothing of the deployment but an object that it writes to
#       each bucket and deletes (OPS-22), though it pulls the tools' images, the API's image,
#       and the Postgres image whatever the database, which a deploy pulls only with the bundled
#       one. Prints a line for each check that it runs, and fails if any fails.
#
# A deploy restarts a service that runs once, such as Postgres or Caddy, only with
# --allow-restart, because restarting one interrupts requests.
# The scanner runs once too, and its restart interrupts only uploads. # part: files
# DEPLOYMENT names the deployment's directory, which holds settings.env, secrets/ and state/. The script first installs the files of
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
. "$here/tasks/secrets.sh"
. "$here/tasks/apply.sh"
. "$here/tasks/backups.sh"
. "$here/tasks/status.sh"
. "$here/tasks/check.sh"
. "$here/tasks/buckets.sh"
. "$here/tasks/accounts.sh"

# Prints yes when $2, a command's last argument, is the option $1, and no when it's empty.
allowed() {
    case ${2:-} in
        '') echo no ;;
        "$1") echo yes ;;
        *) fail "unknown option $2" ;;
    esac
}

# The tasks, for the usage that an unknown one gets.
tasks="deploy|apply|backup|restore|restore-inputs|status|logs|replace-key|remove-second-factor|end-sessions|check|check-images"
# part: files
tasks="$tasks|restore-files"
# end part: files
# part: workspace_setup
tasks="$tasks|setup-workspace"
# end part: workspace_setup
# part: superadmins
tasks="$tasks|superadmin"
# end part: superadmins

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
        case ${5:-} in
            --bucket-as-of)
                if [ "$#" -ne 6 ] || [ -z "$6" ]; then fail "usage: deploy.sh restore <moment> <backup> <timeline> --bucket-as-of <time>"; fi
                restore "$2" "$3" "$4" no "$6"
                ;;
            *)
                unarchived=$(allowed --allow-unarchived "${5:-}")
                restore "${2:-}" "${3:-}" "${4:-}" "$unarchived" ""
                ;;
        esac
        ;;
    restore-inputs) restore_inputs "${2:-}" ;;
    status) status ;;
    logs)
        shift
        logs "$@"
        ;;
    replace-key)
        if [ "$#" -ne 2 ]; then fail "usage: deploy.sh replace-key <hmac_key|grafana_secret_key>"; fi
        replace_key "$2"
        ;;
    remove-second-factor)
        if [ "$#" -ne 2 ]; then fail "usage: deploy.sh remove-second-factor <email>"; fi
        remove_second_factor "$2"
        ;;
    end-sessions)
        if [ "$#" -ne 2 ]; then fail "usage: deploy.sh end-sessions <email>, or deploy.sh end-sessions --all"; fi
        end_sessions "$2"
        ;;
    # part: files
    restore-files)
        if [ "$#" -ne 2 ]; then fail "usage: deploy.sh restore-files <moment>"; fi
        restore_files "$2"
        ;;
    # end part: files
    # part: workspace_setup
    setup-workspace)
        if [ "$#" -lt 3 ] || [ "$#" -gt 4 ]; then fail "usage: deploy.sh setup-workspace <name> <email> [<owner's name>]"; fi
        shift
        setup_workspace "$@"
        ;;
    # end part: workspace_setup
    # part: superadmins
    superadmin)
        case "${2:-}:$#" in
            add:3 | remove:3 | list:2) ;;
            *) fail "usage: deploy.sh superadmin <add|remove> <email>, or deploy.sh superadmin list" ;;
        esac
        shift
        superadmin "$@"
        ;;
    # end part: superadmins
    check)
        if [ "$#" -ne 2 ]; then fail "usage: deploy.sh check <version>"; fi
        check "$2"
        ;;
    check-images)
        if [ "$#" -ne 3 ]; then fail "usage: deploy.sh check-images <API image> <Postgres image>"; fi
        check "$2" "$3"
        ;;
    *) fail "usage: deploy.sh $tasks (see the comment at the top of deploy.sh)" ;;
esac
