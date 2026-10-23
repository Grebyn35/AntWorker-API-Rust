# Module specs

One file per module. A spec states what the module guarantees, and how it is built where that isn't obvious from the code. The [standards](../standards/README.md) state the rules every module follows. A spec never restates a rule; it cites the rule's ID.

A spec is written just before its module is built, and kept current with the code from then on (DOC-01).

## Sections

A spec has these sections, in this order:

1. **Purpose.** What the module is for, in two or three sentences.
2. **Owns.** The tables the module owns (ARCH-06).
3. **Interface.** What other modules may use: types, functions and the traits it implements.
4. **Routes.** Only in a spec whose module serves routes. Each route's method and path, its access level (API-14), and the behaviors it serves. The generated OpenAPI document describes each route in full (API-17).
5. **Behaviors.** One entry per behavior, each with:
   - an ID, using the module's prefix (for example `WORKSPACES-005`)
   - one statement a test can prove
   - the rule IDs or decision IDs it satisfies
   - the test prefix, such as `workspaces_005_`, that every test proving it starts with (TEST-01)
6. **Design.** How the behaviors are met, where the code alone wouldn't make it clear. The tests named in Behaviors verify it. If this section and the code disagree, the code and its tests win, and this section is corrected.
7. **Open design questions.** Questions only building will settle, each with the test that will settle it. When a question is settled, it moves into Design.

## IDs

A behavior's ID is the module name in capitals plus a three-digit number, such as `AUTH-004` or `INVITATIONS-002`. IDs are never reused. A spec covers the behaviors that have been built or are about to be built, and grows as its module does.

## Specs

| Module | Spec | Status |
|---|---|---|
| `platform` | [platform.md](platform.md) | written for the first slice; the database area is built |
| `accounts` | [accounts.md](accounts.md) | written for the first slice, not yet built |
| `auth` | [auth.md](auth.md) | written for the first slice, not yet built |
| `access` | [access.md](access.md) | written for the first slice, not yet built |
| `workspaces` | [workspaces.md](workspaces.md) | written for the first slice, not yet built |
| `invitations` | [invitations.md](invitations.md) | written for the first slice, not yet built |
| `audit` | [audit.md](audit.md) | written for the first slice, not yet built |
| `realtime` | [realtime.md](realtime.md) | written for the first slice, not yet built |
| `mail` | [mail.md](mail.md) | written for the first slice, not yet built |
