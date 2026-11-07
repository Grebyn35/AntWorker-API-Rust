# The helpers of every task of deploy.sh: its messages, the settings, Compose, the lock, and
# installing and putting back the release's files. deploy.sh sources it, so it shares the
# script's variables, as each file of tasks/ does.

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

# The ID of the deployment's container of the service $1, running or not, if it has one. A
# container that `compose run` starts from the service is a one-off, which this never names: one
# may run at any moment, as restore-inputs runs one without the lock.
service_container() {
    docker ps --all --quiet --filter "label=com.docker.compose.project=$project" \
        --filter "label=com.docker.compose.service=$1" --filter label=com.docker.compose.oneoff=False | head -n 1
}

# The ID of the running container of the deployment's service $1, if one runs: never a one-off,
# as for service_container.
running() {
    docker ps --quiet --filter "label=com.docker.compose.project=$project" \
        --filter "label=com.docker.compose.service=$1" --filter label=com.docker.compose.oneoff=False \
        --filter status=running | head -n 1
}

# One deploy, backup or restore at a time (OPS-14). A task takes the lock state/lock, and refuses
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
    # tasks/ holds the files that deploy.sh sources as it starts, each renamed into place too.
    if [ -d "$1/tasks" ]; then
        mkdir -p "$2/tasks"
        for file in "$2"/tasks/*; do
            if [ -f "$file" ] && [ ! -f "$1/tasks/${file##*/}" ]; then rm -f "$file"; fi
        done
        for file in "$1"/tasks/*; do
            if [ -f "$file" ]; then
                cp "$file" "$2/tasks/${file##*/}.next"
                mv "$2/tasks/${file##*/}.next" "$2/tasks/${file##*/}"
            fi
        done
    else
        rm -rf "$2/tasks"
    fi
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
    for name in FILES_S3_ENDPOINT FILES_S3_REGION FILES_S3_BUCKET; do
        if [ -z "$(setting "$name")" ]; then fail "$name is required, for the bucket of files' bytes (docs/operations/deploy.md#the-files-bucket)"; fi
    done
    whole_number FILES_S3_PORT "$(setting FILES_S3_PORT 443)" 1 65535
    case $(setting FILES_S3_URI_STYLE host) in
        host | path) ;;
        *) fail "FILES_S3_URI_STYLE is host or path, not '$(setting FILES_S3_URI_STYLE)'" ;;
    esac
    whole_number FILE_MAX_BYTES "$(setting FILE_MAX_BYTES 10485760)" 1 104857600
    if [ -n "$(setting FILE_DELETION_DELAY_DAYS)" ]; then
        whole_number FILE_DELETION_DELAY_DAYS "$(setting FILE_DELETION_DELAY_DAYS)" 1 3650
    fi
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
                *) fail "MANAGED_OTLP_ENDPOINT is the managed backend's https URL, with TELEMETRY=managed (OPS-17)" ;;
            esac
            if [ -z "$(setting MANAGED_OTLP_USER)" ]; then fail "MANAGED_OTLP_USER is required with TELEMETRY=managed"; fi
            case $(setting MANAGED_GRAFANA_URL) in
                '' | https://?*) ;;
                *) fail "MANAGED_GRAFANA_URL is the managed Grafana's https URL, or empty (OPS-17)" ;;
            esac
            ;;
        off) ;;
        *) fail "TELEMETRY is self-hosted, managed or off, not '$telemetry'" ;;
    esac
}

# Fails unless the connections of both colors' replicas and of migrate fit within what Postgres
# allows the roles that aren't superusers, as docs/operations/deploy.md's Scaling says (OPS-18).
check_connections() {
    needed=$((2 * replicas * (pool + 1) + 1))
    allowed=$((connections - 3))
    if [ "$needed" -gt "$allowed" ]; then
        fail "a deploy runs 2 colors of $replicas replicas, each opening up to $((pool + 1)) connections, and migrate 1, which is $needed; Postgres allows $allowed (POSTGRES_MAX_CONNECTIONS less 3). Lower API_REPLICAS or DATABASE_MAX_CONNECTIONS, or raise POSTGRES_MAX_CONNECTIONS"
    fi
}

random() { od -An -N24 -tx1 /dev/urandom | tr -d ' \n'; }
