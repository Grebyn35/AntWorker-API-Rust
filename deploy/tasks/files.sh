# The bundled files bucket's own secrets (D98, D101), which the deploy writes, as write_secrets
# does the others, and the move of the files' objects to the bucket that a change of FILES chooses
# (D104).

# Writes the bundled files bucket's own secrets, the first time it runs: its key pair, which only
# the deployment's services use, in files of its own, apart from a provider's bucket's, so that
# both stay while FILES changes, with an access key that starts with bundled-; the key that its
# filer signs its requests with, which SeaweedFS's identity service, unused, also needs to start
# without an error, and which makes its filer's identity service refuse requests without it; and
# its certificate. Its identities, which give SeaweedFS that key pair alone, it writes each time.
write_files_secrets() {
    if [ ! -s "$secrets/bundled_files_s3_key" ] || [ ! -s "$secrets/bundled_files_s3_key_secret" ]; then
        printf 'bundled-%s' "$(random)" > "$secrets/bundled_files_s3_key"
        random > "$secrets/bundled_files_s3_key_secret"
        say "wrote the bundled files bucket's key pair to $secrets"
    fi
    printf '{"identities":[{"name":"deployment","credentials":[{"accessKey":"%s","secretKey":"%s"}],"actions":["Read","Write","List"]}]}\n' \
        "$(cat "$secrets/bundled_files_s3_key")" "$(cat "$secrets/bundled_files_s3_key_secret")" > "$secrets/bundled_files_s3_config"
    if [ ! -s "$secrets/bundled_files_security" ]; then
        printf '[jwt.filer_signing]\nkey = "%s"\n' "$(random)" > "$secrets/bundled_files_security"
    fi
    issue_files_certificate
}

# Issues the bundled files bucket's certificate, for the name files on the deployment's network,
# from an authority of the deployment's own, whose certificate the replicas, restore-files and
# files-copy trust, when the deployment has none. Both last 100 years, so no deployment renews
# them: a renewal would give the bucket a certificate that replicas started before it don't
# trust, and the authority's key goes once it has signed, which only the server's own services
# would trust anyway. openssl, on the server, issues both. SeaweedFS runs as a user of its own,
# so the certificate's key is readable by every user, as every secret is, inside the secrets'
# directory, which admits only the deployment's owner; a key that only its owner reads stalls
# every TLS handshake of the bucket.
issue_files_certificate() {
    certificate=$deployment/certs/bundled-files.crt
    if [ -s "$certificate" ] && [ -s "$secrets/bundled_files_tls_key" ]; then return; fi
    if ! command -v openssl > /dev/null; then
        fail "openssl, which issues the bundled files bucket's certificate, isn't installed on the server (docs/operations/deploy.md#what-a-server-needs)"
    fi
    work=$(mktemp -d)
    if ! {
        openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 36500 \
            -subj "/CN=$project files authority" -keyout "$work/ca.key" -out "$work/ca.crt" &&
            openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj /CN=files \
                -keyout "$work/server.key" -out "$work/server.csr" &&
            printf 'subjectAltName=DNS:files\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n' > "$work/extensions" &&
            openssl x509 -req -days 36500 -in "$work/server.csr" -CA "$work/ca.crt" -CAkey "$work/ca.key" \
                -CAcreateserial -extfile "$work/extensions" -out "$work/server.crt"
    } > "$work/log" 2>&1; then
        cat "$work/log" >&2
        rm -rf "$work"
        fail "openssl couldn't issue the bundled files bucket's certificate, with the error above"
    fi
    cp "$work/server.key" "$secrets/bundled_files_tls_key"
    chmod 0644 "$secrets/bundled_files_tls_key"
    cp "$work/ca.crt" "$deployment/certs/bundled-files-ca.crt"
    cp "$work/server.crt" "$certificate"
    rm -rf "$work"
    say "issued the bundled files bucket's certificate, which lasts 100 years, from an authority of the deployment's own"
}

# The files bucket that the settings choose, as state/files-serving and state/files-move record
# one: bundled, or managed with the provider's bucket's settings, one per line.
files_bucket() {
    if [ "$files" = bundled ]; then
        echo bundled
        return
    fi
    echo managed
    printf '%s\n' "FILES_S3_ENDPOINT=$(setting FILES_S3_ENDPOINT)" "FILES_S3_PORT=$(setting FILES_S3_PORT 443)" \
        "FILES_S3_REGION=$(setting FILES_S3_REGION)" "FILES_S3_BUCKET=$(setting FILES_S3_BUCKET)" \
        "FILES_S3_URI_STYLE=$(setting FILES_S3_URI_STYLE host)"
    if [ -f "$deployment/certs/files-ca.crt" ]; then echo "FILES_S3_CA=yes"; fi
}

# Starts the move of the files' objects when FILES chooses another bucket than the one that the
# serving replicas use, bundled or managed, as state/files-serving records it, or goes on with a
# move that an earlier deploy began, which state/files-move records, and copies the objects of the
# bucket it leaves that the bucket it chooses lacks, before any replica of this deploy starts. A
# change of the provider's bucket while FILES stays managed moves nothing (D104). A copy that fails
# stops the deploy, while the serving replicas go on with the bucket they use.
move_files_before() {
    # A move whose settings went back to the bucket it left, before it ended, ends.
    if [ -f "$state/files-move" ] && [ "$(sed -n 1p "$state/files-move")" = "$files" ]; then rm "$state/files-move"; fi
    if [ ! -f "$state/files-move" ]; then
        if [ ! -f "$state/files-serving" ] || [ "$(sed -n 1p "$state/files-serving")" = "$files" ]; then return; fi
        cp "$state/files-serving" "$state/files-move"
    fi
    say "FILES chooses another bucket than the replicas use: copying the files' objects to it"
    if ! move_files; then
        fail "couldn't copy the files' objects to the bucket that FILES chooses, with rclone's error above, so no replica of this deploy started"
    fi
}

# Copies again, once the replicas that used the bucket it leaves have stopped, the objects that they
# stored while the deploy ran, then ends the move, and records the bucket that the replicas use.
# Into the bundled bucket, it then restarts files-copy, whose copy of every object the copy lacks,
# which it makes as it starts, ran before the objects arrived.
move_files_after() {
    if [ -f "$state/files-move" ]; then
        say "copying the files' objects that the bucket that FILES left got while the deploy ran"
        if ! move_files; then
            fail "the replicas use the bucket that FILES chooses, but copying the objects that the bucket it left got while the deploy ran failed, with rclone's error above: deploy again to copy them; until then, their downloads answer 500"
        fi
        rm "$state/files-move"
        if [ "$files" = bundled ]; then compose restart files-copy > /dev/null; fi
        say "every object of the bucket that FILES left is in the one it chooses; docs/operations/deploy.md#changing-files says what to remove"
    fi
    files_bucket > "$state/files-serving"
}

# Copies every object of the bucket that state/files-move records that the bucket the settings
# choose lacks, with the service files-move (files/move.sh), which reads the provider's bucket's
# settings from the side that is managed.
move_files() {
    from=$(sed -n 1p "$state/files-move")
    if [ "$from" = bundled ]; then provider=$(files_bucket); else provider=$(cat "$state/files-move"); fi
    if [ "$(recorded FILES_S3_CA "$provider")" = yes ]; then ca=/etc/deployment-certs/files-ca.crt; else ca=""; fi
    FILES_MOVE_FROM=$from \
        FILES_MOVE_ENDPOINT="https://$(recorded FILES_S3_ENDPOINT "$provider"):$(recorded FILES_S3_PORT "$provider")" \
        FILES_MOVE_REGION=$(recorded FILES_S3_REGION "$provider") \
        FILES_MOVE_BUCKET=$(recorded FILES_S3_BUCKET "$provider") \
        FILES_MOVE_URI_STYLE=$(recorded FILES_S3_URI_STYLE "$provider") \
        FILES_MOVE_CA_FILE=$ca \
        compose --profile tasks run --rm --no-deps --quiet-pull files-move
}

# The value of the setting $1 in $2, a bucket as files_bucket writes it.
recorded() { printf '%s\n' "$2" | sed -n "s/^$1=//p"; }
