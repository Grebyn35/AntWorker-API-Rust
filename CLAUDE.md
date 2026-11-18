# Working in this repository

This is a Rust API built as a template to fork. Read these before changing anything:

1. [docs/architecture.md](docs/architecture.md): how the API is built, and the glossary.
2. [docs/standards/README.md](docs/standards/README.md): how rules work, and how reviews run.
3. The rules in [docs/standards/](docs/standards/) that your change touches.
4. The spec in [docs/spec/](docs/spec/README.md) for the module you are changing.
5. To add a module or a route: [docs/guides/adding-a-module.md](docs/guides/adding-a-module.md), which says where each piece goes.

[docs/MIGRATION.md](docs/MIGRATION.md) tracks what is built, what comes next, and the known gaps. It holds status only (DOC-14).

## Rules for every change

- **Every fact has one home** (DOC-07). A rule change, a decision or a design choice is written into its home in the commit that makes it, never parked in the tracker. The reasons and the evidence go in the commit message.
- **Spec first.** A change to behavior updates the module's spec before or with the code.
- **Docs for unbuilt parts are the plan.** Until a part is built, its spec and its sections of `docs/architecture.md` describe the intended design, not code. The plan states what a part must do and guarantee; the tools and libraries it uses are chosen when its spec is written, just before it is built. When building shows the plan is wrong, correct the plan in the same change as the code. A rule changes through "Changing a rule" in the standards README. A behavior is proven only when a test named by its ID passes.
- **Tests prove IDs.** Every test name starts with the behavior ID or rule ID it proves, such as `auth_004_` or `ops_03_` (TEST-01).
- **A sentence about the code holds for all the code it names.** Before a commit says that every function, route or line of a kind does something, such as taking a proof, list each one that the change wrote or calls, and check it against the sentence: a reviewer reads the sentence against every one of them. A sentence that says what a task or a function does, such as which tasks create a database, is written with that code open, and a change of where something is, or of what it is called, searches the documents and comments for every name they give it. A sentence that says what a check fails on is tried first against an input built to break the sentence and pass the check, such as two components marked with one action where the check compares the actions' names; the sentence then says only what the check refuses. What a check reads of the code is said in the check's own terms, such as the text it matches, in one place, which every other mention points to, and the check refuses what it can't read where it can, as `check-openapi` refuses an event's name that isn't a quoted string. A fix of a review's finding states the case it fixes, not a rule about all of a kind, such as what every module does: its sentences are the ones the next pass reads first. A fix that changes what a term names, such as a budget that is no bucket any more, or that says what a rule allows, is read before its commit against every statement that uses the term, and against the rule's own words.
- **Findings cite what they break.** A review finding cites a statement, as the standards README says. Anything else is a proposal, decided as "Changing a rule" in the same README says.
- **Ask the owner only what the next step needs.** The owner decides what the API guarantees, as "Changing a rule" in the standards README says. A session puts to the owner only a decision that blocks the next step of the build order, in plain words, with an example and a recommendation, and decides the rest itself, with its reasons in the commit message. A question that blocks nothing waits with the step that decides it.
- **An open decision's state is in Current work as soon as it changes.** When a discussion changes a recommendation, or the owner settles part of a decision, the session rewrites Current work and pushes it then, not at its end, even when it changed no code: another session may start meanwhile, on this checkout or on another machine, and it reads the repository, not this session. A session checks `git log` before its first edit, since another may have committed since it started.
- **No open-ended evaluations.** Reviews follow the procedure and the stopping rule in the standards README. A conformance review runs through the `conformance-review` skill in `.claude/skills/`, which launches the `conformance-reviewer` agent in `.claude/agents/`, whose prompt is fixed.
- **Docs and comments describe the current state only** (DOC-01, DOC-13). Edits rewrite in place. History belongs in git, architecture decision records (ADRs) and `CHANGELOG.md`.
- **End every session with the `handoff` skill** in `.claude/skills/`, so the next session, on any machine, starts from the repository alone.
- **A session pushes `main` once its commits are ready, and lets GitHub's CI run the full checks.** The owner allows it without asking, in the middle of a session as at its end: a commit is made once `just checks` passes, or, for a small change, its quick checks and the tests the change touches, and the push's CI run stands in for a local `just ci`, which takes several times as long. The session watches each run to its end, fixes a failure in a commit of its own, and cancels a queued run that a newer push makes redundant. An advisory that RustSec publishes against a locked dependency fails `deny` on every commit from then on, and `just checks` stops there: the session settles it in a commit of its own, by an update where a release fixes it, and otherwise by asking the owner, since SEC-15 fails on every advisory, and it runs by itself each check that `just checks` runs after `deny`. A release still waits for the owner (below).
- **No `TODO` or `FIXME`.** Known work is tracked as an issue, and in the tracker while the migration runs (DOC-04, DOC-07).
- **Releases are the owner's to start.** A release makes the repository's name public in Sigstore's logs, so a session runs `just release` with a version only when the owner asks for that release. Without a version, it only shows the versions that can come next.
- **A session removes what it starts by hand.** A container that a session runs outside the tasks, for a measurement or an experiment, goes with its volumes (`docker rm --force --volumes`) before the session ends, and so does a volume or a database that the session created: the tasks remove what they start (OPS-21), and nothing removes the rest.
- **Agents send no personal data.** An agent that may make web requests is told to put no email address, name or other personal information in any request, header or URL.
- **`docs/inventory/` describes the old API.** Use it to look up old behavior. DOC-13 lists the only places outside it that may point into it. A triage also reads what the old API's client does with a behavior, in the client's code: a verdict of kept holds for what that client relies on, as the heartbeat's didn't while the stream sent a comment, which the client's parser drops.

## Commands

`just` alone lists the tasks, by group, leaving out those only CI runs. The ones a change most often needs:

```
just setup           # checks the tools' pinned versions, and writes .env, with generated passwords
just dev             # the API with auto-reload; `just dev observe` adds the observability stack
just ci              # every check CI runs, the image's included
just checks          # the checks of the code that need neither the image nor a copy of the repository: what a change runs before its commit, while the push's CI run does the rest
just image           # the image, for both platforms, into target/image
just e2e             # the end-to-end tests, against the image in target/image; `just e2e obs_13` runs those whose names hold obs_13
just release         # the versions that can come next; `just release 1.4.0` releases main as 1.4.0, once CI passes (docs/operations/release.md)
just check-test-map  # which spec behaviors tests prove
just new-module      # a new module, such as `just new-module order_items`, holding one workspace resource, with its migration, permissions, spec and tests (ARCH-03)
just check-removable # each run's copy of the repository, such as a workspace shape's, without the parts it removes, passes the checks (ARCH-07); a change to a part's marked Rust runs it for that part's runs before its commit, since `check-parts` lints nothing, and for a run that removes each part whose marked lines the change stands beside, since those lines may hold what the change needs, such as an import, or the arm after which its own is no match's last; a removal that no run makes is tried with `just remove` and `just checks` on a copy of the tracked files and `.env` outside the repository, with `CARGO_TARGET_DIR` at a run's `target/removable/<run>/target` and `CARGO_INCREMENTAL=0`, as that run builds, which leaves `parts.yaml` as it is
just check-new-module # a copy of the repository with two modules that `just new-module` wrote passes the checks (ARCH-03); a change that grows a file every module shares, such as `composition/mod.rs`, to within 20 lines of CODE-08's 500 runs it before its commit, since each new module adds lines there, and so does a change of an item that the templates in `crates/xtask/templates/new-module/` use, such as one that another module exports, since `just checks` builds no template
just test            # the tests; they start Postgres through Compose
just run             # the API with the settings in .env, after bootstrap and migrate
just migrate         # applies new migrations
just prepare         # rewrites the query cache in .sqlx/ (DATA-14)
just metrics         # rewrites the metric catalog from the declarations (OBS-12)
```

How queries, migrations and the query cache in `.sqlx/` work together, and how to edit a migration that no release holds, is in [the guide to adding a module](docs/guides/adding-a-module.md#migrations-and-the-query-cache).
