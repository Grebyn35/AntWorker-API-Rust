# Working in this repository

This is a Rust API built as a template to fork. Read these before changing anything:

1. [docs/architecture.md](docs/architecture.md): how the API is built, and the glossary.
2. [docs/standards/README.md](docs/standards/README.md): how rules work, and how reviews run.
3. The rules in [docs/standards/](docs/standards/) that your change touches.
4. The spec in [docs/spec/](docs/spec/README.md) for the module you are changing.

[docs/MIGRATION.md](docs/MIGRATION.md) tracks what is built and what is decided.

## Rules for every change

- **Spec first.** A change to behavior updates the module's spec before or with the code.
- **Docs for unbuilt parts are the plan.** Until a part is built, its spec and its sections of `docs/architecture.md` describe the intended design, not code. The plan states what a part must do and guarantee; the tools and libraries it uses are chosen when its spec is written, just before it is built. When building shows the plan is wrong, correct the plan in the same change as the code. A rule changes through "Changing a rule" in the standards README. A behavior is proven only when a test named by its ID passes.
- **Tests prove IDs.** Every test name starts with the behavior ID or rule ID it proves, such as `auth_004_` or `ops_03_` (TEST-01).
- **Findings cite IDs.** A review finding cites a rule ID or a behavior ID. Anything else is a proposal for a new rule, and the owner decides (standards README, "Changing a rule").
- **No open-ended evaluations.** Reviews follow the procedure and the stopping rule in the standards README.
- **Docs and comments describe the current state only** (DOC-01 to DOC-04). Edits rewrite in place. History belongs in git, ADRs and `CHANGELOG.md`.
- **No `TODO` or `FIXME`.** Known work is tracked as an issue (DOC-07).
- **`docs/inventory/` describes the old API.** Use it to look up old behavior, and never cite it outside the migration workspace.

## Commands

```
just setup     # writes .env, with generated passwords
just ci        # every check CI runs
just test      # the tests; they start Postgres through Compose
just run       # the API with the settings in .env, after bootstrap and migrate
just migrate   # applies new migrations
just prepare   # rewrites the query cache in .sqlx/ (DATA-20)
```

Queries are checked against the database while compiling, and `migrate` is part of the same binary. Add a migration and run `just migrate` before writing the queries that use it, then run `just prepare` before `just ci`.
