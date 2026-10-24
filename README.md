# AntWorker API

A Rust API for workspace-based products, built as a template to fork. It is built module by module: [docs/spec/](docs/spec/README.md) states what each module guarantees, and [docs/MIGRATION.md](docs/MIGRATION.md) tracks what exists so far.

## Quickstart

Requires Rust (through rustup), [just](https://github.com/casey/just), Docker, and [watchexec](https://github.com/watchexec/watchexec) for `just dev`. `just setup` checks the versions the justfile pins.

```
just setup        # checks the tools, and writes .env from .env.example, with generated passwords
just dev          # starts Postgres, migrates, and runs the API, restarting it when the code changes
just dev observe  # the same, with the observability stack; Grafana answers on http://127.0.0.1:3000
just ci           # runs every check CI runs
```

## Documentation

- [docs/architecture.md](docs/architecture.md): how the API is built.
- [docs/standards/](docs/standards/README.md): the rules every change follows.
- [docs/spec/](docs/spec/README.md): what each module guarantees.
- [docs/operations/observability.md](docs/operations/observability.md): the logs, traces, metrics, dashboards and alerts, and how to follow a request from its trace ID.
