# The operator's tasks on the deployment's accounts, each a subcommand of the API that runs with
# the image of the color that serves, in a service of compose.yaml of its own, which passes it only
# the settings it reads (ARCH-14, OPS-02).

# Runs the API's subcommand $1, which an operator runs against the accounts, with the arguments
# that follow, in a one-off container of its service of the color that serves, which goes once
# the subcommand ends; the arguments replace the service's command, so they name the subcommand
# again. It takes no lock of a deploy's: the subcommand changes only the database's rows, in one
# transaction, as a request does, so it runs beside a deploy or a backup. It takes the
# subcommands' own, state/operating, and refuses while another subcommand holds it, since a
# deploy's budget of connections counts one for them (OPS-18); the kernel releases the lock once
# the task's last process ends.
operate() {
    subcommand=$1
    shift
    mkdir -p "$state"
    exec 7>> "$state/operating"
    if ! flock -n 7; then fail "another of the operator's subcommands is running"; fi
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

# Ends every session of the account that holds the address $1, or, when $1 is --all, of every
# account, through the API's subcommand end-sessions, which exits 1, changing nothing, when no
# account holds the address, or no session is there to end (AUTH-094).
end_sessions() {
    operate end-sessions "$1"
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

# part: superadmins
# Makes the account that holds the address $2 a superadmin of the deployment, with add as $1, or
# ends its being one, with remove, through the API's subcommand superadmin, which exits 1,
# changing nothing, when no account holds the address, when, with email on, the account hasn't
# proved it, or when the account is one already, or is none to end (SUPERADMINS-001 to
# SUPERADMINS-003, D62). With list as $1 alone, the subcommand shows the deployment's
# superadmins, each with the address of their account (SUPERADMINS-008).
superadmin() {
    operate superadmin "$@"
}
# end part: superadmins
