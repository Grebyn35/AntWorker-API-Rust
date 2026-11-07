# The bundled files bucket's own secrets (D98, D101): its key pair, its identities and its
# certificate, which the deploy writes, as write_secrets does the others.

# Writes the bundled files bucket's key pair, which only the deployment's own services use, the
# first time it runs, and again when the key pair is a provider's, as before FILES=bundled, which
# moves aside. Its access key starts with bundled-, which tells it from a provider's. Its
# identities give SeaweedFS that key pair alone. Then it issues the bucket's certificate.
write_files_secrets() {
    if ! grep -q '^bundled-' "$secrets/files_s3_key" 2> /dev/null; then
        for name in files_s3_key files_s3_key_secret; do
            if [ -s "$secrets/$name" ]; then
                mv "$secrets/$name" "$secrets/$name.before-bundled"
                say "moved $secrets/$name, which named a provider's bucket, to $name.before-bundled"
            fi
        done
        printf 'bundled-%s' "$(random)" > "$secrets/files_s3_key"
        random > "$secrets/files_s3_key_secret"
        say "wrote the bundled files bucket's key pair to $secrets"
    fi
    printf '{"identities":[{"name":"deployment","credentials":[{"accessKey":"%s","secretKey":"%s"}],"actions":["Read","Write","List"]}]}\n' \
        "$(cat "$secrets/files_s3_key")" "$(cat "$secrets/files_s3_key_secret")" > "$secrets/bundled_files_s3_config"
    issue_files_certificate
}

# Issues the bundled files bucket's certificate, for the name files on the deployment's network,
# from an authority of the deployment's own, whose certificate the replicas, restore-files and
# files-copy trust: the first time, and again once it has 30 days or less left. Each lasts 10
# years. openssl, on the server, issues both, and the authority's key goes once it has signed, so
# a new certificate comes with a new authority. SeaweedFS runs as a user of its own, so the
# certificate's key is readable by every user, as every secret is, inside the secrets' directory,
# which admits only the deployment's owner; a key that only its owner reads stalls every TLS
# handshake of the bucket.
issue_files_certificate() {
    certificate=$deployment/certs/bundled-files.crt
    if [ -s "$certificate" ] && [ -s "$secrets/bundled_files_tls_key" ] &&
        openssl x509 -checkend 2592000 -noout -in "$certificate" > /dev/null 2>&1; then
        return
    fi
    if ! command -v openssl > /dev/null; then
        fail "openssl, which issues the bundled files bucket's certificate, isn't installed on the server (docs/operations/deploy.md#what-a-server-needs)"
    fi
    work=$(mktemp -d)
    if ! {
        openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -days 3650 \
            -subj "/CN=$project files authority" -keyout "$work/ca.key" -out "$work/ca.crt" &&
            openssl req -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes -subj /CN=files \
                -keyout "$work/server.key" -out "$work/server.csr" &&
            printf 'subjectAltName=DNS:files\nbasicConstraints=CA:FALSE\nextendedKeyUsage=serverAuth\n' > "$work/extensions" &&
            openssl x509 -req -days 3650 -in "$work/server.csr" -CA "$work/ca.crt" -CAkey "$work/ca.key" \
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
    say "issued the bundled files bucket's certificate, which lasts 10 years, from an authority of the deployment's own"
}
