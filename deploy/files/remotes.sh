# The rclone remotes that copy.sh and move.sh reach the buckets of files' bytes through, each
# configured from the environment, which rclone reads its remotes from, so its configuration file
# stays empty. Each reads its key pair from the secret files that Compose mounts, never from the
# command line.

# The bundled files bucket, as the remote `bundled`, over TLS, with the certificate that the
# deployment's own authority signed.
bundled_remote() {
    export RCLONE_CONFIG_BUNDLED_TYPE=s3
    export RCLONE_CONFIG_BUNDLED_PROVIDER=SeaweedFS
    export RCLONE_CONFIG_BUNDLED_ENDPOINT=https://files:8334
    export RCLONE_CONFIG_BUNDLED_REGION=us-east-1
    export RCLONE_CONFIG_BUNDLED_FORCE_PATH_STYLE=true
    RCLONE_CONFIG_BUNDLED_ACCESS_KEY_ID=$(cat /run/secrets/bundled_files_s3_key)
    RCLONE_CONFIG_BUNDLED_SECRET_ACCESS_KEY=$(cat /run/secrets/bundled_files_s3_key_secret)
    export RCLONE_CONFIG_BUNDLED_ACCESS_KEY_ID RCLONE_CONFIG_BUNDLED_SECRET_ACCESS_KEY
}

# A provider's bucket, as the remote `provider`, at the endpoint $1, in the region $2, with the URI
# style $3, host or path, reached with the key pair in the secret files $4 and $5.
provider_remote() {
    export RCLONE_CONFIG_PROVIDER_TYPE=s3
    export RCLONE_CONFIG_PROVIDER_PROVIDER=Other
    export RCLONE_CONFIG_PROVIDER_ENDPOINT="$1"
    export RCLONE_CONFIG_PROVIDER_REGION="$2"
    if [ "$3" = path ]; then path_style=true; else path_style=false; fi
    export RCLONE_CONFIG_PROVIDER_FORCE_PATH_STYLE=$path_style
    RCLONE_CONFIG_PROVIDER_ACCESS_KEY_ID=$(cat "/run/secrets/$4")
    RCLONE_CONFIG_PROVIDER_SECRET_ACCESS_KEY=$(cat "/run/secrets/$5")
    export RCLONE_CONFIG_PROVIDER_ACCESS_KEY_ID RCLONE_CONFIG_PROVIDER_SECRET_ACCESS_KEY
}

# rclone's environment: both buckets exist, and its configuration file is empty.
rclone_environment() {
    export RCLONE_S3_NO_CHECK_BUCKET=true
    export RCLONE_CONFIG=/tmp/rclone.conf
    export HOME=/tmp
    : > "$RCLONE_CONFIG"
}

# The certificates that rclone checks the buckets' against, as its options, which replace the
# system's: the deployment's authority, the system's, and the authority in the file $1, a
# provider's bucket's private one, when $1 isn't empty.
authorities() {
    printf '%s' "--ca-cert /etc/deployment-certs/bundled-files-ca.crt --ca-cert /etc/ssl/certs/ca-certificates.crt"
    if [ -n "$1" ]; then printf ' %s' "--ca-cert $1"; fi
}
