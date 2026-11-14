# The operator's tasks on the deployment's accounts, each a subcommand of the API that runs with
# the image of the color that serves, in a service of compose.yaml of its own, which passes it only
# the settings it reads (ARCH-14, OPS-02).

# Runs the API's subcommand $1, which an operator runs against the accounts, with the arguments
# that follow, in a one-off container of its service of the color that serves, which goes once
# the subcommand ends; the arguments replace the service's command, so they name the subcommand
# again. It takes no lock: the subcommand changes only the database's rows, in one transaction, as
# a request does, so it runs beside a deploy or a backup.
operate() {
    subcommand=$1
    shift
    if [ ! -f "$state/deploy.env" ]; then fail "no deploy has run on this server, so no account exists"; fi
    read_settings
    color=$(live_color)
    if [ -z "$color" ]; then fail "no replica runs, so the API's image can't be run; deploy first"; fi
    compose --profile tasks run --rm --no-deps -T "$subcommand-$color" "$subcommand" "$@"
}

# Removes the authenticator app and the recovery codes of the account that holds the address $1,
# ends every session of the account, and tells the address, through the API's subcommand
# remove-second-factor, which exits 1, changing nothing, when no account holds the address or it
# has no app (AUTH-072, D135).
remove_second_factor() {
    operate remove-second-factor "$1"
}

# part: workspace_setup
# Creates the workspace named $1, owned by the account that holds the address $2, through the
# API's subcommand setup-workspace, which, with email on, creates that account, named $3, when no
# account holds the address, and tells the address. It exits 1, changing nothing, when an earlier
# run was given the name, or the account may not own the workspace (WORKSPACES-023 to
# WORKSPACES-028, D65).
setup_workspace() {
    operate setup-workspace "$@"
}
# end part: workspace_setup
