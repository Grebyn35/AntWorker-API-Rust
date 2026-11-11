# The tasks on the reference deployment's server, each run from a developer's machine over SSH
# (OPS-19). The justfile imports this file.

# Deploys a release to the reference deployment on `host`, an SSH destination such as a Host of
# ~/.ssh/config, `production`, or deploy@203.0.113.7 (OPS-14): the latest release, or the one whose
# version follows the host, such as `just deploy production 1.4.0`. It refuses a version that no
# release has, copies the deployment's files from the release's tag to the server, and runs that
# release's deploy/deploy.sh there, which verifies the release's images before it pulls them, and
# removes the other releases' files and the images that nothing uses once its replicas serve.
# `--allow-restart` lets it restart a service that runs once, such as Postgres or Caddy.
# docs/operations/deploy.md describes a deploy.
[doc("Deploys the latest release to a server, as `just deploy production`; `just deploy production 1.4.0` deploys 1.4.0")]
[group('server')]
deploy host *arguments:
    {{ xtask }} deploy {{ host }} {{ arguments }}

# Takes a full base backup of the bundled database of the reference deployment on `host` (OPS-15).
[doc("Takes a full backup of a server's bundled database")]
[group('server')]
backup host:
    {{ xtask }} backup {{ host }}

# Restores the bundled database of the reference deployment on `host`, as it stood at `moment`, a
# time with its offset from UTC as Postgres writes it, or at the latest moment its backups hold
# (OPS-15). `--allow-unarchived` lets it restore although the database's log doesn't reach the
# bucket, as `just restore production --allow-unarchived`.
# docs/operations/backups.md describes a restore.
[doc("Restores a server's bundled database, as it stood at a moment")]
[group('server')]
restore host moment="" *options:
    {{ xtask }} restore {{ host }} "{{ moment }}" {{ options }}

# Sets up the server that SSH reaches at `host`, as root or a user that may run sudo without a
# password, for the reference deployment of the release `version`, or of the latest release
# (OPS-19): asks for the settings and the secrets that the settings' choices need, then installs
# Docker on Debian and Ubuntu, prepares the user the deployment runs as, logs it in to the
# registry, and writes what it asked. Run again, it changes them. docs/operations/deploy.md
# describes it.
[doc("Sets up a server for the deployment, asking for its settings")]
[group('server')]
server-setup host version="":
    {{ xtask }} server-setup {{ host }} {{ version }}

# Checks what a deploy of the release `version`, or of the latest release, to the reference
# deployment on `host` needs, and changes nothing of the deployment (OPS-19): on the server, the
# settings, the secrets, Docker, the release, the memory, the bucket and a managed backend; from
# here, where its addresses point and whether its ports 80 and 443 answer.
[doc("Checks what a deploy to a server needs, and changes nothing")]
[group('server')]
server-check host version="":
    {{ xtask }} server-check {{ host }} {{ version }}

# Shows the state of the reference deployment on `host` (OPS-19): which color serves, each of its
# containers with its state and its image's version, when the backups last succeeded, the data
# that each restore moved aside, and the server's free disk and memory. It changes nothing.
# docs/operations/deploy.md describes it.
[doc("Shows what a server runs, its backups, and its free disk and memory")]
[group('server')]
status host:
    {{ xtask }} status {{ host }}

# Shows the last 100 lines of the logs of the reference deployment's `services` on `host`, `api`
# for both colors' replicas, or of every service without one (OPS-19). `--follow` goes on showing
# the lines that follow, until Ctrl-C.
[doc("Shows the last log lines of a server's services")]
[group('server')]
logs host *services:
    {{ xtask }} logs {{ host }} {{ services }}

# Removes the authenticator app and the recovery codes of the account that holds `email` on the
# reference deployment on `host`, ends every session of the account, and tells the address, for a
# user who lost both, once you have checked, your own way, that the person who asks owns the
# account (AUTH-072, D135). It runs the API's subcommand `remove-second-factor` with the image of
# the color that serves, and exits 1, changing nothing, when no account holds the address or it has
# no app. docs/operations/deploy.md says when to run it.
[doc("Removes a lost authenticator app from an account on a server")]
[group('server')]
remove-second-factor host email:
    {{ xtask }} remove-second-factor {{ host }} {{ quote(email) }}
