#!/bin/sh
# The reference deployment's tasks on its server (D45): deploys a release with no downtime
# (OPS-16), and with a managed backend, imports its dashboards and alert rules into the backend's
# Grafana through import-grafana.sh (D42), takes and restores the bundled database's backups
# (OPS-17), and shows the deployment's state and its services' logs (OPS-22). `just deploy`,
# `just backup`, `just restore`, `just status` and `just logs` run it over SSH from a developer's
# machine; docs/operations/deploy.md describes the deployment.
#
#   deploy.sh deploy <version> [--allow-restart]
#       Verifies the release <version>, then deploys its images. Once its replicas serve, it
#       removes the other releases' files and the images that nothing uses.
#   deploy.sh apply <API image> <Postgres image> [--allow-restart]
#       Deploys the two images, each named by digest, such as ghcr.io/owner/name@sha256:..., as
#       deploy does once the release verifies.
#   deploy.sh backup
#       Takes a full base backup of the bundled database.
#   deploy.sh restore [<moment>]
#       Restores the bundled database, as it stood at <moment>, a time with its offset from UTC
#       as Postgres writes it, or at the latest moment its backups hold, into a fresh data
#       directory, and keeps the one it replaces beside it.
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

say() { printf '%s\n' "deploy: $*"; }
fail() {
    printf '%s\n' "deploy: $*" >&2
    exit 1
}

# The value of the setting $1 in settings.env, or $2 when it is unset or empty.
setting() {
    value=$(sed -n "s/^$1=//p" "$deployment/settings.env" | tail -n 1)
    printf '%s' "${value:-${2:-}}"
}

# Fails unless $2, the value of the setting $1, is a whole number from $3 to $4.
whole_number() {
    case $2 in
        '' | *[!0-9]*) fail "$1 is a whole number from $3 to $4, not '$2'" ;;
    esac
    if [ "$2" -lt "$3" ] || [ "$2" -gt "$4" ]; then fail "$1 is a whole number from $3 to $4, not $2"; fi
}

compose() {
    # shellcheck disable=SC2086 # $profiles holds one --profile option per word.
    docker compose --project-name "$project" --project-directory "$deployment" \
        --file "$deployment/compose.yaml" --env-file "$deployment/settings.env" \
        --env-file "$state/deploy.env" $profiles "$@"
}

# The ID of the running container of the deployment's service $1, if one runs.
running() {
    docker ps --quiet --filter "label=com.docker.compose.project=$project" \
        --filter "label=com.docker.compose.service=$1" --filter status=running | head -n 1
}

# One deploy, backup or restore at a time (OPS-16). A task takes the lock state/lock, and refuses
# while another task holds it, then holds state/running, which `status` looks at: status takes that
# one for an instant, which delays a task rather than refusing it. The kernel releases both once the
# last of the task's processes ends, however it ends, so no lock outlives its task.
lock() {
    mkdir -p "$state"
    exec 9>> "$state/lock" 8>> "$state/running"
    if ! flock -n 9; then fail "another deploy, backup or restore is running"; fi
    flock 8
    if [ -d "$state/previous" ]; then
        say "an earlier deploy stopped before its replicas served, and didn't put back the deployment files it replaced; putting them back"
        put_back
    fi
    trap finish EXIT
    trap 'exit 1' HUP INT TERM
}

# Runs as the script exits, once it holds the lock: a deploy that stops before its replicas serve
# puts back the deployment files it replaced, which it kept in state/previous/.
finish() {
    status=$?
    if [ "$status" -ne 0 ] && [ -d "$state/previous" ]; then
        say "putting back the deployment files that this deploy replaced"
        put_back
    fi
}

# Puts back the deployment files in state/previous/, which a deploy replaced and whose replicas
# never served. If that fails, they stay there, for the next task to put back (D50).
put_back() {
    # In a subshell, which stops at the first command that fails, as the script does. Under a
    # condition, such as `if`, it wouldn't: the shell ignores -e there.
    set +e
    (
        set -e
        copy_release "$state/previous" "$deployment"
    )
    copied=$?
    set -e
    if [ "$copied" -ne 0 ]; then
        fail "couldn't put back the deployment files, so $deployment holds some of them and some of the release that replaced them. They stay in $state/previous, and the next deploy, backup or restore puts them back, once the error above is solved"
    fi
    rm -rf "$state/previous"
}

# Makes the directory $2 hold exactly the files of the directory $1, writing each file in place,
# so a container that mounts the directory or the file sees the new contents. Without $1, $2 goes.
sync_directory() {
    if [ ! -d "$1" ]; then
        rm -rf "$2"
        return
    fi
    mkdir -p "$2"
    (cd "$1" && find . -type d) | while IFS= read -r directory; do mkdir -p "$2/$directory"; done
    (cd "$1" && find . -type f) | while IFS= read -r file; do cp "$1/$file" "$2/$file"; done
    (cd "$2" && find . -type f) | while IFS= read -r file; do
        if [ ! -e "$1/$file" ]; then rm "$2/$file"; fi
    done
}

# A fingerprint of the files under the directory $1: the same files give the same one.
fingerprint() {
    if [ -d "$1" ]; then (cd "$1" && find . -type f -exec cksum {} + | sort | cksum); fi
}

# Makes the deployment files in the directory $2 those of the directory $1. A file or a directory
# that $1 lacks goes from $2, such as one that a later release adds and a deploy of it that fails
# takes away again (D50).
copy_release() {
    # Each file is renamed into place, so a deploy.sh that runs, whose shell reads it as it goes,
    # goes on reading the one it started with.
    for file in compose.yaml deploy.sh verify-release.sh import-grafana.sh settings.env.example; do
        if [ -f "$1/$file" ]; then
            cp "$1/$file" "$2/$file.next"
            mv "$2/$file.next" "$2/$file"
        else
            rm -f "$2/$file"
        fi
    done
    for directory in caddy observability tools; do
        sync_directory "$1/$directory" "$2/$directory"
    done
}

# Installs this release's files into the deployment's directory, and keeps the ones they replace
# in state/previous/ until the release's replicas serve. Those are copied into
# state/previous.next/, which takes the name once it holds them all, since putting back only some
# of them would remove the others.
install_release() {
    if [ "$here" = "$deployment" ]; then return; fi
    if [ -f "$deployment/compose.yaml" ]; then
        rm -rf "$state/previous.next"
        mkdir -p "$state/previous.next"
        copy_release "$deployment" "$state/previous.next"
        mv "$state/previous.next" "$state/previous"
    fi
    copy_release "$here" "$deployment"
}

# The services of the observability stack that read their configuration files only when they
# start.
stack_services="collector collector-managed prometheus alertmanager loki tempo grafana"

# The directory of the files that the service $1 of the stack reads when it starts. The managed
# backend's collector reads the collector's. Grafana reads its dashboards every 30 seconds on its
# own, and its provisioning only when it starts.
configuration_of() {
    case $1 in
        collector-managed) printf '%s\n' "$deployment/observability/collector" ;;
        grafana) printf '%s\n' "$deployment/observability/grafana/provisioning" ;;
        *) printf '%s\n' "$deployment/observability/$1" ;;
    esac
}

# The running services of the stack whose files changed since they read them, one per line. For
# each service it leaves running, a deploy records the fingerprint of the files the service read,
# in state/read/, so the files of a deploy that stopped before its services read them still
# count as changed at the next deploy. A service that a deploy starts reads the files as it
# starts, so the deploy finds these before it starts any.
stale_services() {
    for service in $stack_services; do
        if [ -z "$(running "$service")" ]; then continue; fi
        if [ "$(cat "$state/read/$service" 2> /dev/null)" != "$(fingerprint "$(configuration_of "$service")")" ]; then
            printf '%s\n' "$service"
        fi
    done
}

# Records the fingerprint of the files that each running service of the stack has read.
record_configuration() {
    mkdir -p "$state/read"
    for service in $stack_services; do
        if [ -n "$(running "$service")" ]; then
            fingerprint "$(configuration_of "$service")" > "$state/read/$service"
        else
            rm -f "$state/read/$service"
        fi
    done
}

# Makes each service in $1, which runs with configuration files that changed since it read them,
# read them again: Prometheus and Alertmanager reload on SIGHUP, and the others restart. The
# deploy reloads Caddy itself.
reread_configuration() {
    for service in $1; do
        say "$service's configuration changed; it reads it again"
        case $service in
            prometheus | alertmanager) compose kill --signal SIGHUP "$service" ;;
            *) compose restart "$service" ;;
        esac
    done
}

# Reads and checks the settings this script acts on.
read_settings() {
    if [ ! -f "$deployment/settings.env" ]; then
        fail "$deployment/settings.env is missing; copy settings.env.example there and set its values"
    fi
    database=$(setting DATABASE bundled)
    telemetry=$(setting TELEMETRY self-hosted)
    replicas=$(setting API_REPLICAS 2)
    pool=$(setting DATABASE_MAX_CONNECTIONS 10)
    connections=$(setting POSTGRES_MAX_CONNECTIONS 100)
    whole_number API_REPLICAS "$replicas" 1 20
    whole_number DATABASE_MAX_CONNECTIONS "$pool" 1 1000
    whole_number POSTGRES_MAX_CONNECTIONS "$connections" 20 10000
    if [ -z "$(setting API_ADDRESS)" ]; then fail "API_ADDRESS is required"; fi
    profiles=""
    case $database in
        bundled)
            profiles="$profiles --profile database"
            for name in BACKUP_S3_ENDPOINT BACKUP_S3_REGION BACKUP_S3_BUCKET; do
                if [ -z "$(setting "$name")" ]; then fail "$name is required with DATABASE=bundled, for its backups (docs/operations/backups.md)"; fi
            done
            whole_number BACKUP_RETENTION_DAYS "$(setting BACKUP_RETENTION_DAYS 28)" 1 3650
            whole_number BACKUP_HOUR "$(setting BACKUP_HOUR 3)" 0 23
            ;;
        managed) ;;
        *) fail "DATABASE is bundled or managed, not '$database'" ;;
    esac
    case $telemetry in
        self-hosted)
            profiles="$profiles --profile telemetry --profile observability"
            if [ -z "$(setting GRAFANA_ADDRESS)" ]; then fail "GRAFANA_ADDRESS is required with TELEMETRY=self-hosted"; fi
            ;;
        managed)
            profiles="$profiles --profile managed-telemetry"
            case $(setting MANAGED_OTLP_ENDPOINT) in
                https://?*) ;;
                *) fail "MANAGED_OTLP_ENDPOINT is the managed backend's https URL, with TELEMETRY=managed (OPS-20)" ;;
            esac
            if [ -z "$(setting MANAGED_OTLP_USER)" ]; then fail "MANAGED_OTLP_USER is required with TELEMETRY=managed"; fi
            case $(setting MANAGED_GRAFANA_URL) in
                '' | https://?*) ;;
                *) fail "MANAGED_GRAFANA_URL is the managed Grafana's https URL, or empty (OPS-20)" ;;
            esac
            ;;
        off) ;;
        *) fail "TELEMETRY is self-hosted, managed or off, not '$telemetry'" ;;
    esac
}

# Fails unless the connections of both colors' replicas and of migrate fit within what Postgres
# allows the roles that aren't superusers, as docs/operations/deploy.md's Scaling says (OPS-21).
check_connections() {
    needed=$((2 * replicas * (pool + 1) + 1))
    allowed=$((connections - 3))
    if [ "$needed" -gt "$allowed" ]; then
        fail "a deploy runs 2 colors of $replicas replicas, each opening up to $((pool + 1)) connections, and migrate 1, which is $needed; Postgres allows $allowed (POSTGRES_MAX_CONNECTIONS less 3). Lower API_REPLICAS or DATABASE_MAX_CONNECTIONS, or raise POSTGRES_MAX_CONNECTIONS"
    fi
}

random() { od -An -N24 -tx1 /dev/urandom | tr -d ' \n'; }

# Writes the secrets the deployment generates, once those the operator holds are there: the
# passwords of the bundled Postgres's other roles and the URLs made from them, and Grafana's
# administrator password. The files may be read by every user of a container, as Compose mounts
# them, so the directory admits only the deployment's owner.
write_secrets() {
    secrets=$deployment/secrets
    umask 022
    mkdir -p "$secrets/alertmanager"
    chmod 700 "$secrets"
    check_secrets
    # The bundled database's URLs, written the first time it runs, and again when they name
    # another database, such as the managed one before DATABASE=bundled, whose URLs move aside.
    # The superuser's password is the operator's, which `just server-setup` writes and an existing
    # data directory keeps; bootstrap sets the other roles' passwords from their URLs.
    if [ "$database" = bundled ] && ! grep -q '@postgres:5432/api$' "$secrets/database_url" 2> /dev/null; then
        for name in database_url database_owner_url database_admin_url; do
            if [ -s "$secrets/$name" ]; then
                mv "$secrets/$name" "$secrets/$name.before-bundled"
                say "moved $secrets/$name, which named another database, to $name.before-bundled"
            fi
        done
        # The superuser's password is percent-encoded byte by byte, since a lost server's may
        # hold any character, and some, such as /, end a URL's password.
        admin=$(printf '%s' "$(cat "$secrets/postgres_password")" | od -An -tx1 -v | tr -d ' \n' | sed 's/../%&/g')
        owner=$(random) app=$(random)
        printf 'postgres://postgres:%s@postgres:5432/api' "$admin" > "$secrets/database_admin_url"
        printf 'postgres://owner:%s@postgres:5432/api' "$owner" > "$secrets/database_owner_url"
        printf 'postgres://app:%s@postgres:5432/api' "$app" > "$secrets/database_url"
        say "wrote the bundled database's passwords to $secrets"
    fi
    # A file that exists empty stands in for a secret the services didn't use before, so it counts
    # as missing.
    if [ "$telemetry" = self-hosted ] && [ ! -s "$secrets/grafana_admin_password" ]; then
        random > "$secrets/grafana_admin_password"
        say "wrote Grafana's administrator password to $secrets/grafana_admin_password"
    fi
    for name in database_url database_owner_url database_admin_url; do
        if [ ! -f "$secrets/$name" ]; then fail "$secrets/$name is missing (docs/operations/deploy.md)"; fi
    done
    # Compose checks every secret file it declares, so the ones this choice of services doesn't
    # use exist empty.
    for name in postgres_password backup_s3_key backup_s3_key_secret backup_cipher_pass grafana_admin_password managed_otlp_token; do
        if [ ! -f "$secrets/$name" ]; then : > "$secrets/$name"; fi
    done
}

# Fails unless secrets/ holds each secret that the operator holds for these settings, which
# `just server-setup` writes.
check_secrets() {
    secrets=$deployment/secrets
    if [ "$database" = managed ]; then
        for name in database_url database_owner_url database_admin_url; do
            if [ ! -s "$secrets/$name" ]; then fail "$secrets/$name is missing (docs/operations/deploy.md)"; fi
        done
        # Replicas that a deploy to a managed database starts with the bundled database's URLs
        # would use the bundled Postgres, which the deploy then stops.
        if grep -q '@postgres:5432/api$' "$secrets/database_url"; then
            fail "$secrets/database_url names the bundled database; write the managed database's URLs into database_admin_url, database_owner_url and database_url first (docs/operations/deploy.md)"
        fi
    fi
    if [ "$database" = bundled ]; then
        for name in postgres_password backup_s3_key backup_s3_key_secret backup_cipher_pass; do
            if [ ! -s "$secrets/$name" ]; then fail "$secrets/$name is missing (docs/operations/deploy.md)"; fi
        done
    fi
    if [ "$telemetry" = managed ] && [ ! -s "$secrets/managed_otlp_token" ]; then
        fail "$secrets/managed_otlp_token is missing (docs/operations/observability.md)"
    fi
    if [ "$telemetry" = managed ] && [ -n "$(setting MANAGED_GRAFANA_URL)" ] && [ ! -s "$secrets/managed_grafana_token" ]; then
        fail "$secrets/managed_grafana_token is missing (docs/operations/observability.md#a-managed-backend)"
    fi
}

# The color whose replicas serve: the one that runs, or, while both run because a deploy
# stopped before it finished, the one the last deploy that finished recorded.
live_color() {
    blue=$(running api-blue)
    green=$(running api-green)
    if [ -n "$blue" ] && [ -n "$green" ]; then
        cat "$state/live" 2> /dev/null || fail "both colors run, and no deploy recorded which serves; stop one with docker compose"
    elif [ -n "$blue" ]; then
        echo blue
    elif [ -n "$green" ]; then
        echo green
    fi
}

# Writes the values that Compose interpolates besides the settings, into $1.
write_deploy_env() {
    {
        printf '%s\n' "API_IMAGE_BLUE=$image_blue"
        printf '%s\n' "API_IMAGE_GREEN=$image_green"
        printf '%s\n' "API_IMAGE_NEW=$api_image"
        printf '%s\n' "POSTGRES_IMAGE=$postgres_image"
        if [ "$telemetry" = off ]; then echo "API_TELEMETRY_ENDPOINT="; else echo "API_TELEMETRY_ENDPOINT=http://collector:4318"; fi
        if [ "$telemetry" = off ]; then echo "PROXY_TRACES_EXPORTER=none"; else echo "PROXY_TRACES_EXPORTER=otlp"; fi
        if [ -f "$deployment/certs/backup-ca.crt" ]; then echo "BACKUP_CA_FILE=/etc/deployment-certs/backup-ca.crt"; fi
        if [ -f "$deployment/certs/managed-otlp-ca.crt" ]; then echo "MANAGED_OTLP_CA_FILE=/etc/deployment-certs/managed-otlp-ca.crt"; fi
    } > "$1"
}

# Writes the sites Caddy serves and the replicas it names, for this choice of services.
write_caddy_state() {
    mkdir -p "$state/caddy/sites"
    for site in "$state"/caddy/sites/*.caddy; do
        if [ -f "$site" ]; then rm "$site"; fi
    done
    cp "$deployment/caddy/sites/api.caddy" "$state/caddy/sites/"
    if [ "$telemetry" = self-hosted ]; then cp "$deployment/caddy/sites/grafana.caddy" "$state/caddy/sites/"; fi
    upstreams="to"
    for color in blue green; do
        replica=1
        while [ "$replica" -le "$replicas" ]; do
            upstreams="$upstreams $project-api-$color-$replica:8080"
            replica=$((replica + 1))
        done
    done
    printf '%s\n' "$upstreams" > "$state/caddy/upstreams"
}

# The services that run once and would restart, because their configuration differs from the
# running ones'. The bundled Postgres counts only while the settings choose it: otherwise the deploy
# stops it instead.
changed_single_services() {
    services=caddy
    if [ "$database" = bundled ]; then services="caddy postgres"; fi
    for service in $services; do
        container=$(running "$service")
        if [ -z "$container" ]; then continue; fi
        current=$(docker inspect --format '{{ index .Config.Labels "com.docker.compose.config-hash" }}' "$container")
        expected=$(compose config --hash "$service" | cut -d ' ' -f 2)
        if [ "$current" != "$expected" ]; then printf '%s\n' "$service"; fi
    done
}

# Whether Caddy's health checks find every replica of the color $1 ready. Caddy updates the
# gauge it reports every 10 seconds.
caddy_finds_ready() {
    metrics=$(compose exec -T caddy wget -qO- http://127.0.0.1:2019/metrics) || return 1
    replica=1
    while [ "$replica" -le "$replicas" ]; do
        if ! printf '%s\n' "$metrics" | grep -qx "caddy_reverse_proxy_upstreams_healthy{upstream=\"$project-api-$1-$replica:8080\"} 1"; then
            return 1
        fi
        replica=$((replica + 1))
    done
}

# Removes the replicas of the color $1, after a deploy that failed.
roll_back() {
    compose logs --tail 20 "api-$1" >&2 || true
    compose rm --stop --force "api-$1" > /dev/null 2>&1 || true
    fail "$1's replicas of $api_image didn't become ready, so the deploy removed them; the live replicas never stopped"
}

# Prepares pgBackRest's stanza in the bucket, and takes the first full backup, so a restore can
# reach any moment from the first deploy that runs the bundled database on.
prepare_backups() {
    status=0
    compose exec -T --user postgres postgres pgbackrest --stanza=db stanza-create || status=$?
    # pgBackRest's error 28: the bucket's backups are of another database than the one Postgres
    # runs, such as on a new server, where Postgres has just created an empty one.
    case $status in
        0) ;;
        28) fail "the backup bucket holds the backups of another database. If they are this deployment's, as on a new server, run \`just restore\`, then deploy again; docs/operations/backups.md says how" ;;
        *) fail "pgBackRest couldn't prepare the backup bucket; its error is above" ;;
    esac
    if ! compose exec -T --user postgres postgres pgbackrest --stanza=db info --output=json | grep -q '"backup":\[{'; then
        say "taking the first full backup"
        full_backup
    fi
}

# One list, so that under a condition too, which ignores -e, no failed backup counts as completed.
full_backup() {
    compose exec -T --user postgres postgres pgbackrest --stanza=db backup --type=full &&
        compose exec -T --user postgres postgres touch /var/lib/backup-status/backup-completed
}

apply() {
    api_image=$1
    postgres_image=$2
    allow_restart=$3
    for image in "$api_image" "$postgres_image"; do
        case $image in
            *@sha256:*) ;;
            *) fail "$image isn't named by its digest, such as name@sha256:... (OPS-13)" ;;
        esac
    done
    lock
    install_release
    read_settings
    check_connections
    write_secrets
    mkdir -p "$deployment/certs"

    live=$(live_color)
    if [ "$live" = blue ]; then target=green; else target=blue; fi
    if [ -n "$live" ]; then live_image=$(docker inspect --format '{{ .Config.Image }}' "$(running "api-$live")"); else live_image=$api_image; fi
    if [ "$target" = blue ]; then image_blue=$api_image image_green=$live_image; else image_blue=$live_image image_green=$api_image; fi

    say "pulling $api_image"
    docker pull --quiet "$api_image" > /dev/null
    if [ "$database" = bundled ]; then
        docker pull --quiet "$postgres_image" > /dev/null
        # An image built from the same files as the running one is the same Postgres, so the
        # running one stays, and Postgres doesn't restart.
        container=$(running postgres)
        if [ -n "$container" ]; then
            running_image=$(docker inspect --format '{{ .Config.Image }}' "$container")
            inputs='{{ index .Config.Labels "build.inputs" }}'
            if [ "$(docker image inspect --format "$inputs" "$running_image")" = "$(docker image inspect --format "$inputs" "$postgres_image")" ]; then
                postgres_image=$running_image
            fi
        fi
    fi

    write_deploy_env "$state/deploy.env.next"
    previous=$state/deploy.env
    if [ ! -f "$previous" ]; then cp "$state/deploy.env.next" "$previous"; fi
    cp "$previous" "$state/deploy.env.previous"
    cp "$state/deploy.env.next" "$state/deploy.env"
    changed=$(changed_single_services)
    if [ -n "$changed" ] && [ "$allow_restart" != yes ]; then
        cp "$state/deploy.env.previous" "$state/deploy.env"
        fail "this release changes $(printf '%s\n' "$changed" | tr '\n' ' ')which runs once, so deploying it restarts it and interrupts requests for a few seconds. Deploy it again with --allow-restart when that's acceptable"
    fi
    write_caddy_state

    stale=$(stale_services)
    say "starting the services the replicas use"
    single="caddy"
    shared=""
    if [ "$database" = bundled ]; then single="$single postgres" shared="$shared backup"; fi
    if [ "$telemetry" = self-hosted ]; then shared="$shared collector prometheus alertmanager loki tempo grafana"; fi
    if [ "$telemetry" = managed ]; then shared="$shared collector-managed"; fi
    if [ "$allow_restart" = yes ]; then recreate=""; else recreate="--no-recreate"; fi
    # The collectors keep their state in volumes that only their user may write.
    if [ "$telemetry" != off ]; then compose --profile tasks run --rm --no-deps --quiet-pull collector-state; fi
    # shellcheck disable=SC2086 # $recreate is an option or nothing, and $single and $shared hold one service per word.
    if ! compose up --detach --wait --quiet-pull $recreate $single || { [ -n "$shared" ] && ! compose up --detach --wait --quiet-pull $shared; }; then
        compose logs --tail 30 $single $shared >&2 || true
        fail "the services the replicas use didn't start; their logs are above"
    fi
    reread_configuration "$stale"
    record_configuration
    compose exec -T caddy caddy reload --config /etc/caddy/Caddyfile --adapter caddyfile
    if [ "$database" = bundled ]; then prepare_backups; fi

    say "running bootstrap and migrate from $api_image"
    compose --profile tasks run --rm --no-deps --quiet-pull bootstrap
    compose --profile tasks run --rm --no-deps --quiet-pull migrate

    say "starting $replicas $target replicas"
    if ! compose up --detach --wait --quiet-pull --wait-timeout 180 --no-deps --scale "api-$target=$replicas" "api-$target"; then
        roll_back "$target"
    fi
    waited=0
    until caddy_finds_ready "$target"; do
        waited=$((waited + 1))
        if [ "$waited" -gt 60 ]; then roll_back "$target"; fi
        sleep 1
    done
    if [ -n "$live" ]; then
        say "stopping the $live replicas, which drain first"
        compose stop "api-$live"
        compose rm --force "api-$live" > /dev/null
    fi
    # The release's replicas serve, so its files stay, whatever follows.
    rm -rf "$state/previous"
    printf '%s\n' "$target" > "$state/live"
    stop_unused_services
    remove_unused
    say "deployed $api_image: $replicas $target replicas serve"
    if [ "$telemetry" = managed ]; then import_into_managed_grafana; fi
    # A deployment may run without an integration, but not without knowing.
    if [ "$telemetry" = self-hosted ] && ! grep -Eq '^[[:space:]]*[a-z]+_configs:' "$deployment/observability/alertmanager/alertmanager.yaml"; then
        echo "deploy: warning: Alertmanager's receivers name no integration, so the alerts notify no one; docs/operations/observability.md#alerts says how to add one" >&2
    fi
}

# Imports the release's dashboards and alert rules into the managed Grafana (OBS-13, OBS-14), once
# the release's replicas serve, so a deploy that fails leaves the serving release's there. A failed
# import only warns, since a managed backend that is down mustn't keep a release from deploying.
import_into_managed_grafana() {
    grafana_url=$(setting MANAGED_GRAFANA_URL)
    if [ -z "$grafana_url" ]; then
        echo "deploy: warning: MANAGED_GRAFANA_URL is empty, so no dashboard or alert rule reaches the managed backend, and no alert fires; docs/operations/observability.md#a-managed-backend says how to set it" >&2
        return
    fi
    tools=$deployment/tools/compose.yaml
    if ! DEPLOYMENT=$deployment MANAGED_GRAFANA_URL=$grafana_url \
        CURL=$(docker compose --file "$tools" config --images curl) \
        YQ=$(docker compose --file "$tools" config --images yq) sh "$deployment/import-grafana.sh"; then
        echo "deploy: warning: the import into the managed Grafana failed, so it keeps the dashboards and alert rules of the release it last imported; the error is above, and deploying this release again imports them again" >&2
    fi
}

# Stops the services that an earlier DATABASE or TELEMETRY started and the settings don't
# choose, once no replica uses them. Their volumes, with the bundled database's data, stay, but
# the bundled database's backup status goes with it, so no alert watches backups that the
# deployment doesn't take.
stop_unused_services() {
    unused=""
    if [ "$database" != bundled ]; then unused="$unused postgres backup"; fi
    if [ "$telemetry" != self-hosted ]; then unused="$unused collector prometheus alertmanager loki tempo grafana"; fi
    if [ "$telemetry" != managed ]; then unused="$unused collector-managed"; fi
    left=""
    for service in $unused; do
        container=$(running "$service")
        if [ -z "$container" ]; then continue; fi
        say "stopping $service, which DATABASE=$database and TELEMETRY=$telemetry don't use; its volumes stay"
        if [ "$service" = postgres ]; then left=$(docker inspect --format '{{ .Config.Image }}' "$container"); fi
        # As long as Compose would give Postgres, the longest of the services' grace periods.
        docker stop --time 60 "$container" > /dev/null
        docker rm "$container" > /dev/null
    done
    # Postgres archives its last segment as it stops, which sets a status file, so the status
    # goes once it has stopped, from a container of the image it ran.
    if [ -n "$left" ]; then
        docker run --rm --volume "${project}_backup-status:/var/lib/backup-status" --entrypoint sh "$left" \
            -c 'rm -f /var/lib/backup-status/*'
    fi
}

# Removes what earlier deploys left and nothing uses, once a deploy's replicas serve, so the disk
# doesn't fill with them (OPS-16): the files of every other release in releases/, where `just
# deploy` copies a release's files, and the images of the deployment's repositories that no
# container uses and that neither the services the settings choose nor the tools name. Those are
# the images of the releases before the previous one, of the pins a release moved, of the
# services the settings no longer choose, and the Postgres images that a deploy pulled and kept
# the running one in place of. The previous release's API image stays, which deploy.env names for
# the color that stopped, so a rollback to it pulls nothing. An image of another repository, such
# as one the operator pulled, stays, and Docker removes no image that a container uses. Nothing
# here fails the deploy, whose replicas already serve.
remove_unused() {
    for release in "$deployment"/releases/*; do
        if [ -d "$release" ] && [ "$release" != "$here" ]; then rm -rf "$release"; fi
    done
    tools=$deployment/tools/compose.yaml
    # A reference's repository: the reference without its digest, and without its tag, which
    # follows the last colon after the last slash, as a registry's port doesn't.
    repositories=$({ compose --profile '*' config --images && docker compose --file "$tools" config --images; } 2> /dev/null |
        awk '{ sub(/@.*/, ""); n = split($0, part, "/"); if (index(part[n], ":") > 0) sub(/:[^:\/]*$/, ""); print }' | sort -u)
    kept=$(
        { compose config --images && docker compose --file "$tools" config --images; } 2> /dev/null |
            while IFS= read -r image; do docker image inspect --format '{{ .Id }}' "$image" 2> /dev/null || true; done
        docker ps --all --quiet | while IFS= read -r container; do docker inspect --format '{{ .Image }}' "$container" 2> /dev/null || true; done
    )
    unused=$(docker image ls --digests --no-trunc --format '{{ .ID }} {{ .Repository }} {{ .Tag }} {{ .Digest }}' |
        while read -r id repository tag digest; do
            if ! printf '%s\n' "$repositories" | grep -qxF "$repository" || printf '%s\n' "$kept" | grep -qxF "$id"; then continue; fi
            if [ "$tag" != '<none>' ]; then printf '%s\n' "$repository:$tag"; fi
            if [ "$digest" != '<none>' ]; then printf '%s\n' "$repository@$digest"; fi
        done | sort -u)
    if [ -z "$unused" ]; then return 0; fi
    before=$(docker image ls --quiet --no-trunc | sort -u | wc -l)
    # shellcheck disable=SC2086 # $unused holds one reference per word.
    docker image rm $unused > /dev/null 2>&1 || true
    removed=$((before - $(docker image ls --quiet --no-trunc | sort -u | wc -l)))
    if [ "$removed" -eq 1 ]; then say "removed an image that nothing uses"; fi
    if [ "$removed" -gt 1 ]; then say "removed $removed images that nothing uses"; fi
}

deploy_release() {
    version=$1
    repository=$(setting IMAGE_REPOSITORY)
    source=$(setting SOURCE)
    if [ -z "$repository" ] || [ -z "$source" ]; then fail "IMAGE_REPOSITORY and SOURCE are required"; fi
    crane=$(docker compose --file "$here/tools/compose.yaml" config --images crane)
    cosign=$(docker compose --file "$here/tools/compose.yaml" config --images cosign)
    say "verifying the release $version of $source"
    if ! verified=$(CRANE=$crane COSIGN=$cosign sh "$here/verify-release.sh" "$version" "$source" "$repository" "$repository-postgres"); then
        fail "the release $version doesn't verify, so nothing changed; the error above says why"
    fi
    apply "$(printf '%s\n' "$verified" | sed -n 1p)" "$(printf '%s\n' "$verified" | sed -n 2p)" "$2"
}

backup() {
    lock
    read_settings
    if [ "$database" != bundled ]; then fail "the database is managed, and its provider backs it up"; fi
    full_backup
    say "took a full backup"
}

restore() {
    moment=$1
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
    say "stopping the API and the backups"
    compose stop api-blue api-green backup
    compose stop postgres
    stamp=$(date -u +%Y%m%dT%H%M%SZ)
    say "restoring the database${moment:+ as it stood at $moment}; the data it replaces moves to /var/lib/postgresql/18/before-restore-$stamp"
    # As root, which owns the data directory's parent. Postgres starts only after a restore that
    # succeeded: on an empty data directory, it would create an empty database. A restore to a
    # moment replays along the timeline of the latest backup before it, the history the database
    # had then: Postgres's default, the latest timeline, branches off before a moment that an
    # earlier restore went back past, so the replay couldn't reach it, and Postgres wouldn't start.
    # shellcheck disable=SC2016 # The script expands its arguments itself.
    compose run --rm --no-deps --quiet-pull --user root --entrypoint sh postgres -c '
        set -e
        mv /var/lib/postgresql/18/docker "/var/lib/postgresql/18/before-restore-$1"
        install -d -o postgres -g postgres -m 0700 /var/lib/postgresql/18/docker
        if [ -n "$2" ]; then
            gosu postgres pgbackrest --stanza=db restore --type=time --target="$2" --target-timeline=current --target-action=promote
        else
            gosu postgres pgbackrest --stanza=db restore
        fi' sh "$stamp" "$moment"
    compose up --detach --wait --quiet-pull postgres
    waited=0
    until [ "$(compose exec -T --user postgres postgres psql -qAt -c 'SELECT pg_is_in_recovery()' 2> /dev/null)" = f ]; do
        waited=$((waited + 1))
        if [ "$waited" -gt 600 ]; then fail "the restored database is still recovering after 10 minutes; its log says why: docker compose logs postgres"; fi
        sleep 1
    done
    if [ -n "$live" ]; then
        API_IMAGE_NEW=$live_image compose --profile tasks run --rm --no-deps --quiet-pull migrate
        compose up --detach --wait --quiet-pull --no-deps --scale "api-$live=$replicas" "api-$live"
    fi
    # Online, as the scheduled backups are, while the replicas serve, so a later restore starts
    # from the restored database's timeline. Postgres archives what they write meanwhile.
    say "taking a full backup of the restored database, while the API serves"
    if ! full_backup; then
        compose up --detach --wait --quiet-pull backup
        fail "restored the database${moment:+ as it stood at $moment}, and the API serves, but the full backup after it failed, with the error above; take one with just backup, so a later restore starts from the restored database's timeline"
    fi
    compose up --detach --wait --quiet-pull backup
    say "restored the database${moment:+ as it stood at $moment}"
}

status() {
    if [ -f "$state/running" ] && ! flock -n -s "$state/running" true; then
        echo "a deploy, backup or restore is running"
    elif [ -d "$state/previous" ]; then
        echo "a deploy stopped before its replicas served; the next deploy, backup or restore puts back the deployment files it replaced"
    fi
    blue=$(running api-blue)
    green=$(running api-green)
    if [ -n "$blue" ] && [ -n "$green" ]; then
        printf '%s\n' "serving: both colors, as while a deploy switches; the last deploy that finished left $(cat "$state/live" 2> /dev/null || echo neither) serving"
    elif [ -n "$blue" ]; then
        echo "serving: blue"
    elif [ -n "$green" ]; then
        echo "serving: green"
    else
        echo "serving: no replica runs"
    fi
    echo "containers:"
    # An image without the version label, such as Prometheus's, has its version as the tag of the
    # name the container runs it by, such as prom/prometheus:v3.6.0@sha256:...
    docker ps --all --filter "label=com.docker.compose.project=$project" \
        --format '{{ .Names }}|{{ .Status }}|{{ .Label "org.opencontainers.image.version" }}|{{ .Image }}' |
        sed "s/^$project-//" | sort | awk -F '|' '{
            version = $3
            if (version == "") {
                image = $4
                sub(/@.*/, "", image)
                sub(/.*\//, "", image)
                if (index(image, ":") > 0) version = substr(image, index(image, ":") + 1)
            }
            printf "  %-26s %-34s %s\n", $1, $2, version
        }'
    container=$(running backup)
    if [ -n "$container" ]; then
        echo "backups:"
        # shellcheck disable=SC2016 # The container's shell expands its own variables.
        docker exec "$container" sh -c '
            for entry in "archived:log last archived" "archive-failed:archive last failed" "backup-completed:base backup last completed"; do
                file=/var/lib/backup-status/${entry%%:*}
                if [ -f "$file" ]; then when=$(date -u -r "$file" "+%Y-%m-%d %H:%M:%S UTC"); else when=never; fi
                printf "  %-26s %s\n" "${entry#*:}" "$when"
            done'
    fi
    # The data that each restore moved aside, which stays, so a restore can be undone, until the
    # operator removes it as docs/operations/backups.md says. It is read from the volume, whether
    # Postgres runs or not, as after a restore that failed or once DATABASE left `bundled`,
    # through Caddy's image, which every deploy runs, since the Postgres image may be gone.
    aside=""
    if [ -f "$state/deploy.env" ] && docker volume inspect "${project}_postgres" > /dev/null 2>&1; then
        profiles=""
        image=$(compose config --images caddy 2> /dev/null) || image=""
        if [ -n "$image" ]; then
            aside=$(docker run --rm --pull never --network none --volume "${project}_postgres:/data:ro" --entrypoint sh "$image" \
                -c 'for directory in /data/*/before-restore-*; do if [ -d "$directory" ]; then du -sh "$directory"; fi; done') || aside=""
        fi
    fi
    if [ -n "$aside" ]; then
        echo "moved aside by a restore, until removed (docs/operations/backups.md):"
        printf '%s\n' "$aside" | awk '{ name = $2; sub(/.*\//, "", name); printf "  %-34s %s\n", name, $1 }'
    fi
    echo "server:"
    df -P -h "$(docker info --format '{{ .DockerRootDir }}')" |
        awk 'NR == 2 { printf "  %-26s %s of %s used (%s), %s free\n", "disk", $3, $2, $5, $4 }'
    awk '/^MemTotal:/ { total = $2 } /^MemAvailable:/ { available = $2 }
        END { printf "  %-26s %.1f GiB available of %.1f GiB\n", "memory", available / 1048576, total / 1048576 }' /proc/meminfo
}

logs() {
    follow=""
    services=""
    for argument in "$@"; do
        case $argument in
            --follow) follow=--follow ;;
            -*) fail "unknown option $argument" ;;
            api) services="$services api-blue api-green" ;;
            *) services="$services $argument" ;;
        esac
    done
    if [ ! -f "$state/deploy.env" ]; then fail "no deploy has run on this server, so no service has logs"; fi
    read_settings
    case " $services " in
        "  " | *" caddy "*) ;;
        *)
            # shellcheck disable=SC2086 # $follow is an option or nothing, and $services holds one service per word.
            compose logs --tail 100 $follow $services
            return
            ;;
    esac
    if [ -z "$follow" ]; then
        # shellcheck disable=SC2086 # $services holds one service per word.
        compose logs --tail 100 $services
        requests < /dev/null
        return
    fi
    # The services' lines hold, as file 5, the pipe that Caddy's lines of the requests read, so
    # these end once those do, however they end.
    exec 4>&1
    # shellcheck disable=SC2086 # $services holds one service per word.
    compose logs --tail 100 --follow $services 5>&1 >&4 4>&- | requests
}

# Shows Caddy's lines of the requests it answered itself, which it writes to a file for the
# collector (D53), each headed as Compose heads a service's line: the last 100, and with
# --follow, the lines that follow, until this function's input ends. The shell in Caddy's
# container then ends its tail, and waits for it, since Caddy reaps no process of its own. The
# shell's printf heads each line, since BusyBox's sed holds lines back while it writes to a pipe.
requests() {
    # shellcheck disable=SC2016 # The shell in Caddy's container expands $line.
    lines='tail -n 100 '${follow:+-F}' /var/log/caddy/access.log 2> /dev/null | while IFS= read -r line; do printf "caddy requests | %s\n" "$line"; done'
    if [ -n "$follow" ]; then lines="$lines & cat > /dev/null; trap '' TERM; kill 0; wait"; fi
    compose exec -T caddy sh -c "$lines" || say "Caddy doesn't run, so its lines of the requests it answered itself aren't shown"
}

# The lines that `check` writes: a check that passed, one that failed, which fails `check`, and
# what it can't see.
passed() { printf '%s\n' "check: ok    $*"; }
failed() {
    printf '%s\n' "check: FAIL  $*"
    findings=$((findings + 1))
}
noted() { printf '%s\n' "check: note  $*"; }

# Checks what a deploy needs (OPS-22): with one argument, of the release <version>, which it
# verifies first, as a deploy does; with two, of the API's image and the Postgres image. It reads
# the settings and the secrets as a deploy does, and refuses what a deploy would refuse.
check() {
    findings=0
    docker_answers=no
    if ! docker_version=$(docker version --format '{{ .Server.Version }}' 2> /dev/null); then
        failed "Docker doesn't answer $(id -un): it doesn't run, or $(id -un) isn't in the docker group"
    else
        docker_answers=yes
        compose_version=$(docker compose version --short 2> /dev/null || true)
        buildx_version=$(docker buildx version 2> /dev/null | cut -d ' ' -f 2 || true)
        major=${docker_version%%.*}
        case $major in
            '' | *[!0-9]*) major=0 ;;
        esac
        if [ "$major" -ge 29 ] && [ -n "$compose_version" ] && [ -n "$buildx_version" ]; then
            passed "Docker $docker_version, with Compose $compose_version and Buildx $buildx_version"
        else
            failed "Docker $docker_version, with Compose ${compose_version:-missing} and Buildx ${buildx_version:-missing}; a deploy needs Docker 29 or later with both plugins"
        fi
    fi
    settled=no
    if problem=$( (read_settings && check_connections) 2>&1 > /dev/null); then
        read_settings
        settled=yes
        passed "the settings in $deployment/settings.env, and the connections they open (OPS-21)"
    else
        failed "${problem#deploy: }"
    fi
    if [ "$settled" = yes ]; then
        if problem=$( (check_secrets) 2>&1 > /dev/null); then
            passed "the secrets that the operator holds, in $deployment/secrets"
        else
            failed "${problem#deploy: }"
        fi
        check_memory
    fi
    api_image="" postgres_image=""
    if [ "$docker_answers" = yes ]; then
        if [ "$#" -eq 1 ]; then check_release "$1"; else api_image=$1 postgres_image=$2; fi
    fi
    if [ -n "$api_image" ]; then
        if pulled=$(docker pull --quiet "$api_image" 2>&1 && docker pull --quiet "$postgres_image" 2>&1); then
            passed "the server pulls $api_image and $postgres_image"
        else
            failed "the server can't pull the release's images: $(printf '%s\n' "$pulled" | tail -n 1)"
            postgres_image=""
        fi
    fi
    if [ "$settled" = yes ] && [ "$database" = bundled ] && [ -n "$postgres_image" ]; then check_bucket; fi
    if [ "$settled" = yes ] && [ "$telemetry" = managed ] && [ "$docker_answers" = yes ]; then check_backend; fi
    noted "whether the disk is encrypted can't be seen from the server; confirm it with the provider (docs/operations/deploy.md#what-a-server-needs)"
    if [ "$findings" -gt 0 ]; then fail "$findings of the checks on the server failed"; fi
    say "every check on the server passed"
}

# Verifies the release $1, as a deploy does first, and sets $api_image and $postgres_image to its
# images.
check_release() {
    repository=$(setting IMAGE_REPOSITORY)
    source=$(setting SOURCE)
    if [ -z "$repository" ] || [ -z "$source" ]; then
        failed "the release $1 can't be verified without IMAGE_REPOSITORY and SOURCE"
        return
    fi
    errors=$(mktemp)
    if verified=$(CRANE=$(docker compose --file "$here/tools/compose.yaml" config --images crane) \
        COSIGN=$(docker compose --file "$here/tools/compose.yaml" config --images cosign) \
        sh "$here/verify-release.sh" "$1" "$source" "$repository" "$repository-postgres" 2> "$errors"); then
        api_image=$(printf '%s\n' "$verified" | sed -n 1p)
        postgres_image=$(printf '%s\n' "$verified" | sed -n 2p)
        passed "the release $1 verifies: the registry login reads its images, which $source's CI signed"
    else
        failed "the release $1 doesn't verify, so a deploy of it would change nothing:"
        sed 's/^/        /' "$errors"
    fi
    rm -f "$errors"
}

# Fails the check unless the server has the memory that the services the settings choose need:
# about 2 gigabytes with the self-hosted stack, and 1 otherwise (docs/operations/deploy.md). A
# server sold with that much reports a little less, the share its kernel keeps.
check_memory() {
    needed=1
    if [ "$telemetry" = self-hosted ]; then needed=2; fi
    kilobytes=$(awk '/^MemTotal:/ { print $2 }' /proc/meminfo)
    gigabytes=$(awk -v kilobytes="$kilobytes" 'BEGIN { printf "%.1f", kilobytes * 1024 / 1e9 }')
    if awk -v kilobytes="$kilobytes" -v needed="$needed" 'BEGIN { exit !(kilobytes * 1024 >= needed * 0.9e9) }'; then
        passed "memory: $gigabytes GB, for the $needed GB that TELEMETRY=$telemetry needs"
    else
        failed "memory: $gigabytes GB, less than the $needed GB that TELEMETRY=$telemetry needs (docs/operations/deploy.md#what-a-server-needs)"
    fi
}

# Fails the check unless the release's pgBackRest reads the bucket, as the bundled Postgres would,
# with the key pair, the passphrase and the authority it would use. `info` changes nothing.
check_bucket() {
    authority=/etc/ssl/certs/ca-certificates.crt
    if [ -f "$deployment/certs/backup-ca.crt" ]; then authority=/etc/deployment-certs/backup-ca.crt; fi
    secrets=$deployment/secrets
    if answer=$(docker run --rm --user postgres --entrypoint pgbackrest \
        --env PGBACKREST_REPO1_S3_ENDPOINT="$(setting BACKUP_S3_ENDPOINT)" \
        --env PGBACKREST_REPO1_STORAGE_PORT="$(setting BACKUP_S3_PORT 443)" \
        --env PGBACKREST_REPO1_S3_REGION="$(setting BACKUP_S3_REGION)" \
        --env PGBACKREST_REPO1_S3_BUCKET="$(setting BACKUP_S3_BUCKET)" \
        --env PGBACKREST_REPO1_S3_URI_STYLE="$(setting BACKUP_S3_URI_STYLE host)" \
        --env PGBACKREST_REPO1_STORAGE_CA_FILE="$authority" \
        --volume "$secrets/backup_s3_key:/run/secrets/backup_s3_key:ro" \
        --volume "$secrets/backup_s3_key_secret:/run/secrets/backup_s3_key_secret:ro" \
        --volume "$secrets/backup_cipher_pass:/run/secrets/backup_cipher_pass:ro" \
        --volume "$deployment/certs:/etc/deployment-certs:ro" \
        "$postgres_image" --stanza=db info 2>&1); then
        found=$(printf '%s\n' "$answer" | sed -n 's/^ *status: //p' | head -n 1)
    else
        found=""
    fi
    # `info` exits 0 when it can't read the bucket too, and says why in the status.
    bucket=$(setting BACKUP_S3_BUCKET)
    case $found in
        ok) passed "the bucket $bucket answers to its key pair, and holds backups that the passphrase decrypts" ;;
        'error (missing stanza path)') passed "the bucket $bucket answers to its key pair, and holds no backups yet" ;;
        'error (no valid backups)') passed "the bucket $bucket answers to its key pair, and holds no complete backup yet" ;;
        *)
            failed "pgBackRest can't read the bucket $bucket with its key pair and the passphrase, and says:"
            printf '%s\n' "$answer" | sed 's/^/        /'
            return
            ;;
    esac
    # A database's record in the bucket, on a server without the database's volume, as after the
    # loss of a server, is one the first deploy's new database doesn't match.
    if [ "$found" != 'error (missing stanza path)' ] && ! docker volume inspect "${project}_postgres" > /dev/null 2>&1; then
        noted "the bucket holds a database's backups, and this server has no database yet, as after losing a server: the first deploy stops before its replicas start, then just restore restores them (docs/operations/backups.md#restoring-onto-a-new-server)"
    fi
}

# Fails the check unless the managed backend accepts an export of no telemetry, signed in as the
# collector would be, and its Grafana, if the settings name one, answers to the import's token.
check_backend() {
    curl=$(docker compose --file "$here/tools/compose.yaml" config --images curl)
    endpoint=$(setting MANAGED_OTLP_ENDPOINT)
    otlp_user=$(setting MANAGED_OTLP_USER)
    trust=""
    if [ -f "$deployment/certs/managed-otlp-ca.crt" ]; then trust="--cacert /run/certs/managed-otlp-ca.crt"; fi
    # shellcheck disable=SC2086 # $trust is an option and its value, or nothing.
    if answer=$(docker run --rm --quiet --volume "$deployment/secrets/managed_otlp_token:/run/token:ro" \
        --volume "$deployment/certs:/run/certs:ro" "$curl" --silent --show-error --fail-with-body $trust \
        --variable token@/run/token --expand-user "$otlp_user:{{token:trim}}" \
        --header 'Content-Type: application/json' --data '{}' "$endpoint/v1/metrics" 2>&1); then
        passed "the managed backend at $endpoint accepts an export, signed in as $otlp_user"
    else
        failed "the managed backend at $endpoint refuses an export, signed in as $otlp_user: $answer"
    fi
    grafana=$(setting MANAGED_GRAFANA_URL)
    if [ -z "$grafana" ]; then
        noted "MANAGED_GRAFANA_URL is empty, so no deploy imports the dashboards and the alert rules (docs/operations/observability.md#a-managed-backend)"
        return
    fi
    trust=""
    if [ -f "$deployment/certs/managed-grafana-ca.crt" ]; then trust="--cacert /run/certs/managed-grafana-ca.crt"; fi
    # A Grafana Cloud instance that slept answers 503 while it loads again, so the check first waits
    # for another answer, as the import does (import-grafana.sh), with the waiting request's output
    # discarded, then asks again for the answer it reports.
    # shellcheck disable=SC2086 # $trust is an option and its value, or nothing.
    docker run --rm --quiet --volume "$deployment/secrets/managed_grafana_token:/run/token:ro" \
        --volume "$deployment/certs:/run/certs:ro" "$curl" --silent --fail --retry 8 --retry-max-time 180 \
        --output /dev/null $trust --variable token@/run/token \
        --expand-header 'Authorization: Bearer {{token:trim}}' "$grafana/api/folders" > /dev/null 2>&1 || true
    # shellcheck disable=SC2086 # $trust is an option and its value, or nothing.
    if answer=$(docker run --rm --quiet --volume "$deployment/secrets/managed_grafana_token:/run/token:ro" \
        --volume "$deployment/certs:/run/certs:ro" "$curl" --silent --show-error --fail-with-body $trust \
        --variable token@/run/token --expand-header 'Authorization: Bearer {{token:trim}}' \
        "$grafana/api/folders" 2>&1); then
        passed "the managed Grafana at $grafana answers to the import's token"
    else
        failed "the managed Grafana at $grafana doesn't accept the import's token: $answer"
    fi
}

allow_restart() {
    case ${1:-} in
        '') echo no ;;
        --allow-restart) echo yes ;;
        *) fail "unknown option $1" ;;
    esac
}

command=${1:-}
case $command in
    deploy)
        if [ "$#" -lt 2 ]; then fail "usage: deploy.sh deploy <version> [--allow-restart]"; fi
        # An assignment, so that the script stops when allow_restart refuses.
        restart=$(allow_restart "${3:-}")
        deploy_release "$2" "$restart"
        ;;
    apply)
        if [ "$#" -lt 3 ]; then fail "usage: deploy.sh apply <API image> <Postgres image> [--allow-restart]"; fi
        restart=$(allow_restart "${4:-}")
        apply "$2" "$3" "$restart"
        ;;
    backup) backup ;;
    restore) restore "${2:-}" ;;
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
    *) fail "usage: deploy.sh deploy|apply|backup|restore|status|logs|check|check-images (see the comment at the top of deploy.sh)" ;;
esac
