# The operator's tasks on the deployment's accounts, each a subcommand of the API that runs with
# the image of the color that serves, in a service of compose.yaml of its own, which passes it only
# the settings it reads (ARCH-14, OPS-02).

# Removes the authenticator app and the recovery codes of the account that holds the address $1,
# ends every session of the account, and tells the address, through the API's subcommand
# remove-second-factor, which exits 1, changing nothing, when no account holds the address or it
# has no app (AUTH-072, D135). It takes no lock: it changes only the database's rows, in one
# transaction, as a request does, so it runs beside a deploy or a backup.
remove_second_factor() {
    if [ ! -f "$state/deploy.env" ]; then fail "no deploy has run on this server, so no account exists"; fi
    read_settings
    color=$(live_color)
    if [ -z "$color" ]; then fail "no replica runs, so the API's image can't be run; deploy first"; fi
    # A one-off container of the service of that color, which goes once the subcommand ends; its
    # arguments replace the service's command, so they name the subcommand again.
    compose --profile tasks run --rm --no-deps -T "remove-second-factor-$color" remove-second-factor "$1"
}
