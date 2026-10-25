#!/bin/sh
# Verifies a release (OPS-14): for each image named, the image tagged with the version is signed
# by the run of ci.yml on main of the repository at the source address, and its image index holds
# the SBOM and the provenance of both platforms. It prints each image with its digest, as
# `<image>@<digest>`, one per line, in the order given, and fails on the first image that doesn't
# verify. `just verify-release` runs it when CI releases a version, and the reference deployment
# runs it before it pulls a release (OPS-16).
#
#   verify-release.sh <version> <source> <image>...
#   verify-release.sh 1.4.0 https://github.com/owner/name ghcr.io/owner/name ghcr.io/owner/name-postgres
#
# CRANE and COSIGN name the images of crane and cosign to run, pinned by digest. The tools read the
# registry login in ~/.docker.
set -eu

if [ "$#" -lt 3 ]; then
    echo "usage: verify-release.sh <version> <source> <image>..." >&2
    exit 2
fi
version=$1
source=$2
shift 2

crane() { docker run --rm --user "$(id -u):$(id -g)" --volume "$HOME/.docker:/docker:ro" --env DOCKER_CONFIG=/docker "${CRANE:?the crane image}" "$@"; }
# cosign keeps Sigstore's trust data in its home, which the user can write only in /tmp.
cosign() { docker run --rm --user "$(id -u):$(id -g)" --env HOME=/tmp --volume "$HOME/.docker:/docker:ro" --env DOCKER_CONFIG=/docker "${COSIGN:?the cosign image}" "$@"; }

for image in "$@"; do
    if ! digest=$(crane digest "$image:$version"); then
        echo "$image has no image tagged $version" >&2
        exit 1
    fi
    if ! cosign verify --certificate-identity "$source/.github/workflows/ci.yml@refs/heads/main" \
        --certificate-oidc-issuer https://token.actions.githubusercontent.com "$image@$digest" > /dev/null; then
        echo "$image:$version isn't signed by the run on main of $source" >&2
        exit 1
    fi
    # Each kind of attestation, with the field that holds its document. A platform the index
    # lacks gives an empty entry, whose field is empty.
    for attestation in SBOM:SPDX Provenance:SLSA; do
        kind=${attestation%%:*}
        field=${attestation##*:}
        for platform in linux/amd64 linux/arm64; do
            found=$(docker buildx imagetools inspect "$image@$digest" \
                --format "{{ with index .$kind \"$platform\" }}{{ if .$field }}present{{ end }}{{ end }}")
            if [ "$found" != present ]; then
                echo "$image:$version has no $kind for $platform" >&2
                exit 1
            fi
        done
    done
    echo "$image@$digest"
done
