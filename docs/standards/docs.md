# Documentation and comment rules (`DOC`)

## Current state only

### DOC-01 Docs and comments describe the system as it is
No document or comment records what changed, what something replaced, what an earlier version did, who asked for it, or when it was written. The migration workspace is the exception: the tracker records the migration's progress (DOC-16), and the inventory describes the old codebase (DOC-04).
- Why: history goes stale, and readers need the current state, not the path to it (D27).
- Check: `ci: check-docs` flags the common markers (`previously`, `no longer`, `used to`, `legacy`, `now uses`, and dates in comments), plus `review`.

### DOC-02 Edits rewrite in place
An edit rewrites the affected text so it reads as if it had always said that. There are no "update", "note", "refinement" or dated additions, and no struck-through text.
- Why: an edited document must read as one coherent description.
- Check: `review`.

### DOC-03 History has three homes
History is recorded only in:
- git (commit messages and pull request descriptions)
- architecture decision records (ADRs), which hold the context and reasoning behind each decision
- `CHANGELOG.md` (release notes for forks)
- Why: each kind of history has one place, so it never leaks into docs or code.
- Check: `review`.

## The documentation set

### DOC-05 Doc comments state what and the contract
A doc comment on a public item states what the item does and its contract: inputs, outputs, errors and invariants. It doesn't describe how the item is implemented.
- Why: callers need the contract. The implementation can change without the comment going stale.
- Check: `review`.

### DOC-06 Inline comments say only what the code can't show
An inline comment exists only where the reader needs something the code can't express, such as an invariant, a non-obvious constraint, or why the obvious approach is wrong. A comment that restates the code is deleted.
- Why: every comment that restates code is a second copy that drifts out of date.
- Check: `review`.

### DOC-07 No reviewer voice and no to-do markers
Comments don't appeal to authority ("industry standard", "best practice", "SOC 2 requires"), don't label fixes ("security fix"), and don't contain `TODO`, `FIXME` or `XXX`. Known work is tracked as an issue, and in the tracker while the migration runs (DOC-14). The inventory, which quotes another codebase, is exempt (DOC-04).
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
- `.claude/`: the Claude Code agent and skill definitions, including the conformance reviewer (D27).

A new kind of document needs a rule change.
- Why: every fact has one home, so nothing is duplicated and nothing drifts (D27).
- Check: `ci: check-docs` (no Markdown files outside these locations, except the migration workspace in DOC-04).

### DOC-09 Docs are written for a developer new to the codebase
Each technical term is defined in the glossary or in the sentence where it first appears. An acronym is spelled out where a document first uses it: it stands alone in parentheses right after the words it abbreviates, as in "Simple Mail Transfer Protocol (SMTP)", or those words stand in parentheses right after it, or after the hyphenated name it starts. The glossary lists the exceptions: the acronyms a developer is assumed to know, and the names written in capitals, such as SHA. Sentences use the active voice, and the same term is used throughout (CODE-10).
- Why: the reader is often someone who forked the template yesterday.
- Check: `ci: check-docs` (where each acronym is first used, outside the migration workspace), plus `review`.

### DOC-14 A fact lives in one place
Each kind of fact has one home, listed below. A fact is written in its home as soon as it is known, and every other mention links to it or cites its ID instead of restating it.

| Fact | Home |
|---|---|
| A rule every module follows | its rule in `docs/standards/` |
| What a module guarantees | a behavior in the module's spec |
| How a module meets its behaviors, and the plan for a part not yet built | the Design section of the module's spec |
| A question only building can settle | the Open design questions of the module's spec |
| How the parts fit together, and what a term means | `docs/architecture.md` |
| A decision between valid designs | what it settles goes into the rules and specs it settles, and its record into an ADR; while the migration runs, a row of the tracker's decisions indexes it (DOC-16) |
| Why a change was made, and the evidence behind it | the commit message (DOC-03) |
| A working rule for Claude Code sessions | `CLAUDE.md` |
| How to operate the system: CI, releases, deploys, backups, dashboards and alerts | `docs/operations/` |
| How to carry out a task, such as adding a module or renaming a fork | `docs/guides/` |
| A setting, with its type, default and meaning | `.env.example` (OPS-02) |
| What a public item does | its doc comment (DOC-05) |
| What the code can't show | an inline comment (DOC-06) |
| Known work, and built code that doesn't follow a rule yet | an issue (DOC-07); while the migration runs, the tracker (DOC-16) |
| A number a tool computes, such as how many tests prove behaviors | no document: the tool reports it |

- Why: a fact written in two places drifts apart, and a reader can't tell which copy is true. A fact written anywhere but its home is missed by whoever reads the home (D27).
- Check: `ci: check-docs` (the formats that DOC-15 and DOC-16 fix), plus `review`.

### DOC-15 Documents keep their format
- A spec has the sections that the [spec README](../spec/README.md#sections) lists, in that order, and no others.
- Every table of behaviors has the columns `ID`, `Behavior`, `Rules` and `Test prefix`. A behavior's ID starts with its module's name in capitals, and its test prefix is the ID in lowercase, with `_` in place of `-` and after the number, such as `workspaces_005_` for `WORKSPACES-005`.
- The spec README's index links every spec.
- A module's spec lists, in its Routes table, each operation that `openapi.json` documents for the module, with its access level. Each operation names its module as its first tag. Once the module is built, its Routes table lists no operation the document lacks; until then, the table is the plan.
- A rule follows the format in the [standards README](README.md#rule-format): a heading with its ID and its name, the statement, a `Why` line, and a `Check` line that starts with one of the kinds of check the README lists. Its ID starts with the prefix the README gives its file.
- Every table in a document, a block of lines whose first character other than whitespace is `|`, has a delimiter row under its header, and a blank line after it unless it ends the document. Every row, the delimiter row among them, has as many cells as the header: one pipe at each end of a row bounds it, and a pipe that follows a backslash belongs to its cell.
- Why: every document of a kind reads the same way, and an edit that changes a document's shape fails CI instead of waiting for a reader to notice (D27).
- Check: `ci: check-docs`, plus `review` (an operation's first tag names the module that serves it, and a spec lists the routes its module serves outside `openapi.json`, such as the health probes).

## Specs, ADRs and the changelog

### DOC-10 Every spec behavior has an ID and a test
Each behavior in `docs/spec/<module>.md` has an ID (for example `WORKSPACES-005`), a statement, and at least one test named by that ID (TEST-01). While the migration runs, specs are written ahead of their code, so a behavior that no test proves is reported instead of failing the check.
- Why: the spec, the tests and review findings all refer to the same IDs.
- Check: `ci: check-test-map`.

### DOC-11 ADRs are immutable once accepted
An ADR has four sections: context, decision, consequences and status. After it is accepted, only its status changes, and only to "superseded by" and the ID of a later ADR that replaces it.
- Why: the decision record stays a true record of what was decided and why.
- Check: `ci: check-docs` (the body of an accepted ADR is unchanged in git), plus `review`.

### DOC-12 The changelog serves forks
`CHANGELOG.md` follows the Keep a Changelog format. Each release has upgrade notes that tell a fork what to port, and security fixes are marked.
- Why: forks diverge, and porting by hand is how they receive fixes (D29).
- Check: `review`.

### DOC-13 Links and IDs resolve
Every link in the docs resolves, and every rule ID or behavior ID cited in a document, comment or test exists, except in the inventory, whose IDs are its own (DOC-04). Every CI check a rule names is listed in the table of checks in `docs/operations/ci.md`, or, while the migration runs, in the tracker's CI checks.
- Why: a broken reference is a silent lie.
- Check: `ci: check-docs`.

## The migration workspace

### DOC-04 Docs describe only this codebase
Documents and comments describe only this codebase. The one exception is the migration workspace:
- `docs/inventory/` describes the old codebase, quotes it, and numbers its entries with IDs of its own, so `check-docs` reads neither its IDs nor its markers (DOC-01, DOC-07, DOC-13).
- `docs/MIGRATION.md`, the tracker, records the migration (DOC-16).
- `CLAUDE.md` may point to the inventory, so a session can look up old behavior.
- Until the ADRs are written, documents cite decisions by their IDs, such as D4, and `docs/architecture.md` and the tracker link to the decisions' records in the inventory.

When the migration completes, the workspace is deleted, and these exceptions with it.
- Why: a fork's developers never saw any other codebase, so references to one mean nothing to them.
- Check: `ci: check-docs` (known names of other codebases), plus `review`.

### DOC-16 The tracker holds status, not content
While the migration runs, `docs/MIGRATION.md` records its progress, which DOC-01 otherwise forbids. It holds status and pointers. Every other fact lives in its home (DOC-14). Its sections, in this order:
1. **Current work.** The next step, and the decisions that wait on the owner. At most 200 words. A session rewrites it, and never appends to it.
2. **Build order.** The steps, and what each one builds.
3. **Foundation.** Each part that isn't a module, with its decisions and the step that builds it.
4. **Modules.** Each module's stages.
5. **CI checks.** Each check a rule names, with the step that builds it.
6. **Known gaps.** Each rule or behavior that built code doesn't follow yet, with the step that closes the gap.
7. **Rule proposals.** Each proposal that waits, with the step that decides it ([Changing a rule](README.md#changing-a-rule)).
8. **Conformance reviews.** One row per pass, with its counts, its outcome and the commits that acted on it. The details are in those commits' messages.
9. **Decisions.** The index of decisions, until their ADRs are written.

When the migration completes, the tracker and this rule are deleted.
- Why: every session edits the tracker. A fixed shape keeps it a status board, so a decision reaches its home instead of waiting in the tracker where the rule's readers never look (D27).
- Check: `ci: check-docs` (the sections, in order, and the length of Current work), plus `review`.
