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
    # Each bucket's versions: the backups', once the check of its key pair passed, and the files
    # bucket's, which also checks its key pair.
    if [ "$settled" = yes ] && [ "$database" = bundled ] && [ -n "$postgres_image" ] && check_bucket; then
        check_versions BACKUP_S3 backup_s3_key backup-ca.crt "the backups' bucket"
    fi
    if [ "$settled" = yes ] && [ "$docker_answers" = yes ]; then
        check_versions FILES_S3 files_s3_key files-ca.crt "the files bucket"
    fi
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

# Fails the check unless a provider's bucket keeps no noncurrent version for more than a day, S3's
# name for the old version that a bucket with versioning keeps of an object deleted or replaced,
# as docs/operations/deploy.md requires of every bucket of the deployment: its versioning is off,
# or an enabled lifecycle rule of the whole bucket expires them after 1 day. The bucket's settings
# start with $1, its key pair is in the secret files $2 and $2_secret, the authority of a private
# bucket's certificate in certs/$3, and $4 names it. It reads the bucket's versioning and its
# lifecycle with S3's GetBucketVersioning and GetBucketLifecycleConfiguration, which change nothing.
check_versions() {
    bucket=$(setting "$1_BUCKET")
    if [ "$(setting "$1_URI_STYLE" host)" = path ]; then
        url="https://$(setting "$1_ENDPOINT"):$(setting "$1_PORT" 443)/$bucket"
    else
        url="https://$bucket.$(setting "$1_ENDPOINT"):$(setting "$1_PORT" 443)/"
    fi
    if versioning=$(bucket_request "$1" "$2" "$3" "$url?versioning"); then
        :
    elif printf '%s' "$versioning" | grep -q '<Code>NotImplemented</Code>'; then
        noted "$4, $bucket, can't say whether it keeps noncurrent versions; with a provider that keeps them, give it a lifecycle rule that expires them after a day (docs/operations/deploy.md#what-a-server-needs)"
        return
    else
        failed "S3 can't read the versioning of $4, $bucket, with its key pair, and says: $versioning"
        return
    fi
    if ! printf '%s' "$versioning" | grep -q -e '<Status>Enabled</Status>' -e '<Status>Suspended</Status>'; then
        passed "$4, $bucket, keeps no versions"
        return
    fi
    lifecycle=$(bucket_request "$1" "$2" "$3" "$url?lifecycle") || true
    # Each rule on a line of its own, then those that are enabled, expire noncurrent versions
    # after a day, and cover the whole bucket.
    expiring=$(printf '%s' "$lifecycle" | tr -d '\r\n' | sed 's/<Rule>/\
<Rule>/g' | grep '^<Rule>' | grep '<Status>Enabled</Status>' |
        grep '<NoncurrentDays>[[:space:]]*1[[:space:]]*</NoncurrentDays>' |
        grep -v -e '<Prefix>[^<]' -e '<Tag>' -e '<And>' -e '<ObjectSize' || true)
    if [ -n "$expiring" ]; then
        passed "$4, $bucket, keeps versions, and a lifecycle rule expires its noncurrent ones after a day"
    else
        failed "$4, $bucket, keeps versions, and no enabled lifecycle rule of the whole bucket expires its noncurrent ones after a day, so it keeps the bytes of what the deployment deletes; give it one, such as Backblaze B2's \"Keep only the last version of the file\" (docs/operations/deploy.md#what-a-server-needs)"
    fi
}

# Prints the answer of the bucket whose settings start with $1 to a GET of the URL $4, signed with
# S3's signature version 4 and the key pair in the secret files $2 and $2_secret, trusting the
# authority in certs/$3, if any, besides the system's; fails, printing the answer's body, when the
# bucket refuses it.
bucket_request() {
    curl=$(docker compose --file "$here/tools/compose.yaml" config --images curl)
    trust=""
    if [ -f "$deployment/certs/$3" ]; then trust="--cacert /run/certs/$3"; fi
    # shellcheck disable=SC2086 # $trust is an option and its value, or nothing.
    docker run --rm --quiet --volume "$deployment/secrets/$2:/run/key:ro" --volume "$deployment/secrets/$2_secret:/run/secret:ro" \
        --volume "$deployment/certs:/run/certs:ro" "$curl" --silent --show-error --fail-with-body $trust \
        --aws-sigv4 "aws:amz:$(setting "$1_REGION"):s3" --variable key@/run/key --variable secret@/run/secret \
        --expand-user '{{key:trim}}:{{secret:trim}}' "$4" 2>&1
}

# Fails the check, and returns 1, unless the release's pgBackRest reads the bucket, as the bundled
# Postgres would, with the key pair, the passphrase and the authority it would use. `info` changes
# nothing.
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
            return 1
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
