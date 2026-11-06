# AntWorker API

A Rust API for workspace-based products, built as a template to fork. It is built module by module: [docs/spec/](docs/spec/README.md) states what each module guarantees, and [docs/MIGRATION.md](docs/MIGRATION.md) tracks what exists so far.

## Quickstart

Requires Rust (through rustup), [just](https://github.com/casey/just), Docker, [cargo-deny](https://github.com/EmbarkStudios/cargo-deny), and [watchexec](https://github.com/watchexec/watchexec) for `just dev`. `just setup` checks the versions the justfile pins.

```
just              # lists the tasks, by group, each with what it does; those only CI runs are left out
just setup        # checks the tools, and writes .env from .env.example, with generated passwords
just dev          # starts Postgres, migrates, and runs the API, restarting it when the code changes
just dev observe  # the same, with the observability stack (docs/operations/observability.md)
just seed         # fills the development database with demo accounts and workspaces, and prints how to sign in
just ci           # runs every check CI runs, building the image too
just release      # shows the versions that can come next; `just release 1.0.0` releases main as 1.0.0 (docs/operations/release.md)
```

While `just dev` runs, http://127.0.0.1:8080/docs shows the API reference of the running code, from which a browser can call the API, signed in as an account that `just seed` created, and http://127.0.0.1:8025 shows every email the API sent. `just seed` fills a database once. To fill it again, recreate the database with the commands that [the guide to adding a module](docs/guides/adding-a-module.md#migrations-and-the-query-cache) gives for editing a migration.

## Documentation

- [docs/architecture.md](docs/architecture.md): how the API is built.
- [docs/standards/](docs/standards/README.md): the rules every change follows.
- [docs/spec/](docs/spec/README.md): what each module guarantees.
- [docs/guides/adding-a-module.md](docs/guides/adding-a-module.md): adding a module with `just new-module`, and adding a route to one.
- [docs/operations/observability.md](docs/operations/observability.md): the logs, traces, metrics, dashboards and alerts, and how to follow a request from its trace ID.
- [docs/operations/ci.md](docs/operations/ci.md) and [docs/operations/release.md](docs/operations/release.md): the pipeline, the image, and releases.
