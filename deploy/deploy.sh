#!/bin/sh
# The reference deployment's tasks on its server (D45): deploys a release with no downtime
# (OPS-16), and takes and restores the bundled database's backups (OPS-17). `just deploy`,
# `just backup` and `just restore` run it over SSH from a developer's machine, from the files of
# the release they name; docs/operations/deploy.md describes the deployment.
#
#   deploy.sh deploy <version> [--allow-restart]
#       Verifies the release <version>, then deploys its images.
#   deploy.sh apply <API image> <Postgres image> [--allow-restart]
#       Deploys the two images, each named by digest, such as ghcr.io/owner/name@sha256:...
#   deploy.sh backup
#       Takes a full base backup of the bundled database.
#   deploy.sh restore [<moment>]
#       Restores the bundled database, as it stood at <moment>, a time with its offset from UTC
#       as Postgres writes it, or at the latest moment its backups hold, into a fresh data
#       directory, and keeps the one it replaces beside it.
#
# A deploy restarts Postgres or Caddy, which each run once, only with --allow-restart, because
# restarting either interrupts requests for a few seconds. DEPLOYMENT names the deployment's
# directory, which holds settings.env, secrets/ and state/. The script first installs the files of
# its own release there, so the deployment runs the release's configuration.
set -eu

here=$(cd "$(dirname "$0")" && pwd)
deployment=$(cd "${DEPLOYMENT:?DEPLOYMENT names the deployment directory}" && pwd)
state=$deployment/state
project=deployment

say() { echo "deploy: $*"; }
fail() {
    echo "deploy: $*" >&2
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

lock() {
    mkdir -p "$state"
    if ! mkdir "$state/lock" 2> /dev/null; then
        fail "another deploy, backup or restore is running; if none is, remove $state/lock"
    fi
    trap finish EXIT
    trap 'exit 1' HUP INT TERM
}

# Runs as the script exits, once it holds the lock: a deploy that didn't finish puts back the
# deployment files it replaced, which it kept in state/previous/, then the lock goes.
finish() {
    status=$?
    if [ "$status" -ne 0 ] && [ -d "$state/previous" ]; then
        say "putting back the deployment files that this deploy replaced"
        copy_release "$state/previous" "$deployment"
    fi
    rm -rf "$state/previous"
    rmdir "$state/lock"
}

# Makes the directory $2 hold exactly the files of the directory $1, writing each file in place,
# so a container that mounts the directory or the file sees the new contents.
sync_directory() {
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

# Copies a release's deployment files from the directory $1 into the directory $2.
copy_release() {
    for file in compose.yaml deploy.sh verify-release.sh settings.env.example; do
        cp "$1/$file" "$2/$file"
    done
    for directory in caddy observability tools; do
        sync_directory "$1/$directory" "$2/$directory"
    done
}

# Installs this release's files into the deployment's directory, and keeps the ones they replace
# in state/previous/ until the deploy finishes.
install_release() {
    if [ "$here" = "$deployment" ]; then return; fi
    rm -rf "$state/previous"
    if [ -f "$deployment/compose.yaml" ]; then
        mkdir -p "$state/previous"
        copy_release "$deployment" "$state/previous"
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
        collector-managed) echo "$deployment/observability/collector" ;;
        grafana) echo "$deployment/observability/grafana/provisioning" ;;
        *) echo "$deployment/observability/$1" ;;
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
            echo "$service"
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

# Writes the secrets the deployment generates: the bundled Postgres's passwords and the URLs
# made from them, and Grafana's administrator password. The files may be read by every user of a
# container, as Compose mounts them, so the directory admits only the deployment's owner.
write_secrets() {
    secrets=$deployment/secrets
    umask 022
    mkdir -p "$secrets/alertmanager"
    chmod 700 "$secrets"
    # The bundled database's URLs, written the first time it runs, and again when they name
    # another database, such as the managed one before DATABASE=bundled, whose URLs move aside.
    # The superuser keeps its password, which an existing data directory holds; bootstrap sets the
    # other roles' passwords from their URLs.
    if [ "$database" = bundled ] && ! grep -q '@postgres:5432/api$' "$secrets/database_url" 2> /dev/null; then
        for name in database_url database_owner_url database_admin_url; do
            if [ -s "$secrets/$name" ]; then
                mv "$secrets/$name" "$secrets/$name.before-bundled"
                say "moved $secrets/$name, which named another database, to $name.before-bundled"
            fi
        done
        if [ ! -s "$secrets/postgres_password" ]; then random > "$secrets/postgres_password"; fi
        admin=$(cat "$secrets/postgres_password") owner=$(random) app=$(random)
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
    # Replicas that a deploy to a managed database starts with the bundled database's URLs would use
    # the bundled Postgres, which the deploy then stops.
    if [ "$database" = managed ] && grep -q '@postgres:5432/api$' "$secrets/database_url"; then
        fail "$secrets/database_url names the bundled database; write the managed database's URLs into database_admin_url, database_owner_url and database_url first (docs/operations/deploy.md)"
    fi
    if [ "$database" = bundled ]; then
        for name in backup_s3_key backup_s3_key_secret backup_cipher_pass; do
            if [ ! -s "$secrets/$name" ]; then fail "$secrets/$name is missing (docs/operations/deploy.md)"; fi
        done
    fi
    if [ "$telemetry" = managed ] && [ ! -s "$secrets/managed_otlp_token" ]; then
        fail "$secrets/managed_otlp_token is missing (docs/operations/observability.md)"
    fi
    # Compose checks every secret file it declares, so the ones this choice of services doesn't
    # use exist empty.
    for name in postgres_password backup_s3_key backup_s3_key_secret backup_cipher_pass grafana_admin_password managed_otlp_token; do
        if [ ! -f "$secrets/$name" ]; then : > "$secrets/$name"; fi
    done
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
        echo "API_IMAGE_BLUE=$image_blue"
        echo "API_IMAGE_GREEN=$image_green"
        echo "API_IMAGE_NEW=$api_image"
        echo "POSTGRES_IMAGE=$postgres_image"
        if [ "$telemetry" = off ]; then echo "API_TELEMETRY_ENDPOINT="; else echo "API_TELEMETRY_ENDPOINT=http://collector:4318"; fi
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
    echo "$upstreams" > "$state/caddy/upstreams"
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
        if [ "$current" != "$expected" ]; then echo "$service"; fi
    done
}

# Whether Caddy's health checks find every replica of the color $1 ready. Caddy updates the
# gauge it reports every 10 seconds.
caddy_finds_ready() {
    metrics=$(compose exec -T caddy wget -qO- http://127.0.0.1:2019/metrics) || return 1
    replica=1
    while [ "$replica" -le "$replicas" ]; do
        if ! echo "$metrics" | grep -qx "caddy_reverse_proxy_upstreams_healthy{upstream=\"$project-api-$1-$replica:8080\"} 1"; then
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

full_backup() {
    compose exec -T --user postgres postgres pgbackrest --stanza=db backup --type=full
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
        fail "this release changes $(echo "$changed" | tr '\n' ' ')which runs once, so deploying it restarts it and interrupts requests for a few seconds. Deploy it again with --allow-restart when that's acceptable"
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
    echo "$target" > "$state/live"
    stop_unused_services
    say "deployed $api_image: $replicas $target replicas serve"
    # A deployment may run without an integration, but not without knowing.
    if [ "$telemetry" = self-hosted ] && ! grep -Eq '^[[:space:]]*[a-z]+_configs:' "$deployment/observability/alertmanager/alertmanager.yaml"; then
        echo "deploy: warning: Alertmanager's receivers name no integration, so the alerts notify no one; docs/operations/observability.md#alerts says how to add one" >&2
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
        docker run --rm --volume "${project}_backup-status:/var/lib/backup-status" --entrypoint sh "$left"             -c 'rm -f /var/lib/backup-status/*'
    fi
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
    apply "$(echo "$verified" | sed -n 1p)" "$(echo "$verified" | sed -n 2p)" "$2"
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
    # succeeded: on an empty data directory, it would create an empty database.
    # shellcheck disable=SC2016 # The script expands its arguments itself.
    compose run --rm --no-deps --quiet-pull --user root --entrypoint sh postgres -c '
        set -e
        mv /var/lib/postgresql/18/docker "/var/lib/postgresql/18/before-restore-$1"
        install -d -o postgres -g postgres -m 0700 /var/lib/postgresql/18/docker
        if [ -n "$2" ]; then
            gosu postgres pgbackrest --stanza=db restore --type=time --target="$2" --target-action=promote
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
    say "taking a full backup of the restored database"
    full_backup
    compose up --detach --wait --quiet-pull backup
    if [ -n "$live" ]; then
        API_IMAGE_NEW=$live_image compose --profile tasks run --rm --no-deps --quiet-pull migrate
        compose up --detach --wait --quiet-pull --no-deps --scale "api-$live=$replicas" "api-$live"
    fi
    say "restored the database${moment:+ as it stood at $moment}"
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
    *) fail "usage: deploy.sh deploy|apply|backup|restore (see the comment at the top of deploy.sh)" ;;
esac
