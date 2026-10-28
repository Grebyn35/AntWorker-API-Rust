#!/bin/sh
# Imports a release's dashboards and alert rules into a managed Grafana (D42), as a deploy with
# TELEMETRY=managed does once its replicas serve. The dashboards go into the folder `API`, which
# Viewers and Editors may only view (OBS-13), each naming the managed Grafana's data source where
# it names one the self-hosted Grafana provisions. The rules go into the folder's subfolder
# `Alerts`, through Grafana's conversion of Prometheus rules, as rules that Grafana evaluates and
# that no one changes in Grafana (OBS-14). The dashboards and the rule groups that an earlier
# import made and the release lacks are deleted. It warns while the managed Grafana's
# notification policies send the alerts to no contact point.
#
#   DEPLOYMENT=<directory> MANAGED_GRAFANA_URL=<https URL> CURL=<image> YQ=<image> import-grafana.sh
#
# DEPLOYMENT names the deployment's directory, whose observability/ holds the release's
# dashboards and rules, whose secrets/managed_grafana_token holds the token of a service account
# with the Admin role, and whose certs/managed-grafana-ca.crt, if any, holds the authority that
# signed the managed Grafana's certificate. CURL and YQ name the images of the tools, from
# tools/compose.yaml.
set -eu

deployment=${DEPLOYMENT:?DEPLOYMENT names the deployment directory}
url=${MANAGED_GRAFANA_URL:?MANAGED_GRAFANA_URL names the managed Grafana}
url=${url%/}
token=$deployment/secrets/managed_grafana_token

# Grafana Cloud's data sources, by their uids, that stand for the ones the self-hosted Grafana
# provisions as prometheus, loki and tempo.
prometheus=grafanacloud-prom
loki=grafanacloud-logs
tempo=grafanacloud-traces
# The folder's uid and its title, and the title of its subfolder for the rules.
folder=api
title=API
alerts=Alerts
# The label that marks a dashboard this import made, so a later import finds the ones its release
# lacks.
label=app.kubernetes.io/managed-by=deploy

say() { printf '%s\n' "deploy: $*"; }
fail() {
    printf '%s\n' "deploy: $*" >&2
    exit 1
}

trust=""
if [ -f "$deployment/certs/managed-grafana-ca.crt" ]; then trust="--cacert /run/certs/managed-grafana-ca.crt"; fi

# Sends the request $1 for the path $2 to the managed Grafana as the import's service account,
# with any further curl options, and the standard input as its body for PUT and POST. Writes the
# answer to standard output, or, when Grafana refuses, to standard error, and fails.
grafana() {
    method=$1 path=$2
    shift 2
    body="" input=""
    if [ "$method" = PUT ] || [ "$method" = POST ]; then body="--data-binary @-" input=--interactive; fi
    # shellcheck disable=SC2086 # $input, $trust and $body are options and their values, or nothing.
    if ! answer=$(docker run --rm --quiet $input --volume "$token:/run/token:ro" \
        --volume "$deployment/certs:/run/certs:ro" "$CURL" \
        --silent --show-error --fail-with-body $trust \
        --variable token@/run/token --expand-header 'Authorization: Bearer {{token:trim}}' \
        --header 'Content-Type: application/json' --request "$method" $body "$@" "$url$path"); then
        printf '%s\n' "$answer" >&2
        fail "the managed Grafana refused $method $path"
    fi
    printf '%s\n' "$answer"
}

# Runs yq on the standard input, or on the release's files, which it sees under /release, with
# the variables this script exports for yq's strenv().
yq() {
    docker run --rm --quiet --interactive \
        --env MAPPED --env PRESENT --env FOLDER --env LABEL --env ALERTS --env NAME \
        --volume "$deployment/observability/grafana/dashboards:/release/dashboards:ro" \
        --volume "$deployment/observability/prometheus/rules.yaml:/release/rules.yaml:ro" \
        "$YQ" "$@"
}

# A Grafana Cloud instance that slept answers 503 while it loads again, so the import first waits
# up to three minutes for another answer to a request with its token, through curl's retries of a
# 503 and its other transient answers. Each failed answer's body would precede the next one's in
# curl's output, so this request's output is discarded, and the requests that follow, which
# report what Grafana answers, don't retry.
# shellcheck disable=SC2086 # $trust is an option and its value, or nothing.
docker run --rm --quiet --volume "$token:/run/token:ro" --volume "$deployment/certs:/run/certs:ro" \
    "$CURL" --silent --fail --retry 8 --retry-max-time 180 --output /dev/null $trust \
    --variable token@/run/token --expand-header 'Authorization: Bearer {{token:trim}}' \
    "$url/api/folders" > /dev/null 2>&1 || true

# Grafana Cloud keeps a stack's resources in the namespace stacks-<its instance's ID>, and
# Grafana itself, those of its first organization in `default`; /bootdata says which.
bootdata=$(grafana GET /bootdata)
namespace=$(printf '%s\n' "$bootdata" | yq --input-format json --output-format yaml '.settings.namespace')
sources=$(grafana GET /api/datasources)
PRESENT=$(printf '%s\n' "$sources" | yq --input-format json --output-format yaml '[.[].uid] | join(" ")')
MAPPED="{prometheus: $prometheus, loki: $loki, tempo: $tempo}"
FOLDER=$folder LABEL=$label ALERTS=$alerts NAME=""
export PRESENT MAPPED FOLDER LABEL ALERTS NAME

say "importing the dashboards into the managed Grafana's folder $title"
printf '{"apiVersion": "folder.grafana.app/v1", "kind": "Folder", "metadata": {"name": "%s"}, "spec": {"title": "%s"}}' "$folder" "$title" |
    grafana PUT "/apis/folder.grafana.app/v1/namespaces/$namespace/folders/$folder" > /dev/null
for role in Viewer Editor; do
    echo '{"permission": "View"}' | grafana POST "/api/access-control/folders/$folder/builtInRoles/$role" > /dev/null
done

# Each dashboard as one line: its uid, then the object the dashboard API takes. Its file names
# hold no spaces, as check-docs' format checks require.
files=$(cd "$deployment/observability/grafana/dashboards" && find . -name '*.json' | sed 's|^\./|/release/dashboards/|')
# shellcheck disable=SC2016,SC2086 # yq expands its own variables, and $files holds one file per word.
dashboards=$(yq --input-format json --output-format yaml --no-doc --unwrapScalar '
    (strenv(MAPPED) | from_yaml) as $mapped
    | (strenv(PRESENT) | split(" ")) as $present
    | with(.. | select(tag == "!!map" and has("datasource")) | .datasource;
        .uid = ($mapped[.uid] // error(filename + " names the data source " + .uid + ", which has no counterpart in the managed Grafana")))
    | ([.. | select(tag == "!!map" and has("datasource")) | .datasource.uid] - $present) as $missing
    | with(select($missing | length > 0); error("the managed Grafana has no data source " + ($missing | join(", ")) + ", which " + filename + " names"))
    | (strenv(LABEL) | split("=")) as $label
    | .uid + " " + ({"apiVersion": "dashboard.grafana.app/v1", "kind": "Dashboard",
        "metadata": {"name": .uid, "labels": {$label[0]: $label[1]}, "annotations": {"grafana.app/folder": strenv(FOLDER)}},
        "spec": .} | to_json(0))' $files)
printf '%s\n' "$dashboards" | while IFS= read -r line; do
    printf '%s' "${line#* }" | grafana PUT "/apis/dashboard.grafana.app/v1/namespaces/$namespace/dashboards/${line%% *}" > /dev/null
done
imported=$(printf '%s\n' "$dashboards" | sed 's/ .*//')
listed=$(grafana GET "/apis/dashboard.grafana.app/v1/namespaces/$namespace/dashboards" --get --data-urlencode "labelSelector=$label")
printf '%s\n' "$listed" | yq --input-format json --output-format yaml '.items[].metadata.name' | while IFS= read -r name; do
    if ! printf '%s\n' "$imported" | grep -qx -- "$name"; then
        say "deleting the dashboard $name, which this release lacks"
        grafana DELETE "/apis/dashboard.grafana.app/v1/namespaces/$namespace/dashboards/$name" > /dev/null
    fi
done

say "importing the alert rules into the managed Grafana's folder $title/$alerts"
rules=/api/convert/prometheus/config/v1/rules/$alerts
yq --output-format json --indent 0 '.groups[]' /release/rules.yaml | while IFS= read -r group; do
    printf '%s' "$group" | grafana POST "$rules" --header "X-Grafana-Alerting-Folder-UID: $folder" \
        --header "X-Grafana-Alerting-Datasource-UID: $prometheus" > /dev/null
done
names=$(yq '.groups[].name' /release/rules.yaml)
loaded=$(grafana GET /api/convert/prometheus/config/v1/rules --header "X-Grafana-Alerting-Folder-UID: $folder" \
    --header 'Accept: application/json')
# shellcheck disable=SC2016 # yq expands its own variables.
printf '%s\n' "$loaded" | yq --input-format json --output-format yaml '.[strenv(ALERTS)] // [] | .[].name' |
    while IFS= read -r name; do
        if ! printf '%s\n' "$names" | grep -qx -- "$name"; then
            say "deleting the rule group $name, which this release lacks"
            NAME=$name
            grafana DELETE "$rules/$(yq --null-input 'strenv(NAME) | @uri')" \
                --header "X-Grafana-Alerting-Folder-UID: $folder" > /dev/null
        fi
    done

# A deployment may run without a contact point, but not without knowing.
policies=$(grafana GET /api/v1/provisioning/policies)
points=$(grafana GET /api/v1/provisioning/contact-points)
# shellcheck disable=SC2016 # yq expands its own variables.
reached=$(printf '%s\n%s\n' "$policies" "$points" | yq eval-all --input-format json --output-format yaml '
    [select(document_index == 0) | .. | select(tag == "!!map" and has("receiver")) | .receiver] as $named
    | [select(document_index == 1) | .[].name] as $points
    | ($named - ($named - $points)) | length > 0')
if [ "$reached" != true ]; then
    echo "deploy: warning: the managed Grafana's notification policies send the alerts to no contact point, so they notify no one; docs/operations/observability.md#a-managed-backend says how to add one" >&2
fi
say "imported $(printf '%s\n' "$imported" | wc -l | tr -d ' ') dashboards and $(printf '%s\n' "$names" | wc -l | tr -d ' ') rule groups into the managed Grafana"
