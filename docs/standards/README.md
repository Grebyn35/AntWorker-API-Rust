# Standards

The rules every module follows. Each rule has an ID, a statement a reviewer can answer yes or no, the reason for it, and how it is checked.

A review finding cites a rule ID from these files or a behavior ID from a module spec. A finding that cites neither is a proposal for a new rule, not a defect.

## Files

| Prefix | File | Covers |
|---|---|---|
| `ARCH` | [architecture.md](architecture.md) | Crates, module boundaries, layers, dependencies |
| `CODE` | [code.md](code.md) | Rust style, lints, naming, size, dependencies |
| `DOC` | [docs.md](docs.md) | Documents, comments, ADRs, changelog |
| `ERR` | [errors.md](errors.md) | Error types, codes, propagation, logging of errors |
| `API` | [api.md](api.md) | The HTTP contract: paths, methods, bodies, errors, pagination, idempotency |
| `DATA` | [data.md](data.md) | Schema, migrations, database roles, tenancy, queries, transactions |
| `SEC` | [security.md](security.md) | Secrets, cryptography, authentication, authorization, outbound requests |
| `OBS` | [observability.md](observability.md) | Logs, traces, metrics, dashboards, alerts |
| `TEST` | [testing.md](testing.md) | What is tested, at which level, and how |
| `OPS` | [operations.md](operations.md) | Configuration, background jobs, runtime, CI, release, deploy, backups |

## Rule format

```
### ERR-04 Infrastructure errors stay in their adapter
The rule, stated so a reviewer can answer yes or no.
- Why: the reason, in one line, with the decision it comes from.
- Check: how it is enforced.
```

`Check` names the strongest enforcement available, in this order of preference:

| Check | Meaning |
|---|---|
| `compiler` | The code does not compile if the rule is broken |
| `clippy` | A configured lint fails CI |
| `ci: <task>` | A `just` task fails CI |
| `test` | A test fails |
| `review` | Only a reviewer can check it. Used when nothing above can |

A `ci:` check that isn't listed in [docs/operations/ci.md](../operations/ci.md) doesn't exist yet. Until it is built, its rule is enforced by review.

## Reviews

A review checks a change, a module or the documents against these rules. It never asks for open-ended improvements.

### Passes

A review runs in passes over its whole scope. Each pass is two independent reviews, by reviewers that use different models, read in different orders (one rule by rule, the other file by file), and don't see each other's findings. A pass's findings are the union of the two, with duplicates merged, and a finding is `must-fix` if either reviewer marks it so. The pass records how many findings each reviewer made and how many both made: when they share few, each one misses much, and more probably remains to find.

### Findings

- Each finding falls into a fixed category: a broken rule, a contradiction, a rule that can't be checked, a missing rule, or a reference that doesn't resolve.
- Each finding cites the rule IDs, behavior IDs or decisions involved.
- Each finding is marked `must-fix` or `should-fix`. A finding is `must-fix` when an implementer following the documents would build the wrong thing, or would find two rules impossible to satisfy together. In a review of code, a finding is `must-fix` when the code breaks a cited statement, or when two statements can't both hold for it.
- Each finding is tagged `regression` if it involves text or code that the previous pass's fixes changed, or `found` if it doesn't.

### Fixing

- A fix rewrites the whole group of rules a finding touches, so they stay coherent with each other. It never patches a single sentence in isolation.
- A fix writes each fact in its home, and nowhere else (DOC-14).
- A fix for a defect that a check could have caught also adds that check, such as a clippy ban or a test, or proposes it. The next pass then doesn't have to look for that kind of defect again.
- A fix adds a new rule only when a `must-fix` finding requires one. Any other "missing rule" finding becomes a proposal, as in [Changing a rule](#changing-a-rule).

### When the review stops

The review stops at the first of these:
1. **A pass reports no `must-fix` findings.** That pass's `should-fix` findings are applied and checked one by one, without another full pass. The review is complete.
2. **A pass makes no progress.** It reports at least as many `must-fix` findings as the pass before, or a `must-fix` finding tagged `regression`.
3. **Six passes have run.**

The second condition alone ends every review, because the number of `must-fix` findings has to fall with each pass. The cap bounds what a slowly falling count can cost: a scope that still yields new `must-fix` findings after five rounds of fixes points at loose rules more than at its code, and the resolutions below settle what remains.

If the review stops under 2 or 3, each remaining `must-fix` finding gets exactly one resolution. No further full pass runs.
- **Fix.** A defect in code is fixed, with a test that fails before the fix wherever a test can show it.
- **Decide.** A choice between valid designs is written up with its options and a recommendation, and decided as in [Changing a rule](#changing-a-rule). The decision becomes an ADR.
- **Prove.** A question about how something behaves is settled by a small throwaway test, and the rule is written from the result.
- **Defer.** A question that only building can settle is removed from the rule, which keeps only what is certain. The question goes into the open design questions of the spec it concerns, together with what will settle it.

The changed items are then checked one by one. The documents never state something known to be wrong: each issue is fixed, decided, or listed as an open question with the test that resolves it.

## Changing a rule

1. Write a proposal: the rule, the reason, and the modules it affects.
2. Decide it:
   - The owner decides a change to what the API guarantees: a capability, a guarantee that a rule or a behavior states, the security bar, the handling of personal data, or the licenses dependencies may use.
   - The developer making the change decides any other change, such as how code is written, tested, checked or documented, or new wording that keeps a rule's meaning. The commit message gives the reason.
3. Apply an accepted change in the commit that accepts it: rewrite the rule, and every spec and document it affects (DOC-14). Built code that doesn't follow the new rule yet is recorded as a known gap, with the step that brings it in line (DOC-07). No document goes on stating a rule that has been replaced.
4. A change that reverses a decision gets a new ADR that supersedes the old one.

A proposal that isn't decided at once waits with the step that decides it: the next step that builds or changes code the rule covers, because that step has the evidence.

Rule IDs are never reused, so a retired rule leaves a gap in the numbering.
