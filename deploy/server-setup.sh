#!/bin/sh
# Sets up a server for the reference deployment, and changes its settings and secrets (OPS-22).
# `just server-setup` sends deploy/ from the developer's checkout to a temporary directory on the
# server, and runs this script there, as the user SSH signs in as: root, or a user that may run
# sudo without a password. With root, the deployment runs as the user deploy, which the script
# creates; with another user, as that user. Each run removes the script's directory as it ends.
#
#   server-setup.sh state
#       Writes what the server holds, for the questions that the developer's machine asks: the
#       user the deployment runs as, its settings, and the secrets and certificates it holds.
#   server-setup.sh apply
#       Installs Docker, unless Docker 29 or later runs, creates the deployment's user, then, as
#       that user, writes what its standard input holds, and generates the backups' passphrase
#       when the bundled database has none. Its input holds one item per line, each value in
#       base64:
#           settings <value>                    the deployment's settings.env
#           secret <name> <value>               the secret file secrets/<name>, such as
#                                               alertmanager/<file> for an alert integration's
#           cert <name> <value>                 the certificate certs/<name>
#           login <registry> <user> <token>     a login to the registry, with the token
#
# docs/operations/deploy.md describes the setup.
set -eu

here=$(cd "$(dirname "$0")" && pwd)

say() { printf '%s\n' "server-setup: $*"; }
fail() {
    printf '%s\n' "server-setup: $*" >&2
    exit 1
}

# The fingerprint of the key that signs Docker's apt repository, as Docker's documentation gives it.
docker_key=9DC858229FC7DD38854AE2D88D81803C0EBFCD88

if [ "$(id -u)" -eq 0 ]; then user=deploy; else user=$(id -un); fi

# Runs its arguments as root.
as_root() {
    if [ "$(id -u)" -eq 0 ]; then "$@"; else sudo -n "$@"; fi
}

# The home directory of the user $1, or nothing while no such user exists.
home_of() { awk -F: -v name="$1" '$1 == name { print $6 }' /etc/passwd; }

# The value of the field $1 of /etc/os-release.
os_release() { sed -n "s/^$1=//p" /etc/os-release | tr -d '"'; }

state() {
    printf 'user %s\n' "$user"
    home=$(home_of "$user")
    if [ -z "$home" ]; then return; fi
    deployment=$home/deployment
    if [ -f "$deployment/settings.env" ]; then
        printf 'settings %s\n' "$(base64 < "$deployment/settings.env" | tr -d '\n')"
    fi
    for file in "$deployment"/secrets/*; do
        if [ -f "$file" ] && [ -s "$file" ]; then printf 'secret %s\n' "${file##*/}"; fi
    done
    for file in "$deployment"/secrets/alertmanager/*; do
        if [ -s "$file" ]; then printf 'secret alertmanager/%s\n' "${file##*/}"; fi
    done
    for file in "$deployment"/certs/*; do
        if [ -s "$file" ]; then printf 'cert %s\n' "${file##*/}"; fi
    done
}

# Whether Docker 29 or later runs, with its Compose and Buildx plugins. Sets $version to the
# version that runs.
docker_ready() {
    version=$(as_root docker version --format '{{ .Server.Version }}' 2> /dev/null) || return 1
    case ${version%%.*} in
        '' | *[!0-9]*) return 1 ;;
    esac
    [ "${version%%.*}" -ge 29 ] && as_root docker compose version > /dev/null 2>&1 &&
        as_root docker buildx version > /dev/null 2>&1
}

install_docker() {
    if docker_ready; then
        say "Docker $version runs"
        return
    fi
    if command -v docker > /dev/null; then
        fail "the deployment needs Docker Engine 29 or later with its Compose and Buildx plugins, and the Docker here is ${version:-not running}; upgrade it from Docker's repository, as Docker's documentation says, then run just server-setup again"
    fi
    os=$(os_release ID)
    case $os in
        debian | ubuntu) ;;
        *) fail "Docker isn't installed, and this script installs it only on Debian and Ubuntu; install Docker Engine 29 or later with its Compose and Buildx plugins, as Docker's documentation says, then run just server-setup again" ;;
    esac
    say "installing Docker from Docker's apt repository"
    as_root env DEBIAN_FRONTEND=noninteractive apt-get update -qq
    as_root env DEBIAN_FRONTEND=noninteractive apt-get install -qq -y ca-certificates curl gnupg > /dev/null
    key=$(mktemp)
    keyring=$(mktemp -d)
    curl -fsSL "https://download.docker.com/linux/$os/gpg" -o "$key"
    fingerprint=$(gpg --homedir "$keyring" --show-keys --with-colons "$key" 2> /dev/null | awk -F: '$1 == "fpr" { print $10; exit }')
    rm -rf "$keyring"
    if [ "$fingerprint" != "$docker_key" ]; then
        rm -f "$key"
        fail "the key at download.docker.com has the fingerprint ${fingerprint:-none}, not $docker_key, which Docker's documentation gives, so nothing was installed"
    fi
    as_root install -D -m 0644 "$key" /etc/apt/keyrings/docker.asc
    rm -f "$key"
    printf 'deb [arch=%s signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/%s %s stable\n' \
        "$(dpkg --print-architecture)" "$os" "$(os_release VERSION_CODENAME)" |
        as_root tee /etc/apt/sources.list.d/docker.list > /dev/null
    as_root env DEBIAN_FRONTEND=noninteractive apt-get update -qq
    as_root env DEBIAN_FRONTEND=noninteractive apt-get install -qq -y \
        docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin > /dev/null
    if ! docker_ready; then fail "Docker was installed, but doesn't run; systemctl status docker says why"; fi
    say "installed Docker $version"
}

# Creates the deployment's user, if root signed in, and gives it root's authorized SSH keys; and
# adds the user to the docker group.
prepare_user() {
    if [ "$(id -u)" -eq 0 ] && [ -z "$(home_of "$user")" ]; then
        if command -v useradd > /dev/null; then
            useradd --create-home --shell /bin/bash "$user"
        else
            adduser -D -s /bin/sh "$user"
        fi
        say "created the user $user, which runs the deployment"
    fi
    if ! id -nG "$user" | tr ' ' '\n' | grep -qx docker; then
        if command -v usermod > /dev/null; then as_root usermod -aG docker "$user"; else as_root addgroup "$user" docker; fi
        say "added $user to the docker group"
    fi
    if [ "$(id -u)" -eq 0 ] && [ -s /root/.ssh/authorized_keys ]; then
        home=$(home_of "$user")
        group=$(id -gn "$user")
        install -d -m 0700 -o "$user" -g "$group" "$home/.ssh"
        keys=$home/.ssh/authorized_keys
        touch "$keys"
        added=0
        while IFS= read -r key; do
            if [ -n "$key" ] && ! grep -qxF -- "$key" "$keys"; then
                printf '%s\n' "$key" >> "$keys"
                added=$((added + 1))
            fi
        done < /root/.ssh/authorized_keys
        chown "$user:$group" "$keys"
        chmod 0600 "$keys"
        if [ "$added" -gt 0 ]; then say "gave $user the SSH keys that root's authorized_keys holds ($added new)"; fi
    fi
}

apply() {
    payload=$(mktemp)
    trap 'rm -rf "$payload" "$here"' EXIT
    cat > "$payload"
    install_docker < /dev/null
    prepare_user < /dev/null
    # The deployment's user reads this script, and writes in a login of its own, in which it
    # belongs to the docker group.
    chmod 0755 "$here"
    chmod 0644 "$here/server-setup.sh"
    if [ "$(id -u)" -eq 0 ]; then
        su -s /bin/sh "$user" -c "sh '$here/server-setup.sh' write" < "$payload"
    else
        # shellcheck disable=SC2024 # This script's user reads the payload, which only it may read.
        sudo -n -u "$user" sh "$here/server-setup.sh" write < "$payload"
    fi
    say "the deployment runs as $user: the other tasks sign in as $user, such as just server-check $user@<address>"
}

decode() { printf '%s' "$1" | base64 -d; }

# Writes the file $1 with the value $2, in base64, through a file renamed into place.
write_file() {
    decode "$2" > "$1.next"
    mv "$1.next" "$1"
}

write() {
    deployment=$HOME/deployment
    umask 022
    mkdir -p "$deployment/secrets" "$deployment/certs"
    chmod 0700 "$deployment/secrets"
    while read -r kind first second third; do
        case $kind in
            settings)
                write_file "$deployment/settings.env" "$first"
                say "wrote $deployment/settings.env"
                ;;
            secret)
                case $first in
                    database_admin_url | database_owner_url | database_url | postgres_password | backup_s3_key | backup_s3_key_secret | backup_cipher_pass | managed_otlp_token | managed_grafana_token) ;;
                    # A credential of an alert integration, which alertmanager.yaml names (D49).
                    alertmanager/*/* | alertmanager/.*) fail "no secret is named $first" ;;
                    alertmanager/?*)
                        case ${first#alertmanager/} in
                            *[!A-Za-z0-9_.-]*) fail "no secret is named $first" ;;
                        esac
                        mkdir -p "$deployment/secrets/alertmanager"
                        ;;
                    *) fail "no secret is named $first" ;;
                esac
                write_file "$deployment/secrets/$first" "$second"
                say "wrote $deployment/secrets/$first"
                ;;
            cert)
                case $first in
                    backup-ca.crt | managed-otlp-ca.crt | managed-grafana-ca.crt) ;;
                    *) fail "no certificate is named $first" ;;
                esac
                write_file "$deployment/certs/$first" "$second"
                say "wrote $deployment/certs/$first"
                ;;
            login)
                decode "$third" | docker login --username "$second" --password-stdin "$first" > /dev/null
                say "logged in to $first as $second"
                ;;
            *) fail "no item is named $kind" ;;
        esac
    done
    database=$(sed -n 's/^DATABASE=//p' "$deployment/settings.env" | tail -n 1)
    if [ "${database:-bundled}" = bundled ] && [ ! -s "$deployment/secrets/backup_cipher_pass" ]; then
        head -c 48 /dev/urandom | base64 | tr -d '\n' > "$deployment/secrets/backup_cipher_pass"
        say "generated the passphrase that encrypts the backups, which no task shows again:"
        printf '\n    %s\n\n' "$(cat "$deployment/secrets/backup_cipher_pass")"
        say "keep a copy off the server, such as in a password manager: without it, no backup can be restored (docs/operations/backups.md)"
    fi
}

case ${1:-} in
    state)
        trap 'rm -rf "$here"' EXIT
        state
        ;;
    apply) apply ;;
    write) write ;;
    *) fail "usage: server-setup.sh state|apply (see the comment at the top of server-setup.sh)" ;;
esac
