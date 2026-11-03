# What the deployment runs, and its services' logs (OPS-22).

status() {
    if [ -f "$state/running" ] && ! flock -n -s "$state/running" true; then
        echo "a deploy, backup or restore is running"
    elif [ -d "$state/previous" ]; then
        echo "a deploy stopped before its replicas served; the next deploy, backup or restore puts back the deployment files it replaced"
    fi
    blue=$(running api-blue)
    green=$(running api-green)
    if [ -n "$blue" ] && [ -n "$green" ]; then
        printf '%s\n' "serving: both colors, as while a deploy switches; $(live_color) served before the other started"
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
    # The data that each restore moved aside, which stays, so a restore that failed can be put
    # back, until the operator removes it as docs/operations/backups.md says; a restore that
    # succeeded is undone by another. It is read from the volume, whether
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
