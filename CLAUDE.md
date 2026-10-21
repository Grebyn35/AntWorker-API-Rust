# Working in this repository

This is a Rust API built as a template to fork. Read these before changing anything:

1. [docs/architecture.md](docs/architecture.md): how the API is built, and the glossary.
2. [docs/standards/README.md](docs/standards/README.md): how rules work, and how reviews run.
3. The rules in [docs/standards/](docs/standards/) that your change touches.
4. The spec in [docs/spec/](docs/spec/README.md) for the module you are changing.

[docs/MIGRATION.md](docs/MIGRATION.md) tracks what is built and what is decided.

## Rules for every change

- **Spec first.** A change to behavior updates the module's spec before or with the code.
- **Tests prove IDs.** Every test name starts with the behavior ID or rule ID it proves, such as `auth_004_` or `ops_03_` (TEST-01).
- **Findings cite IDs.** A review finding cites a rule ID or a behavior ID. Anything else is a proposal for a new rule, and the owner decides (standards README, "Changing a rule").
- **No open-ended evaluations.** Reviews follow the procedure and the stopping rule in the standards README.
- **Docs and comments describe the current state only** (DOC-01 to DOC-04). Edits rewrite in place. History belongs in git, ADRs and `CHANGELOG.md`.
- **No `TODO` or `FIXME`.** Known work is tracked as an issue (DOC-07).
- **`docs/inventory/` describes the old API.** Use it to look up old behavior, and never cite it outside the migration workspace.

## Commands

```
just ci      # every check CI runs
just test    # the tests
just run     # the API, with the settings in .env
```
