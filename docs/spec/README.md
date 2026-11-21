# Module specs

One file per module. A spec states what the module guarantees, and how it is built where that isn't obvious from the code. The [standards](../standards/README.md) state the rules every module follows. A spec never restates a rule; it cites the rule's ID.

A spec is written just before its module is built, and kept current with the code from then on (DOC-01).

## Sections

A spec has these sections, in this order:

1. **Purpose.** What the module is for, in two or three sentences.
2. **Owns.** The tables and views the module owns (ARCH-06).
3. **Interface.** What other modules may use: types, functions and the traits it implements. It may also list what the composition root alone takes of the module, such as a personal data contract, the registration of a job or a subcommand's operation, but not what does nothing but wire the module: its routes, their state and its settings.
4. **Routes.** Only in a spec whose module serves routes. Each route's method and path, its access level (API-14), and the behaviors it serves. The generated OpenAPI document describes each route in full (API-17).
   A row cites no behavior of an optional part but the one whose marker the row carries, since a line takes one part's marker: a part's behavior that a core route serves is found in that part's rows of Behaviors. <!-- part: removal -->
5. **Behaviors.** One entry per behavior, each with:
   - an ID, using the module's prefix (for example `WORKSPACES-005`)
   - one statement a test can prove
   - the rule IDs or decision IDs it satisfies
   - the test prefix, such as `workspaces_005_`, that every test proving it starts with (TEST-01)
6. **Design.** How the behaviors are met, where the code alone wouldn't make it clear. The tests named in Behaviors verify it. If this section and the code disagree, the code and its tests win, and this section is corrected.
7. **Open design questions.** Questions only building will settle, each with what will settle it: a test, or the spec of the build step that decides it. When a question is settled, its answer moves into Design or Behaviors.

A section with nothing in it reads "None.", which a sentence that says why may follow, such as "None. No other module uses `system`."

## IDs

A behavior's ID is the module name in capitals plus a three-digit number, such as `AUTH-004` or `WORKSPACES-002`, the next number after the highest the spec names. IDs are never reused: a deleted behavior's number is retired, in a line at the top of its spec's Behaviors, such as "IDs 014 and 015 are retired.", so no later behavior takes it. A spec covers the behaviors that have been built or are about to be built, and grows as its module does.

## Specs

| Module | Spec |
|---|---|
| `platform` | [platform.md](platform.md) |
| `system` | [system.md](system.md) |
| `accounts` | [accounts.md](accounts.md) |
| `auth` | [auth.md](auth.md) |
| `access` | [access.md](access.md) |
| `workspaces` | [workspaces.md](workspaces.md) |
| `invitations` | [invitations.md](invitations.md) <!-- part: invitations --> |
| `superadmins` | [superadmins.md](superadmins.md) <!-- part: superadmins --> |
| `audit` | [audit.md](audit.md) |
| `realtime` | [realtime.md](realtime.md) |
| `files` | [files.md](files.md) <!-- part: files --> |
| `client_telemetry` | [client_telemetry.md](client_telemetry.md) <!-- part: client_telemetry --> |
