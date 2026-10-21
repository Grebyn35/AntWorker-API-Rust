# AntWorker API

A Rust API for workspace-based products, built as a template to fork. It is built module by module: [docs/spec/](docs/spec/README.md) states what each module guarantees, and [docs/MIGRATION.md](docs/MIGRATION.md) tracks what exists so far.

## Quickstart

Requires Rust (through rustup), [just](https://github.com/casey/just) and Docker.

```
just setup   # writes .env from .env.example, with generated passwords
just run     # starts Postgres, creates the database roles, migrates, and runs the API
just ci      # runs every check CI runs
```

## Documentation

- [docs/architecture.md](docs/architecture.md): how the API is built.
- [docs/standards/](docs/standards/README.md): the rules every change follows.
- [docs/spec/](docs/spec/README.md): what each module guarantees.
