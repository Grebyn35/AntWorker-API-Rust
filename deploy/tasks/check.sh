# What a deploy needs, which `just server-check` checks (OPS-19).

# The lines that `check` writes: a check that passed, one that failed, which fails `check`, and
# what it can't see.
passed() { printf '%s\n' "check: ok    $*"; }
failed() {
    printf '%s\n' "check: FAIL  $*"
    findings=$((findings + 1))
}
noted() { printf '%s\n' "check: note  $*"; }

# Checks what a deploy needs (OPS-19): with one argument, of the release <version>, which it
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
        passed "the settings in $deployment/settings.env, and the connections they open (OPS-18)"
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
