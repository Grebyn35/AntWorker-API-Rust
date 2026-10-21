# CI

CI runs `just ci` on every pull request and on every push to `main` (OPS-12). Each check below is a `just` task.

## Checks that exist

| Check | `just` task | Enforces |
|---|---|---|
| Formatting | `fmt-check` | CODE-02 |
| Clippy, with warnings as errors | `lint` | CODE-03 to CODE-06, CODE-08, CODE-14 |
| Tests | `test` | TEST-01 and every behavior's test |
| Dependencies: advisories, licenses, bans and sources | `deny` | CODE-11, CODE-12, SEC-13, SEC-14 |

A `ci:` check that a rule names but that isn't listed here doesn't exist yet. Until it is built, the rule is enforced by review.

## Checks required for merging

None yet. Branch protection is applied by `just protect-branch` once the required checks are chosen (OPS-12).
