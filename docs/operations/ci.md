# CI

CI runs `just setup`, then `just ci`, on every pull request and on every push to `main` (OPS-12). A scheduled workflow also runs `just deny` every day, so an advisory published against a locked dependency fails CI without waiting for a push (SEC-13). `just setup` writes `.env` with generated passwords, and the tests start Postgres through Compose with them, as they do on a developer's machine (TEST-11). Each check below is a `just` task.

## Checks that exist

| Check | `just` task | Enforces |
|---|---|---|
| Formatting | `fmt-check` | CODE-02 |
| Clippy, with warnings as errors | `lint` | CODE-03 to CODE-06, CODE-08, CODE-14, DATA-20 (the query functions and string-SQL executor methods outside the macros), DATA-21 (pools, connections, transactions and the listener outside `platform::db`) |
| Tests, against Postgres | `test` | TEST-11, and every behavior and rule a test proves |
| Dependencies: advisories, licenses, bans and sources | `deny` | CODE-11, CODE-12, SEC-13, SEC-14 |
| The map from spec behaviors to tests | `check-test-map` | TEST-01, DOC-10 |
| Links, IDs and named CI checks resolve; no history or unfinished-work markers; documents only in the documentation set; specs, rules and the tracker keep their format | `check-docs` | DOC-01, DOC-07, DOC-08, DOC-13, DOC-14, DOC-15, DOC-16 |

`check-test-map` and `check-docs` are subcommands of the `xtask` crate (ARCH-01). `check-test-map` prints how many of each module's behaviors a test proves. A behavior that no test proves fails it once `docs/MIGRATION.md` is deleted, and is reported until then (DOC-10).

`check-docs` doesn't cover two of the rules that name it: the names of other codebases (DOC-04), and accepted ADRs (DOC-11), which waits for the first ADR. Review enforces both until then.

Builds check queries against the query cache committed in `.sqlx/` (DATA-20), so a query missing from the cache fails `lint` and `test`. `just prepare` rewrites the cache from the migrated database.

A `ci:` check that a rule names but that isn't listed in the table above doesn't exist yet. Until it is built, the rule is enforced by review. A check is built when the code it checks exists, and a check that never catches a real problem is removed.

## Checks required for merging

None yet. Branch protection is applied by `just protect-branch` once the required checks are chosen (OPS-12).
