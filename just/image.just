# The tasks of the image: building it, scanning it, and running the end-to-end tests and the load
# test against it. The justfile imports this file.

# Prints the names of the packages that the images of `repository`, such as owner/name, are in
# the registry, separated by commas, for the workflows.
[private]
image-packages repository:
    @echo {{ lowercase(file_name(repository)) }},{{ lowercase(file_name(repository)) }}-postgres

# Builds each image once for linux/amd64 and linux/arm64, with its software bill of materials
# (SBOM) and provenance, into an OCI image layout (OPS-12, OPS-13): the API's into target/image,
# and the reference deployment's Postgres into target/image-postgres (D44). The Postgres image
# carries an identifier of the files it is built from as `build.inputs`, so a deploy restarts
# Postgres only for an image built from other files (OPS-14). With IMAGE_CACHE naming a directory,
# BuildKit reads its cache for the API's image from there, and replaces it with the cache of this
# build unless IMAGE_CACHE_WRITE is false. While the two platforms write the layer they share,
# buildx can print "ref ... locked" errors: they are retries, and the exit status says whether the
# build succeeded. The builder then keeps at most 10 gigabytes of its cache, the least recently
# used going first, and a later build compiles again what went (OPS-21).
[doc("Builds the API's and Postgres's images, for both platforms, into target/")]
[group('image')]
image: _image-builder && _image-cache
    docker buildx build --builder {{ image_builder }} --platform linux/amd64,linux/arm64 \
        --attest type=sbom,generator={{ sbom_generator_image }} --attest type=provenance,mode=max \
        --label org.opencontainers.image.version={{ version }} --label org.opencontainers.image.revision={{ image_revision }} \
        {{ image_cache_from }} {{ image_cache_to }} \
        --output type=oci,dest={{ image_dir }},tar=false .
    docker buildx build --builder {{ image_builder }} --platform linux/amd64,linux/arm64 \
        --attest type=sbom,generator={{ sbom_generator_image }} --attest type=provenance,mode=max \
        --label org.opencontainers.image.version={{ version }} --label org.opencontainers.image.revision={{ image_revision }} \
        --label build.inputs={{ postgres_inputs }} \
        --output type=oci,dest={{ image_dir }}-postgres,tar=false deploy/postgres
    docker buildx prune --builder {{ image_builder }} --force --max-used-space 10gb

# Creates the builder, if this machine doesn't have it yet, and removes the builders of earlier
# BuildKit images with their caches, which no build uses again.
[unix]
_image-builder:
    docker buildx inspect {{ image_builder }} > /dev/null 2>&1 || { docker buildx ls --format '{{{{.Name}}' | grep -xE 'buildkit-[0-9a-f]{64}' | while read -r builder; do docker buildx rm "$builder" > /dev/null; done; docker buildx create --name {{ image_builder }} --driver docker-container --driver-opt image={{ buildkit_image }} > /dev/null; }

# Creates the builder, if this machine doesn't have it yet, and removes the builders of earlier
# BuildKit images with their caches, which no build uses again.
[windows]
_image-builder:
    docker buildx inspect {{ image_builder }} *> $null; if ($LASTEXITCODE -ne 0) { docker buildx ls --format '{{{{.Name}}' | Where-Object { $_ -match '^buildkit-[0-9a-f]{64}$' } | ForEach-Object { docker buildx rm $_ | Out-Null }; docker buildx create --name {{ image_builder }} --driver docker-container --driver-opt image={{ buildkit_image }} | Out-Null }

# Replaces the cache in IMAGE_CACHE with the one the build wrote, if it wrote one, so the cache
# holds only what the last build used.
[unix]
_image-cache:
    if [ -n "${IMAGE_CACHE:-}" ] && [ -d "$IMAGE_CACHE-new" ]; then rm -rf "$IMAGE_CACHE" && mv "$IMAGE_CACHE-new" "$IMAGE_CACHE"; fi

# Replaces the cache in IMAGE_CACHE with the one the build wrote, if it wrote one, so the cache
# holds only what the last build used.
[windows]
_image-cache:
    if ('{{ image_cache }}' -and (Test-Path '{{ image_cache }}-new')) { Remove-Item -Recurse -Force '{{ image_cache }}' -ErrorAction SilentlyContinue; Move-Item '{{ image_cache }}-new' '{{ image_cache }}' }

# Fails on a vulnerability that has a fix and is rated high or critical, in each platform's image
# of each image in target (SEC-15). The scanner reads the operating system's packages and the
# crates the binary records.
[doc("Scans the images in target/ for vulnerabilities that have a fix")]
[group('image')]
image-scan:
    docker compose run --rm grype oci-dir:/image --platform linux/amd64 --only-fixed --fail-on high
    docker compose run --rm grype oci-dir:/image --platform linux/arm64 --only-fixed --fail-on high
    docker compose run --rm grype oci-dir:/image-postgres --platform linux/amd64 --only-fixed --fail-on high
    docker compose run --rm grype oci-dir:/image-postgres --platform linux/arm64 --only-fixed --fail-on high

# Runs the end-to-end tests against the images in target, on the platform E2E_PLATFORM, which is
# this machine's unless the environment names another (TEST-03). The tests push the images to the
# registry that Compose runs, which they empty first, and run them from there, against the Postgres
# that `just services` starts (crates/app/tests/e2e/machine.rs). Every other image
# the tests pull is fetched first: this machine's, and, through the mirror, their servers', which
# run on this machine's platform, so no test reaches a registry outside (TEST-06). The arguments go
# to the test harness: names run only the tests whose names hold one of them, such as
# `just e2e obs_13`, and `--skip` leaves out those whose names hold the name after it, as CI's
# jobs do. A name with a slash is refused: it would be a platform, which matches no test, so the
# run would pass with none.
[doc("Runs the end-to-end tests; `just e2e obs_13` runs only those whose names contain obs_13")]
[group('image')]
e2e *tests: services
    {{ if tests =~ '/' { error("just e2e takes test names and the test harness's options; E2E_PLATFORM names the platform, such as E2E_PLATFORM=linux/arm64") } else { "" } }}docker compose up --detach --wait registry mirror
    docker compose pull --quiet server
    docker compose --profile observability pull --quiet collector prometheus loki tempo
    {{ xtask }} mirror {{ native_platform }}
    cargo test --locked --package app --features e2e --test e2e -- {{ tests }}

# Measures one replica's capacity, for the release notes (OPS-16): runs the API's image in
# target/image, on this machine's platform, with the reference deployment's limits on CPU and
# memory for a replica, and sends it requests at rising constant rates through k6, from the image
# that compose.yaml pins. It prints each step, then the highest rate served with none failed and
# the 99th percentile within the latency objective, for GET /v1/capabilities, or with `me`, for
# GET /v1/users/me as a signed-in user. docs/operations/release.md says when to run it.
# `just load-test streams` measures the replica's hub instead, with 1,000 streams open on it. # part: invitations
[doc("Measures one replica's capacity, for the release notes")]
[group('image')]
load-test route="capabilities": services
    docker compose --profile observability up --detach --wait collector prometheus loki tempo
    docker compose up --detach --wait registry
    docker compose run --rm crane push --insecure /image registry:5000/{{ e2e_image }}:e2e
    {{ xtask }} load-test {{ route }}
