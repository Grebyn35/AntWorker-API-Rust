# A deploy (OPS-14): the secrets the deployment generates, the colors of replicas and the
# rollback, the backups a deploy prepares, the services its settings choose, the import into a
# managed Grafana, what a deploy removes, and the release's verification.

# Writes the secrets the deployment generates, once those the operator holds are there: the
# passwords of the bundled Postgres's other roles and the URLs made from them, the API's HMAC
# key, and Grafana's administrator password. The files may be read by every user of a container, as Compose mounts
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
    # The key of the API's keyed hashes, 32 random bytes in base64, written once: a new key starts
    # every rate-limit bucket afresh (PLATFORM-057).
    if [ ! -s "$secrets/hmac_key" ]; then
        head -c 32 /dev/urandom | base64 | tr -d '\n' > "$secrets/hmac_key"
        say "wrote the API's HMAC key to $secrets/hmac_key"
    fi
    # The mail server's password, which the replicas read as a file even when the server takes
    # none: empty, the API reads it as unset.
    if [ ! -f "$secrets/smtp_password" ]; then
        : > "$secrets/smtp_password"
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
    for name in files_s3_key files_s3_key_secret; do
        if [ ! -s "$secrets/$name" ]; then fail "$secrets/$name is missing (docs/operations/deploy.md#the-files-bucket)"; fi
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
# stopped before it finished, the one that served before that deploy started the other: the one
# the last deploy that finished recorded, or, when none did, as after the first deploy stopped
# before it could, the one whose replicas were created first. A deploy's two colors start
# seconds apart at least, so their creation times differ in the second they name.
live_color() {
    blue=$(running api-blue)
    green=$(running api-green)
    if [ -n "$blue" ] && [ -n "$green" ]; then
        if [ -f "$state/live" ]; then
            cat "$state/live"
        else
            blue_created=$(docker inspect --format '{{ .Created }}' "$blue")
            green_created=$(docker inspect --format '{{ .Created }}' "$green")
            first=$(printf '%s\n' "${blue_created%%.*}" "${green_created%%.*}" | sort | head -n 1)
            if [ "$first" = "${green_created%%.*}" ] && [ "$first" != "${blue_created%%.*}" ]; then echo green; else echo blue; fi
        fi
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
        if [ -f "$deployment/certs/files-ca.crt" ]; then echo "FILES_CA_FILE=/etc/deployment-certs/files-ca.crt"; fi
        # How long the bytes of a file that no row names stay: the backups' reach with the bundled
        # database, so a restore finds every file's bytes it brings back (FILES-011).
        delay=$(setting FILE_DELETION_DELAY_DAYS)
        if [ -z "$delay" ] && [ "$database" = bundled ]; then delay=$(($(setting BACKUP_RETENTION_DAYS 28) + 7)); fi
        printf '%s\n' "FILE_DELETION_DELAY=${delay:-35}"
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
            *) fail "$image isn't named by its digest, such as name@sha256:... (OPS-11)" ;;
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
# doesn't fill with them (OPS-14): the files of every other release in releases/, where `just
# deploy` copies a release's files, and the images of the deployment's repositories that no
# container uses and that neither the services the settings choose nor the tools name. Those are
# the images of the releases before the previous one, of the pins a release moved, of the
# services the settings don't choose, and the Postgres images that a deploy pulled and kept
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
