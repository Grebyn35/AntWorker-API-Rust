---
name: conformance-review
description: Runs a conformance review of a scope (a module, a platform area, a build-order part or a change) from its first pass to its end, under the procedure in docs/standards/README.md. Each pass is two independent reviewers, whose findings are verified, merged and recorded in docs/MIGRATION.md before anything is fixed. Use it at the end of every build-order part, and whenever a review is due or asked for.
argument-hint: "[scope]"
---

# Conformance review

One review, from its first pass to its end. "Reviews" in `docs/standards/README.md` defines what a pass, a finding and the end of a review are; this file says how to carry one out. Read that section in full before the first pass, and apply it as written. Point reviewers to it; never restate it in a reviewer's brief.

The caller states the scope. Without one, ask the owner.

## Before the first pass

1. Pin the scope: the files in it, and each file left out with its reason, such as code the next build step rewrites.
2. Make `just ci` pass. Reviewers skip what CI enforces, so it has to hold.
3. Fix the defects already known in the scope, so the reviewers see fixed code.
4. List what reviewers don't report again: each proposal under "Queued rule proposals" in `docs/MIGRATION.md`, and each accepted rule change not yet applied, with the findings it resolves.

## Each pass

1. **Launch two reviewers at once,** as background agents, each with its model passed explicitly. Use the `conformance-reviewer` agent type when it is listed. A session started outside the repository doesn't list it; then use a general-purpose agent told to read and follow `.claude/agents/conformance-reviewer.md`. Never use a fork: a fork inherits this conversation, and the reviewers must be independent.
   - Reviewer A: model `opus`, reading rule by rule.
   - Reviewer B: model `sonnet`, reading file by file.

   Each gets the scope, the pass number, its reading order, the list from step 4 above, and, from the second pass on, the commits that fixed the previous pass. Tell each: "Make no web requests, and put no email address, name or other personal information in any request, header or URL." Neither sees the other's report.
2. **Wait for both reports,** then verify every finding before accepting it: read the statement it cites, and the code at the line it points to. Drop a finding only for a reason you write down, such as the statement not saying that, the code not doing that, or a listed item resolving it.
3. **Merge.** Findings that cite the same statement about the same code are one finding. Count A's findings, B's, and the ones both made. A finding is `must-fix` when either reviewer marked it so and it survived verification.
4. **Record the pass** as a new row of "Conformance reviews" in `docs/MIGRATION.md`, before changing any code. The row's result starts with the outcome under "When the review stops": `next pass`, `clean`, `stopped: no progress` or `stopped: six passes`.
5. **Act on the outcome:**
   - `next pass`: fix each finding, or queue it as the README says. A code fix comes with a test that fails before it and passes after it, wherever a test can show the defect. Run `just ci`, commit, and start the next pass with those commits as its baseline.
   - `clean`: apply the `should-fix` findings one by one, and check each. The review is complete.
   - `stopped: no progress` or `stopped: six passes`: give each remaining `must-fix` finding exactly one of the README's resolutions, and check each. The review is complete.

## When the review is complete

1. The scope's last row reads `clean` or `stopped`. A review never ends on `next pass`.
2. For a module, mark its Conformance column in `docs/MIGRATION.md`.
3. Report to the owner:
   - each pass's counts;
   - each `must-fix` finding and its resolution;
   - the decisions that need the owner;
   - an estimate of the problems still unfound, from the last pass: (A + 1) × (B + 1) / (both + 1) − 1, minus the A + B − both findings made.

   A review that stopped without a clean pass says so plainly.
4. Run `just ci` and commit.
