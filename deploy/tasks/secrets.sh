# The deployment's secrets (OPS-14, OPS-19): those that a deploy generates, the check of those
# that the operator holds, and the replacement of a key that a deploy generated.

# Writes the secrets the deployment generates, once those the operator holds are there: the
# passwords of the bundled Postgres's other roles and the URLs made from them, the API's HMAC
# key, and Grafana's secret key. The files may be read by every
# user of a container, as Compose mounts them, so the directory admits only the deployment's
# owner.
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
    # The key of the API's keyed hashes, written once: a new key starts every rate-limit bucket
    # afresh (PLATFORM-057), so only `replace-key` writes another.
    if [ ! -s "$secrets/hmac_key" ]; then
        hmac_key > "$secrets/hmac_key"
        say "wrote the API's HMAC key to $secrets/hmac_key"
    fi
    # The mail server's password, which the replicas read as a file even when the server takes
    # none: empty, the API reads it as unset.
    if [ ! -f "$secrets/smtp_password" ]; then
        : > "$secrets/smtp_password"
    fi
    # part: github
    # The secret of GitHub's client, empty while the deployment doesn't use the client, which the
    # API reads as unset (AUTH-081).
    if [ ! -f "$secrets/github_client_secret" ]; then
        : > "$secrets/github_client_secret"
    fi
    # end part: github
    # part: google
    # The secret of each of Google's clients, empty for a client the deployment doesn't use, which
    # the API reads as unset (AUTH-081).
    for client in google_client_secret google_desktop_client_secret; do
        if [ ! -f "$secrets/$client" ]; then
            : > "$secrets/$client"
        fi
    done
    # end part: google
    # part: openid_connect
    # The secret of the client of the provider that the settings name, empty while the deployment
    # doesn't use the client, which the API reads as unset (AUTH-081).
    if [ ! -f "$secrets/openid_connect_client_secret" ]; then
        : > "$secrets/openid_connect_client_secret"
    fi
    # end part: openid_connect
    # The key under which Grafana encrypts what it stores and hashes its sessions' tokens. A file
    # that exists empty stands in for a secret the services didn't use before, so it counts as
    # missing.
    if [ "$telemetry" = self-hosted ] && [ ! -s "$secrets/grafana_secret_key" ]; then
        random > "$secrets/grafana_secret_key"
    fi
    for name in database_url database_owner_url database_admin_url; do
        if [ ! -f "$secrets/$name" ]; then fail "$secrets/$name is missing (docs/operations/deploy.md)"; fi
    done
    # Compose checks every secret file it declares, so the ones this choice of services doesn't
    # use exist empty.
    for name in postgres_password backup_s3_key backup_s3_key_secret backup_cipher_pass grafana_admin_password grafana_secret_key managed_otlp_token; do
        if [ ! -f "$secrets/$name" ]; then : > "$secrets/$name"; fi
    done
}

# A new key of the API's keyed hashes: 32 random bytes in base64.
hmac_key() { head -c 32 /dev/urandom | base64 | tr -d '\n'; }

# Writes a new key in place of $1, one of the two keys that a deploy generates (OPS-19): the key
# of the API's keyed hashes, or the one under which Grafana encrypts what it stores. It writes a
# new file, as `just server-setup` writes a secret, so the services that run keep the key they
# started with, and the next deploy starts its replicas, or recreates Grafana, with the new one.
replace_key() {
    lock
    secrets=$deployment/secrets
    case $1 in
        hmac_key) key=$(hmac_key) ;;
        grafana_secret_key) key=$(random) ;;
        *) fail "replace-key takes hmac_key or grafana_secret_key, the keys that a deploy generates, not $1 (docs/operations/deploy.md#the-keys-that-a-deploy-generates)" ;;
    esac
    # A file that is missing or empty holds no key yet: the first deploy that needs one writes it.
    if [ ! -s "$secrets/$1" ]; then fail "$secrets/$1 holds no key to replace: the first deploy that needs one writes it"; fi
    umask 022
    printf '%s' "$key" > "$secrets/$1.next"
    mv "$secrets/$1.next" "$secrets/$1"
    say "wrote a new key to $secrets/$1, which the next deploy gives the services that read it"
}

# The values that the query of the URL on standard input gives the parameter $1, one per line,
# as the URL writes them: what stands between the URL's first `?` and its first `#`, if any,
# split at each `&`.
query_values() {
    tr -d '\n' | sed 's/#.*//' | sed -n 's/^[^?]*?//p' | tr '&' '\n' | sed -n "s/^$1=//p"
}

# Fails unless secrets/ holds each secret that the operator holds for these settings, which
# `just server-setup` writes.
check_secrets() {
    secrets=$deployment/secrets
    if [ ! -s "$secrets/encryption_keys" ]; then fail "$secrets/encryption_keys is missing: just server-setup generates it (docs/operations/deploy.md#the-encryption-keys)"; fi
    if [ "$database" = managed ]; then
        for name in database_url database_owner_url database_admin_url; do
            if [ ! -s "$secrets/$name" ]; then fail "$secrets/$name is missing (docs/operations/deploy.md)"; fi
        done
        # Replicas that a deploy to a managed database starts with the bundled database's URLs
        # would use the bundled Postgres, which the deploy then stops.
        if grep -q '@postgres:5432/api$' "$secrets/database_url"; then
            fail "$secrets/database_url names the bundled database; write the managed database's URLs into database_admin_url, database_owner_url and database_url first (docs/operations/deploy.md)"
        fi
        # A managed database is reached over TLS that checks its certificate and its name
        # (OPS-17): without `sslmode`, the driver falls back to plain text. The API refuses a
        # URL that names a parameter twice, so the one this reads is the one the driver reads.
        for name in database_url database_owner_url database_admin_url; do
            if [ "$(query_values sslmode < "$secrets/$name")" != verify-full ]; then
                fail "$secrets/$name doesn't set sslmode=verify-full, once, without which the connection to a managed database may go unencrypted and unverified (docs/operations/deploy.md#setting-up-a-server)"
            fi
        done
    fi
    if [ "$database" = bundled ]; then
        for name in postgres_password backup_s3_key backup_s3_key_secret backup_cipher_pass; do
            if [ ! -s "$secrets/$name" ]; then fail "$secrets/$name is missing (docs/operations/deploy.md)"; fi
        done
    fi
    # part: files
    for name in files_s3_key files_s3_key_secret; do
        if [ ! -s "$secrets/$name" ]; then fail "$secrets/$name is missing (docs/operations/deploy.md#the-files-bucket)"; fi
    done
    # end part: files
    # The password of Grafana's administrator, which its operator chooses: the deployment
    # generates none, since nothing would make its holder change it (SEC-14). A file that exists
    # empty stands in for a secret the services didn't use before, so it counts as missing.
    if [ "$telemetry" = self-hosted ]; then
        if [ ! -s "$secrets/grafana_admin_password" ]; then
            fail "$secrets/grafana_admin_password is missing: just server-setup asks for it (docs/operations/deploy.md#grafana)"
        fi
        if [ "$(wc -m < "$secrets/grafana_admin_password")" -lt 15 ]; then
            fail "$secrets/grafana_admin_password holds fewer than 15 characters: just server-setup asks for another (docs/operations/deploy.md#grafana)"
        fi
    fi
    if [ "$telemetry" = managed ] && [ ! -s "$secrets/managed_otlp_token" ]; then
        fail "$secrets/managed_otlp_token is missing (docs/operations/observability.md)"
    fi
    if [ "$telemetry" = managed ] && [ -n "$(setting MANAGED_GRAFANA_URL)" ] && [ ! -s "$secrets/managed_grafana_token" ]; then
        fail "$secrets/managed_grafana_token is missing (docs/operations/observability.md#a-managed-backend)"
    fi
}
