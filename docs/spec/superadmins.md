# Spec: `superadmins`

## Purpose

`superadmins` holds the deployment's superadmins (D62): the users whom its operator made one, through the binary's subcommand `superadmin`, which nothing in the API does, or, in development, through `seed` (SYSTEM-010). A superadmin sees every workspace's name, creation time, member count and owner, deletes any workspace, and creates workspaces, which they then own, through routes of their own, and never sees a workspace's content as a superadmin. This spec covers the subcommand, what authorizes a superadmin's routes, and the route that tells a caller whether they are one. The routes themselves are `workspaces`', which owns what they read ([workspaces spec](workspaces.md#a-superadmins-routes)).

## Owns

| Table | Holds |
|---|---|
| `superadmins` | One row for each superadmin: `id`, `user_id`, which is unique, and `created_at`, when the operator made the user one. `user_id` references `users` with `ON DELETE CASCADE`. Rows are never edited, and `app` may not update them (DATA-19). |

## Interface

- `Policy`, the `SuperadminPolicy` that `platform::authz` asks for every route whose access is `superadmin` (SUPERADMINS-004, SUPERADMINS-005).
- `Change`, what the subcommand `superadmin` does with the account that holds the address it is given, and `change`, which runs it and answers why it changed nothing, as a `Refusal`, when it did (SUPERADMINS-001 to SUPERADMINS-003).
- `listed`, which reads the deployment's superadmins for the subcommand's `list`, each as a `Listed`, with the address of their account (SUPERADMINS-008).
- `Contract`, the personal data contract for `superadmins` (D31), which exports when the user became a superadmin, and whose erasure the foreign key carries out (SUPERADMINS-007).

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `GET /v1/users/me/superadmin` | user | SUPERADMINS-006 |

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| SUPERADMINS-001 | The subcommand `superadmin add`, given an email address, makes the account that holds it a superadmin, in one unit of work. It records `superadmin.added` in the account's own log, with no actor and no client, and the user as its resource. It logs what it did, without the address, and exits 0. | D62, ARCH-14, DATA-22, OBS-04 | `superadmins_001_` |
| SUPERADMINS-002 | `superadmin add` changes nothing when what it is given isn't an email address, when no account holds the address, when the account that holds it is a superadmin already, or, with email on, when that account hasn't proved the address. It then ends as a failure does (OPS-03), exiting 1, with a message that says which, without the address. With email off, the account that holds the address becomes a superadmin whether or not it proved it. | D62, D67, OPS-03, OBS-04, ERR-05 | `superadmins_002_` |
| SUPERADMINS-003 | The subcommand `superadmin remove`, given an email address, ends the superadmin of the account that holds it, in one unit of work: once it has committed, a route whose access is `superadmin` refuses the account as SUPERADMINS-004 says. It records `superadmin.removed` in the account's own log, as SUPERADMINS-001 records its entry, logs what it did, and exits 0. What isn't an address, an address that no account holds, and an account that is no superadmin change nothing, and end the subcommand as SUPERADMINS-002 says. | D62, ARCH-14, DATA-22, OPS-03, OBS-04 | `superadmins_003_` |
| SUPERADMINS-004 | A route whose access is `superadmin` serves a caller only while they are a superadmin, which it reads inside the request's unit of work. Every other caller gets 403 `request.permission_denied`, whatever they own or belong to, and the refusal is recorded as `access.denied` in the caller's own log, with them as its actor, the deployment as its resource, which has no ID, and `{"access": "superadmin"}` as its details. A request without a credential answers 401 `request.unauthenticated` first. | D62, API-14, DATA-22, SEC-08 | `superadmins_004_` |
| SUPERADMINS-005 | A superadmin whose account has no authenticator app gets 403 `request.second_factor_setup_required` from every route whose access is `superadmin`, whatever `SECOND_FACTOR_REQUIRED` says, until they have set one up (AUTH-056). Nothing records it. | D63, D154 | `superadmins_005_` |
| SUPERADMINS-006 | `GET /v1/users/me/superadmin` answers 200 with `{"superadmin": <boolean>}`: whether the caller is a superadmin. It asks no authenticator app of a superadmin, so one with an app and one without both read `true`: it refuses an account without one only as any route of a user's does, while the deployment requires one of every user (AUTH-070). | D62, API-02, API-14, DATA-16 | `superadmins_006_` |
| SUPERADMINS-007 | Deleting a superadmin's account ends their being one, with the account. The export of a user's data holds `superadmins/superadmins.json`: for a superadmin, one record with the `created_at` of their row, and for any other user, none. | D31, DATA-23 | `superadmins_007_` |
| SUPERADMINS-008 | The subcommand `superadmin list` shows the operator who runs it the deployment's superadmins: it prints to its standard output a line for each, the one made first first, with the account's email address, the user's ID and when the operator made them a superadmin, under a line that says what they are, or, for a deployment without one, a line that says so. It changes nothing and records nothing, logs how many it listed, without an address, and exits 0. | D157, ARCH-14, OPS-03, OBS-04, DATA-16 | `superadmins_008_` |

## Design

### Who is a superadmin

A superadmin is a user with a row in `superadmins`. It is not a role: a role belongs to a membership, and a superadmin needs none ([architecture's glossary](../architecture.md#11-glossary)). Only the subcommand makes a superadmin, and, in development, `seed`, through the subcommand's operation (SYSTEM-010), so only someone who reaches the deployment's server does: no route does, whoever calls it (D62). The subcommand ends one too, and so does the deletion of their account, which takes the row with it (SUPERADMINS-007).

### The subcommand

An operator runs `superadmin add <email>`, `superadmin remove <email>` or `superadmin list` on the deployment's server, with the image of the color that serves, as `just superadmin` runs it over Secure Shell (SSH) ([deploy](../operations/deploy.md#superadmins)). It acts for no request, no job and no user, so it opens its unit of work through `Database::for_operator`, and the entry of a change records no actor and no client, under `AuditScope::attempt`, in the account's own log ([platform spec](platform.md#unit-of-work)). Its log says what it did, and never the address (OBS-04).

It reads which account holds the address through `accounts::holder_for_update`, which locks the user's row and reads it again under the lock, so a deletion of the account, or a change of its address, commits before the run reads it or after the run commits (DATA-20). With email on, an account becomes a superadmin only once it has proved the address, so nobody becomes one by registering an operator's address before its owner does. With email off, nothing proves an address, and the account that holds it is the one the operator means (D67).

`add` inserts the row, and an account that has one already inserts nothing, which the run reports (ERR-05). `remove` deletes it. A request that a superadmin's route authorized before the removal committed runs to its end, and does what it could have done a moment before the removal: nothing that the removal's order with it could leave wrong, so the proof takes no lock.

Neither sends an email: the operator who runs the subcommand knows whom they named, and the account's own log holds the entry.

`list` reads every row of `superadmins`, the one made first first, and each user's address through `accounts::profiles`, by exactly those users' IDs, in one unit of work that changes nothing (DATA-16). An operator knows the people of a deployment by their addresses, as `add` and `remove` take them, so the list shows each superadmin's address, with the user's ID, which the logs and the audit entries name them by, and when they became one. An address is personal data, which no log holds (OBS-04): the subcommand hands the lines to the binary's `main.rs`, which prints them to the standard output, the answer to the operator who asked, outside the logging setup, so no log line and no telemetry holds them, and its log says only how many it listed (OPS-03). The one-off container that runs the subcommand goes when it ends, with what Docker kept of its output ([deploy](../operations/deploy.md#superadmins)).

### What authorizes a superadmin's routes

`Routes::superadmin` registers a route whose access is `superadmin`, and whose handler extracts `AuthorizeSuperadmin` ([platform spec](platform.md#refusals)). Its `open` opens the request's unit of work and asks `Policy` inside it, which answers one of three things:

- The caller is no superadmin. `platform::authz` rolls the unit of work back, reports the refusal to the `SecurityEventSink`, which records it in a transaction of its own, and answers 403 (SUPERADMINS-004). A superadmin's routes are in the API's document, so a 404 would hide nothing.
- The caller is a superadmin whose account has no authenticator app, which `auth::has_authenticator_app` reads. The request answers 403 `request.second_factor_setup_required`, the answer a client already handles for a deployment that requires a second factor of every user, by having the user set one up (AUTH-070).
- The caller is a superadmin with an app. `open` returns the unit of work with the proof, `Superadmin`, which only it builds, and which holds the caller: what a superadmin changes is recorded under the scope of their `Principal`, their own log with them as its actor, and an operation that needs a proof of who the session's user is reads their session (WORKSPACES-030). Its `owner_scope` names the log of a workspace that the superadmin just created, with them as its actor, which only that creation may call, as clippy enforces (WORKSPACES-031).

Every superadmin must have a second factor (D63), and the proof is where that holds (D154): whatever happened to the account since the operator made it a superadmin, such as the removal of its app, by its user or by an operator (AUTH-064, AUTH-072), no route serves it as a superadmin without one. So the subcommand takes an account without an app, which sets one up before its first request as a superadmin, and no removal of an app needs to know of superadmins.

A function that exists only for a superadmin's routes takes the proof, down to the function of its query, as `access::census` and `access::dissolve_any` do, so the compiler keeps any other caller from what only a superadmin may do: a read across workspaces, and the first reach of the workspace that a path names, which locks its owner's membership (DATA-16). From there such an operation goes on through the functions that it shares with a member's, which take the workspace's ID, as a deletion's last steps do (WORKSPACES-030).

### Whether the caller is a superadmin

A client offers a superadmin's pages only to a superadmin, so it asks: `GET /v1/users/me/superadmin` is a route of a user's, which reads the caller's own row, by their ID (DATA-16), and answers every user that such a route serves, so asking is no refused request, and records nothing. The capabilities tell a client whether its API has the module at all ([system spec](system.md#capabilities)).

## Open design questions

None.
