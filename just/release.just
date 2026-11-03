# The tasks of releases: `just release`, and the steps CI runs to publish, release, verify and
# rescan the images. The justfile imports this file.

# Pushes each image in target to the registry as sha-<commit>, under the name of `repository`,
# such as owner/name, or that name with `-postgres` for the Postgres image, and moves `edge` to it
# while the commit is still the head of main (OPS-14). CI runs it on main, once every check has
# passed, after a `docker login` to the registry.
[unix]
[private]
publish repository commit:
    #!/usr/bin/env bash
    set -euo pipefail
    repository={{ image_registry }}/{{ lowercase(repository) }}
    crane() { docker compose run --rm --user "$(id -u):$(id -g)" --volume "$HOME/.docker:/docker:ro" --env DOCKER_CONFIG=/docker crane "$@"; }
    for suffix in "" -postgres; do
        tested=$(grep -o '"digest":"sha256:[0-9a-f]*"' "{{ image_dir }}$suffix/index.json" | head -n 1 | cut -d '"' -f 4)
        # The registry can refuse a manifest moments after the first one created a new package,
        # so a failed push is tried again. Pushing again pushes only what is missing.
        for attempt in 1 2 3 4 5; do
            if crane push "/image$suffix" "$repository$suffix:sha-{{ commit }}"; then break; fi
            if [ "$attempt" = 5 ]; then exit 1; fi
            echo "the push failed; pushing again in 15 seconds" >&2
            sleep 15
        done
        pushed=$(crane digest "$repository$suffix:sha-{{ commit }}")
        if [ "$pushed" != "$tested" ]; then echo "the registry holds $pushed, not the tested $tested" >&2; exit 1; fi
    done
    if [ "$(git ls-remote origin refs/heads/main | cut -f 1)" = "{{ commit }}" ]; then
        for suffix in "" -postgres; do crane tag "$repository$suffix:sha-{{ commit }}" edge; done
    else
        echo "{{ commit }} is no longer the head of main, so edge stays where it is"
    fi

# Releases main as `version`, X.Y.Z, or X.Y.Z-rc.N for a prerelease (OPS-14): checks that main can
# release it, sets the app's version to it, commits, and pushes main, with its commits that
# GitHub's lacks. The run on main that passes with that version releases it. Without a version,
# it shows the latest release, what has changed since, and the versions that can come next, and
# changes nothing. docs/operations/release.md describes a release.
[doc("Releases main as a version; without one, shows the next versions")]
[group('release')]
release version="":
    {{ xtask }} release {{ version }}

# Releases the app's version from the images this run on main published for `commit`, if a commit
# since the latest release asks for it and no release has it yet (OPS-14): gives each image the
# version's tag, signs it, verifies the release, and only then tags the commit v<version>. The
# request is the commit `just release` makes, whose subject is "Release <version>". `repository`
# is the GitHub repository, such as owner/name, and `source` its address. CI runs it on main once
# every check has passed, after a `docker login`, and each failure says what to do next.
[unix]
[private]
release-image repository commit source:
    #!/usr/bin/env bash
    set -euo pipefail
    image={{ image_registry }}/{{ lowercase(repository) }}
    crane() { docker compose run --rm --user "$(id -u):$(id -g)" --volume "$HOME/.docker:/docker:ro" --env DOCKER_CONFIG=/docker crane "$@"; }
    # cosign keeps Sigstore's trust data in its home, which the runner's user can write only in /tmp.
    cosign() { docker compose run --rm --user "$(id -u):$(id -g)" --env HOME=/tmp --volume "$HOME/.docker:/docker:ro" --env DOCKER_CONFIG=/docker --env ACTIONS_ID_TOKEN_REQUEST_URL --env ACTIONS_ID_TOKEN_REQUEST_TOKEN cosign "$@"; }
    # An error annotation, which GitHub shows on the run's page.
    fail() { echo "::error::$1" >&2; exit 1; }
    version={{ version }}
    git fetch --quiet --tags origin
    if [ -n "$(git tag --list "v$version")" ]; then
        echo "v$version is released already, so this commit is published but not released. To release it, run \`just release\` with a new version."
        exit 0
    fi
    if latest=$(git describe --tags --abbrev=0 --match 'v[0-9]*' "{{ commit }}" 2> /dev/null); then since="$latest..{{ commit }}"; else since="{{ commit }}"; fi
    # Captured first: grep stops reading at the first match, which would fail git log's write. The
    # request matches as `asks_for` in crates/xtask/src/release/mod.rs does: its subject alone, or with
    # the number a squash merge appends.
    subjects=$(git log --format=%s "$since")
    if ! grep --quiet --extended-regexp --line-regexp "Release ${version//./\\.}( \(#[0-9]+\))?" <<< "$subjects"; then
        echo "No commit since the latest release asks for the release of $version, so this commit is published but not released. To release it, run \`just release\`."
        exit 0
    fi
    if ! refused=$({{ xtask }} release-check "$version" 2>&1); then fail "$refused"; fi
    released=""
    for suffix in "" -postgres; do
        digest=$(crane digest "$image$suffix:sha-{{ commit }}") \
            || fail "This run's publish job pushed no image of {{ commit }} to $image$suffix, so $version isn't released. Re-run this run's failed jobs."
        crane tag "$image$suffix@$digest" "$version" \
            || fail "The registry refused the tag $version of $image$suffix, so $version isn't released. Re-run this job."
        cosign sign --yes "$image$suffix@$digest" \
            || fail "Signing failed, so $version isn't released. Re-run this job; if it fails again, Sigstore may be down: https://status.sigstore.dev."
        released+="$image$suffix@$digest"$'\n'
    done
    verified=$(just verify-release {{ repository }} "$version" {{ source }}) \
        || fail "The release of $version doesn't verify, so it isn't released; the error above says why. Re-run this job."
    if [ "$verified"$'\n' != "$released" ]; then fail "The release of $version names other images than the tested ones, so it isn't released: $verified. Re-run this job."; fi
    { git tag "v$version" "{{ commit }}" && git push --quiet origin "v$version"; } \
        || fail "The images of $version are released, but GitHub refused the tag v$version. Re-run this job."
    echo "Released $version:"
    echo "$released"

# Fails unless each image of `repository`, such as owner/name, tagged `version` is signed by the
# run on main of the repository at `source`, with the SBOM and the provenance of both platforms,
# and prints each one's digest (OPS-14). It runs deploy/verify-release.sh, which the reference
# deployment's deploy also runs before it pulls a release (OPS-16).
[unix]
[doc("Verifies a release's images, signatures, SBOMs and provenance")]
[group('release')]
verify-release repository version source:
    CRANE="{{ crane_image }}" COSIGN="{{ cosign_image }}" deploy/verify-release.sh "{{ version }}" "{{ source }}" \
        "{{ image_registry }}/{{ lowercase(repository) }}" "{{ image_registry }}/{{ lowercase(repository) }}-postgres"

# Scans the published images of `repository`, such as owner/name, again: `edge`, the newest final
# release and the newest prerelease of each image, for vulnerabilities published since they were
# tested (SEC-13). The scheduled workflow runs it every week, after a `docker login`.
[unix]
[private]
image-rescan repository:
    #!/usr/bin/env bash
    set -euo pipefail
    # crane runs as the runner's user, who alone can read the registry login. Grype runs as its
    # image's root, which can read it too, and write the database to the volume Compose made.
    crane() { docker compose run --rm --user "$(id -u):$(id -g)" --volume "$HOME/.docker:/docker:ro" --env DOCKER_CONFIG=/docker crane "$@"; }
    grype() { docker compose run --rm --volume "$HOME/.docker:/docker:ro" --env DOCKER_CONFIG=/docker grype "$@"; }
    for suffix in "" -postgres; do
        repository={{ image_registry }}/{{ lowercase(repository) }}$suffix
        tags=$(crane ls "$repository")
        release=$(grep -E '^[0-9]+\.[0-9]+\.[0-9]+$' <<< "$tags" | sort -V | tail -n 1 || true)
        prerelease=$(grep -E '^[0-9]+\.[0-9]+\.[0-9]+-rc\.[0-9]+$' <<< "$tags" | sort -V | tail -n 1 || true)
        for tag in edge $release $prerelease; do
            for platform in linux/amd64 linux/arm64; do
                grype "registry:$repository:$tag" --platform "$platform" --only-fixed --fail-on high
            done
        done
    done
