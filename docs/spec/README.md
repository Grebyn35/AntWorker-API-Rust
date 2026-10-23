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
7. **Open design questions.** Questions only building will settle, each with what will settle it: a test, or the spec of the build step that decides it. When a question is settled, its answer moves into Design or Behaviors.

## IDs

A behavior's ID is the module name in capitals plus a three-digit number, such as `AUTH-004` or `INVITATIONS-002`. IDs are never reused. A spec covers the behaviors that have been built or are about to be built, and grows as its module does.

## Specs

| Module | Spec |
|---|---|
| `platform` | [platform.md](platform.md) |
| `system` | [system.md](system.md) |
| `accounts` | [accounts.md](accounts.md) |
| `auth` | [auth.md](auth.md) |
| `access` | [access.md](access.md) |
| `workspaces` | [workspaces.md](workspaces.md) |
| `invitations` | [invitations.md](invitations.md) |
| `audit` | [audit.md](audit.md) |
| `realtime` | [realtime.md](realtime.md) |
| `mail` | [mail.md](mail.md) |
