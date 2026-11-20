# What each bucket of the deployment keeps of what is deleted in it, which `just server-check`
# checks (OPS-22).

# The object that the check writes to a bucket and deletes, outside pgBackRest's path in the
# backups' bucket.
# It is outside the prefixes of the files bucket's objects too. # part: files
probe_object=server-check/probe

# Checks what the bucket whose settings start with $1 keeps of what is deleted in it, as
# BUCKETS_KEEP_DELETED_DAYS says: for that many days, where its key pair can't remove it, or, at
# 0, nothing for more than a day. Its key pair is in the secret files $2 and $2_secret, the
# authority of a private bucket's certificate in certs/$3, and $4 names it.
check_versions() {
    keep=$(setting BUCKETS_KEEP_DELETED_DAYS 7)
    bucket=$(setting "$1_BUCKET")
    if [ "$(setting "$1_URI_STYLE" host)" = path ]; then
        url="https://$(setting "$1_ENDPOINT"):$(setting "$1_PORT" 443)/$bucket"
        probe="$url/$probe_object"
    else
        url="https://$bucket.$(setting "$1_ENDPOINT"):$(setting "$1_PORT" 443)/"
        probe="$url$probe_object"
    fi
    if [ "$keep" -eq 0 ]; then check_keeps_nothing "$@"; else check_keeps_deleted "$@"; fi
}

# Fails the check while the bucket keeps noncurrent versions, S3's name for the old version that
# a bucket with versioning keeps of an object deleted or replaced, for more than a day: while its
# versioning is on or suspended, and no enabled lifecycle rule of the whole bucket expires them
# after 1 day. A bucket that answers that it implements no versioning gets a note. It reads the
# bucket's versioning and its lifecycle with S3's GetBucketVersioning and
# GetBucketLifecycleConfiguration, which change nothing.
check_keeps_nothing() {
    bucket_send "$1" "$2" "$3" GET "$url?versioning"
    if [ "$status" != 200 ]; then
        case $body in
            *'<Code>NotImplemented</Code>'*)
                noted "$4, $bucket, can't say whether it keeps noncurrent versions; with a provider that keeps them, give it a lifecycle rule that expires them after a day (docs/operations/deploy.md#what-a-server-needs)"
                ;;
            *) failed "S3 can't read the versioning of $4, $bucket, with its key pair, and says: $body" ;;
        esac
        return
    fi
    case $body in
        *'<Status>Enabled</Status>'* | *'<Status>Suspended</Status>'*) ;;
        *)
            passed "$4, $bucket, keeps no versions"
            return
            ;;
    esac
    bucket_send "$1" "$2" "$3" GET "$url?lifecycle"
    if [ -n "$(expiring_rules "$body" 1)" ]; then
        passed "$4, $bucket, keeps versions, and a lifecycle rule expires its noncurrent ones after a day"
    else
        failed "$4, $bucket, keeps versions, and no enabled lifecycle rule of the whole bucket expires its noncurrent ones after a day, so it keeps the bytes of what the deployment deletes; give it one, such as Backblaze B2's \"Keep only the last version of the file\" (docs/operations/deploy.md#what-a-server-needs)"
    fi
}

# Fails the check unless the bucket keeps what is deleted in it for $keep days, where its key pair
# can't remove it (OPS-22):
# - its versioning is on, and its lifecycle holds one NoncurrentVersionExpiration, that of an
#   enabled rule of the whole bucket, with $keep NoncurrentDays;
# - it keeps the version of an object that the check writes and deletes by its name, after the
#   check asked it to delete that version: the check reads the version again;
# - it answers 403 to a write of its lifecycle, with the rules it has, and to one of its
#   versioning, with `Enabled`, neither of which would change it.
# The object stays in the bucket as a noncurrent version, which the lifecycle expires.
check_keeps_deleted() {
    how="docs/operations/deploy.md#what-a-server-needs says how, and BUCKETS_KEEP_DELETED_DAYS=0 is for a provider that can't"
    bucket_send "$1" "$2" "$3" GET "$url?versioning"
    if [ "$status" != 200 ]; then
        failed "S3 can't read the versioning of $4, $bucket, with its key pair, and says: $body"
        return
    fi
    case $body in
        *'<Status>Enabled</Status>'*) ;;
        *)
            failed "$4, $bucket, has no versioning on, so it keeps nothing of what is deleted in it; $how"
            return
            ;;
    esac
    bucket_send "$1" "$2" "$3" GET "$url?lifecycle"
    lifecycle=$body
    if [ -z "$(only_expiry "$lifecycle" "$keep")" ]; then
        failed "$4, $bucket, doesn't expire its noncurrent versions $keep days after they become noncurrent, as BUCKETS_KEEP_DELETED_DAYS says: its lifecycle needs one NoncurrentVersionExpiration, of an enabled rule of the whole bucket, with $keep NoncurrentDays; $how"
        return
    fi
    bucket_send "$1" "$2" "$3" GET "$url?versions&max-keys=1"
    if [ "$status" != 200 ]; then
        failed "$4, $bucket, doesn't list its versions to its key pair, which reading it as it stood at a moment needs, and says: $body"
        return
    fi
    bucket_send "$1" "$2" "$3" PUT "$probe" "" "just server-check wrote this object, and deleted it, to see that the bucket keeps its version"
    kept=$version
    if [ "$status" != 200 ] || [ -z "$kept" ]; then
        failed "$4, $bucket, didn't take an object from its key pair and name its version, and says: $status $body"
        return
    fi
    bucket_send "$1" "$2" "$3" DELETE "$probe"
    case $status in
        2??) ;;
        *)
            failed "$4, $bucket, refuses its key pair the deletion of an object by its name, and says: $status $body"
            return
            ;;
    esac
    before=$findings
    # Whatever the bucket answers, the version tells: it is there, or it went.
    bucket_send "$1" "$2" "$3" DELETE "$probe" "$kept"
    bucket_send "$1" "$2" "$3" GET "$probe" "$kept"
    case $status in
        200) ;;
        404) failed "$4, $bucket, let its key pair delete a version that it kept, so whoever holds the server could delete what the bucket keeps; give it a key pair that may not delete versions: $how" ;;
        *) failed "$4, $bucket, doesn't give its key pair the version that it kept of a deleted object, which reading it as it stood at a moment needs, and says: $status $body" ;;
    esac
    bucket_send "$1" "$2" "$3" PUT "$url?lifecycle" "" "$lifecycle"
    check_refused "$4" "lifecycle rules"
    bucket_send "$1" "$2" "$3" PUT "$url?versioning" "" '<VersioningConfiguration xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><Status>Enabled</Status></VersioningConfiguration>'
    check_refused "$4" "versioning"
    if [ "$findings" -eq "$before" ]; then
        passed "$4, $bucket, keeps what is deleted in it for $keep days, and its key pair can't remove it"
    fi
}

# Fails the check unless the bucket that $1 names answered 403, S3's refusal of a request that
# its key pair may not make, to the write of its $2 that the check just sent, which holds what
# the bucket has, so that one it takes changes nothing.
check_refused() {
    case $status in
        403) ;;
        2??) failed "$1, $bucket, let its key pair write its $2, so whoever holds the server could end what the bucket keeps; give it a key pair that may not: $how" ;;
        *) failed "$1, $bucket, answered a write of its $2 with neither a refusal, 403, nor a success, so the check can't tell whether its key pair may change them: $status $body" ;;
    esac
}

# Prints the NoncurrentVersionExpiration of the lifecycle configuration $1, if it holds one alone,
# and that one expires every noncurrent version of the whole bucket $2 days after it became
# noncurrent, as expiring_rules reads it: a second one could expire some versions sooner. grep
# prints each match of the one pattern on a line of its own, GNU's and BusyBox's alike.
only_expiry() {
    if [ "$(printf '%s' "$1" | grep -o '<NoncurrentVersionExpiration>' | wc -l)" -eq 1 ]; then expiring_rules "$1" "$2"; fi
}

# Prints the NoncurrentVersionExpiration of each rule of the lifecycle configuration $1 that
# expires every noncurrent version of the whole bucket $2 days after it became noncurrent: an
# enabled rule with no filter but an empty prefix, whose NoncurrentVersionExpiration has
# NoncurrentDays $2 and keeps no newer noncurrent versions, as NewerNoncurrentVersions would. A
# transition of noncurrent versions deletes none.
expiring_rules() {
    printf '%s' "$1" | tr -d '\r\n' | sed 's/<Rule>/\
<Rule>/g' | grep '^<Rule>' | grep '<Status>Enabled</Status>' |
        grep -v -e '<Prefix>[^<]' -e '<Tag>' -e '<And>' -e '<ObjectSize' |
        sed -n 's|.*<NoncurrentVersionExpiration>\(.*\)</NoncurrentVersionExpiration>.*|\1|p' |
        grep "<NoncurrentDays>[[:space:]]*$2[[:space:]]*</NoncurrentDays>" |
        grep -v '<NewerNoncurrentVersions>' || true
}

# Sends the bucket whose settings start with $1 the request $4 of the URL $5, signed with S3's
# signature version 4 and the key pair in the secret files $2 and $2_secret, trusting the
# authority in certs/$3, if any, besides the system's: of the object's version $6, if one is
# given, and with the body $7, if one is, beside its MD5, which S3 asks of a bucket's
# configuration. It sets $status to the answer's status, $version to the version that the answer
# names in x-amz-version-id, and $body to its body; a bucket that doesn't answer leaves curl's
# error there, and 000 as the status.
bucket_send() {
    curl=$(docker compose --file "$here/tools/compose.yaml" config --images curl)
    trust=""
    if [ -f "$deployment/certs/$3" ]; then trust=/run/certs/$3; fi
    # shellcheck disable=SC2016 # The script expands its own arguments.
    answer=$(printf '%s' "${7:-}" | docker run --rm --quiet --interactive --entrypoint sh \
        --volume "$deployment/secrets/$2:/run/key:ro" --volume "$deployment/secrets/$2_secret:/run/secret:ro" \
        --volume "$deployment/certs:/run/certs:ro" "$curl" -c '
            cat > /tmp/body
            method=$1 url=$2 region=$3 trust=$4 version=$5
            set -- --silent --show-error --request "$method" --aws-sigv4 "aws:amz:$region:s3" \
                --variable key@/run/key --variable secret@/run/secret --expand-user "{{key:trim}}:{{secret:trim}}" \
                --write-out "\n%{http_code} %header{x-amz-version-id}\n"
            if [ -n "$trust" ]; then set -- "$@" --cacert "$trust"; fi
            if [ -n "$version" ]; then set -- "$@" --get --data-urlencode "versionId=$version"; fi
            if [ -s /tmp/body ]; then
                md5=$(md5sum /tmp/body | cut -d " " -f 1 | xxd -r -p | base64)
                set -- "$@" --data-binary @/tmp/body --header "Content-MD5: $md5" --header "Content-Type: application/octet-stream"
            fi
            exec curl "$@" "$url"' sh "$4" "$5" "$(setting "$1_REGION")" "$trust" "${6:-}" 2>&1) || true
    last=$(printf '%s\n' "$answer" | tail -n 1)
    status=${last%% *}
    version=${last#* }
    if [ "$version" = "$last" ]; then version=""; fi
    body=$(printf '%s\n' "$answer" | sed '$d')
}
