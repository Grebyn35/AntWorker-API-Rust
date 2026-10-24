# CI

CI runs `just setup`, then `just ci`, on every pull request and on every push to `main` (OPS-12). A scheduled workflow also runs `just deny` every day, so an advisory published against a locked dependency fails CI without waiting for a push (SEC-13). `just setup` writes `.env` with generated passwords, and the tests start Postgres through Compose with them, as they do on a developer's machine (TEST-11). Each check below is a `just` task.

## Checks that exist

| Check | `just` task | Enforces |
|---|---|---|
| Formatting | `fmt-check` | CODE-02 |
| Clippy, with warnings as errors | `lint` | CODE-03 to CODE-06, CODE-08, CODE-14, CODE-15, TEST-07 (sleeps in tests), ERR-11 (results dropped with `.ok()`), API-14 and API-17 (routes registered outside `platform::http`), DATA-20 (the query functions and string-SQL executor methods outside the macros), DATA-21 (pools, connections, transactions and the listener outside `platform::db`) |
| Tests, against Postgres | `test` | TEST-11, and every behavior and rule a test proves |
| Dependencies: advisories, licenses, bans and sources | `deny` | CODE-11, CODE-12, SEC-13, SEC-14 |
| The map from spec behaviors to tests | `check-test-map` | TEST-01, DOC-10 |
| Links, IDs and named CI checks resolve; no history or unfinished-work markers; documents only in the documentation set; acronyms spelled out where first used; specs, rules, tables and the tracker keep their format; each spec's Routes table matches `openapi.json` | `check-docs` | DOC-01, DOC-07, DOC-08, DOC-09, DOC-13, DOC-14, DOC-15, DOC-16 |
| The committed `openapi.json`: its paths, its property names, each operation's access level and response descriptions, the codes of its error responses, and references that don't resolve | `check-openapi` | API-01, API-05, API-17, ERR-02, ERR-13 |
| Log and span field names come from the list in `platform::telemetry::names`; constant messages; `skip_all` on every `#[instrument]`; `app` code other than `main.rs` writes only `DEBUG` events and opens no spans; print lints excepted only where printing is allowed | `check-instrument` | CODE-03, OBS-01, OBS-03, OBS-04, OBS-05, OBS-06 |

`check-test-map`, `check-docs`, `check-openapi` and `check-instrument` are subcommands of the `xtask` crate (ARCH-01). `check-test-map` prints how many of each module's behaviors a test proves. A behavior that no test proves fails it once `docs/MIGRATION.md` is deleted, and is reported until then (DOC-10).

`check-openapi` covers the parts of the rules that name it whose subjects exist: the paths, the property names, the access levels, the response descriptions, the error codes, and the references. It checks list shapes (API-05), methods (API-03), statuses (API-04), scopes in paths (API-02) and formats (API-11) once a route has them, and the breaking-change diff (API-18, ERR-02) from the first release on. Tests, not this check, compare the committed document with the one the code writes, and every documented path's methods with the ones it serves (API-17).

`check-docs` doesn't cover two of the rules that name it: the names of other codebases (DOC-04), and accepted architecture decision records (ADRs), whose check waits for the first ADR (DOC-11). Review enforces both until then.

Builds check queries against the query cache committed in `.sqlx/` (DATA-20), so a query missing from the cache fails `lint` and `test`. `just prepare` rewrites the cache from the migrated database.

A `ci:` check that a rule names but that isn't listed in the table above doesn't exist yet. Until it is built, the rule is enforced by review. A check is built when the code it checks exists, and a check that never catches a real problem is removed.

## Checks required for merging

None yet. Branch protection is applied by `just protect-branch` once the required checks are chosen (OPS-12).
