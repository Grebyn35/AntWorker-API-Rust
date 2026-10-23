---
name: handoff
description: Ends a working session so the next one, on any machine, starts from the repository alone. Writes each fact the session learned into its home (DOC-14), removes what the session's changes made untrue, brings the tracker's status up to date, rewrites its Current work (DOC-16), and commits after `just ci` passes. Use it before the last commit of every session, and whenever the owner asks to wrap up.
---

# Handoff

A session ends with every fact it learned in its home, and nothing it changed contradicted anywhere else. DOC-14 lists the homes and DOC-16 the tracker's sections; read both before step 1, and apply them as written.

1. **List what the session learned or decided:** rule changes, behaviors, designs, decisions, open questions, known gaps, proposals, and lessons about the codebase. Include what was only said in conversation, such as a decision the owner made.
2. **Write each into its home.** Rewrite the surrounding text so it reads as one description (DOC-02). A decision goes into the rules and specs it settles, and gets a row in the tracker's Decisions; never into Current work.
   - A lesson about the codebase goes into the repository, in its home under DOC-14: usually a doc comment or an inline comment where the code is, a rule, or `CLAUDE.md` for a working rule of sessions. A session's memory stays on one machine, so it never holds the only copy.
   - The reasons and the evidence go into the commit message.
3. **Remove what became untrue.** For each name, ID, setting or behavior the session changed, search the documents and comments for other statements about it, and rewrite or delete each one the change made untrue. Delete any count or status that a tool reports.
4. **Bring the tracker's status up to date:** Foundation, Modules, CI checks, Known gaps (add a row for each rule or behavior that built code doesn't follow yet, and delete each row the session closed), Rule proposals (each with the step that decides it), Conformance reviews, and Decisions (a row for each decision the session made).
5. **Rewrite Current work** from scratch: the next step, and the decisions that wait on the owner. Never add to the old text.
6. **Run `just ci`,** fix what fails, and commit.
7. **Report to the owner:** what the session changed, what comes next, and what waits on them.
