#!/bin/sh
# Runs the reference deployment's scanner (the files spec's "Scanning"): freshclam, which keeps
# ClamAV's signatures in /var/lib/clamav up to date, beside clamd, which scans with them. A
# volume that holds no signatures, as on a new server, gets them first, since clamd can't start
# without them. clamd ending ends the container, which Docker starts again; a check of
# freshclam's that fails waits for its next, and signatures that grow old raise an alert.
set -eu

# A stop signal stops what runs, a download of the signatures included, and ends the script.
updates=""
scans=""
trap 'kill $updates $scans 2> /dev/null || true; exit 143' TERM INT

signatures=/var/lib/clamav
configuration=/etc/scanner

# The collector reads when the signatures last changed, as a user of its own (OBS-14).
chmod 755 "$signatures"

has_signatures() {
    find "$signatures" -maxdepth 1 -type f \( -name '*.cvd' -o -name '*.cld' -o -name '*.cud' -o -name '*.ndb' -o -name '*.hdb' -o -name '*.ldb' \) | grep -q .
}

# Without signatures, downloads them, and tries again every minute until it can: a mirror that
# refuses a check, as ClamAV's do for a server that checks too often, refuses it for a while.
until has_signatures; do
    if freshclam --config-file="$configuration/freshclam.conf" --stdout; then break; fi
    echo "scanner: no signatures yet, so clamd can't start; freshclam tries again in a minute" >&2
    sleep 60
done

# freshclam checks at once, then 12 times a day, and has clamd load what it downloads.
freshclam --config-file="$configuration/freshclam.conf" --stdout --daemon \
    --daemon-notify="$configuration/clamd.conf" &
updates=$!
clamd --config-file="$configuration/clamd.conf" &
scans=$!

# Waits for clamd, and once it ends, stops freshclam.
status=0
wait "$scans" || status=$?
kill "$updates" "$scans" 2> /dev/null || true
wait || true
exit "$status"
