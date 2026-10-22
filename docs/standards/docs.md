# Documentation and comment rules (`DOC`)

## Current state only

### DOC-01 Docs and comments describe the system as it is
No document or comment records what changed, what something replaced, what an earlier version did, who asked for it, or when it was written.
- Why: history goes stale, and readers need the current state, not the path to it (D27).
- Check: `ci: check-docs` flags the common markers (`previously`, `no longer`, `used to`, `legacy`, `now uses`, and dates in comments), plus `review`.

### DOC-02 Edits rewrite in place
An edit rewrites the affected text so it reads as if it had always said that. There are no "update", "note", "refinement" or dated additions, and no struck-through text.
- Why: an edited document must read as one coherent description.
- Check: `review`.

### DOC-03 History has three homes
History is recorded only in:
- git (commit messages and pull request descriptions)
- ADRs (the context and reasoning behind a decision)
- `CHANGELOG.md` (release notes for forks)
- Why: each kind of history has one place, so it never leaks into docs or code.
- Check: `review`.

### DOC-04 Docs describe only this codebase
Documents and comments describe only this codebase. The one exception is the migration workspace, `docs/inventory/` and `docs/MIGRATION.md`. When the migration completes, the workspace is deleted, and this exception is deleted with it.
- Why: a fork's developers never saw any other codebase, so references to one mean nothing to them.
- Check: `ci: check-docs` (known names of other codebases), plus `review`.

## The documentation set

### DOC-05 Doc comments state what and the contract
A doc comment on a public item states what the item does and its contract: inputs, outputs, errors and invariants. It doesn't describe how the item is implemented.
- Why: callers need the contract. The implementation can change without the comment going stale.
- Check: `review`.

### DOC-06 Inline comments say only what the code can't show
An inline comment exists only where the reader needs something the code can't express, such as an invariant, a non-obvious constraint, or why the obvious approach is wrong. A comment that restates the code is deleted.
- Why: every comment that restates code is a second copy that drifts out of date.
- Check: `review`.

### DOC-07 No reviewer voice and no TODOs
Comments don't appeal to authority ("industry standard", "best practice", "SOC 2 requires"), don't label fixes ("security fix"), and don't contain `TODO`, `FIXME` or `XXX`. Known work is tracked as an issue.
- Why: a comment explains the code. It doesn't argue for it or hold a backlog.
- Check: `ci: check-docs` for the markers, plus `review`.

### DOC-08 The documentation set is closed
These are the only documents:
- `README.md`: what the template is, and a quickstart.
- `CLAUDE.md`: the working rules for Claude Code.
- `SECURITY.md`: the disclosure policy.
- `CHANGELOG.md`: the release notes for forks.
- `docs/architecture.md`: the design, including the glossary.
- `docs/coverage.md`: the map from every foundational area to the rules and ADRs that cover it.
- `docs/standards/`: these rules.
- `docs/spec/`: one file per module.
- `docs/adr/`: decision records.
- `docs/operations/`: CI, release, deploy, backups, observability, the metric catalog, runbooks, and the threat model.
- `docs/guides/`: adding a module, and renaming a fork.
- `.claude/`: the Claude Code agent and command definitions, including the conformance reviewer (D27).

A new kind of document needs a rule change.
- Why: every fact has one home, so nothing is duplicated and nothing drifts (D27).
- Check: `ci: check-docs` (no Markdown files outside these locations, except the migration workspace in DOC-04).

### DOC-09 Docs are written for a developer new to the codebase
Each technical term is defined in the glossary or in the sentence where it first appears. Acronyms are spelled out on first use. Sentences use the active voice, and the same term is used throughout (CODE-10).
- Why: the reader is often someone who forked the template yesterday.
- Check: `review`.

## Specs, ADRs and the changelog

### DOC-10 Every spec behavior has an ID and a test
Each behavior in `docs/spec/<module>.md` has an ID (for example `WORKSPACES-005`), a statement, and at least one test named by that ID (TEST-01).
- Why: the spec, the tests and review findings all refer to the same IDs.
- Check: `ci: check-test-map`.

### DOC-11 ADRs are immutable once accepted
An ADR has four sections: context, decision, consequences and status. After it is accepted, only its status changes, and only to "superseded by ADR-NNNN" when a later ADR replaces it.
- Why: the decision record stays a true record of what was decided and why.
- Check: `ci: check-docs` (the body of an accepted ADR is unchanged in git), plus `review`.

### DOC-12 The changelog serves forks
`CHANGELOG.md` follows the Keep a Changelog format. Each release has upgrade notes that tell a fork what to port, and security fixes are marked.
- Why: forks diverge, and porting by hand is how they receive fixes (D29).
- Check: `review`.

### DOC-13 Links and IDs resolve
Every link in the docs resolves, and every rule ID or behavior ID cited in a document, comment or test exists. Every CI check a rule names is listed in `docs/operations/ci.md`, or, while the migration runs, in the tracker's list of CI checks.
- Why: a broken reference is a silent lie.
- Check: `ci: check-docs`.
