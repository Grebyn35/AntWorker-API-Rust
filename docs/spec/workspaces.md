# Spec: `workspaces`

## Purpose

`workspaces` owns the workspace itself and the members endpoints. This spec covers creating a workspace, listing the caller's workspaces, reading and renaming one, listing members, and removing a member. Transfer, leaving, deletion and the setup subcommand arrive with the full module.
So does the workspace's picture, which `files` stores (D117). <!-- part: files -->

## Owns

| Table | Holds |
|---|---|
| `workspaces` | `id`, `name`, `created_at`, `updated_at`, `version`. The database generates the `id` (DATA-08). |

## Interface

- `names(uow, &[WorkspaceId])`: workspace names for a batch of IDs that came from a query filtered by tenant, used by `invitations` (DATA-16). <!-- part: invitations -->
- `matching(uow, &[WorkspaceId], &Search)`: the IDs among a batch, from a query filtered by tenant, of the workspaces whose name contains the search, used by `invitations` (API-20). <!-- part: invitations -->
- The domain type `WorkspaceName` (D25), which `invitations` uses. <!-- part: invitations -->

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `POST /v1/workspaces` | user | WORKSPACES-001, WORKSPACES-002 |
| `GET /v1/workspaces` | user | WORKSPACES-003 |
| `GET /v1/workspaces/{workspace_id}` | `workspace::<WorkspaceRead>` | WORKSPACES-007 |
| `PATCH /v1/workspaces/{workspace_id}` | `workspace::<WorkspaceUpdate>` | WORKSPACES-002, WORKSPACES-008 |
| `GET /v1/workspaces/{workspace_id}/members` | `workspace::<MembersRead>` | WORKSPACES-004 |
| `DELETE /v1/workspaces/{workspace_id}/members/{user_id}` | `workspace::<MembersRemove>` | WORKSPACES-005, WORKSPACES-006 |

`POST /v1/workspaces` accepts an `Idempotency-Key` (API-12), and `PATCH /v1/workspaces/{workspace_id}` an `If-Match` (API-13, PLATFORM-071).

## Behaviors

ID 009 is retired.

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| WORKSPACES-001 | Creating a workspace makes the caller its owner, in one unit of work. It answers 201 with `Location`, the workspace's `ETag` (PLATFORM-071), and `{id, name, role: "owner", created_at}`. It records `workspace.created` in the workspace's log, and publishes the user event `membership.created`, which names the session that caused it, so that device can tell it already knows. | D33, API-04, DATA-22 | `workspaces_001_` |
| WORKSPACES-002 | A workspace name is trimmed, has internal whitespace collapsed to single spaces, is normalized to Unicode Normalization Form C (NFC), and must be 1 to 255 characters and not only whitespace. | D25, API-10 | `workspaces_002_` |
| WORKSPACES-003 | `GET /v1/workspaces` lists every workspace the caller belongs to, with `{id, name, role, joined_at}`, the latest joined first, in cursor pages. Its `search` matches the name. | D3, API-05, API-06, API-20 | `workspaces_003_` |
| WORKSPACES-004 | `GET .../members` lists the workspace's members with `{user_id, name, email, role, joined_at}`, the latest joined first, in cursor pages. Its `search` matches the name or the email, and `role` keeps the members of one role. | API-05, API-06, API-20, DATA-16 | `workspaces_004_` |
| WORKSPACES-005 | Removing a member deletes their membership and answers 204. It records `member.removed` in the workspace's log, with the removed user as the target, and publishes the workspace event `member.removed`, which requires `members.read`. The removal's access-change signal closes the removed user's streams that PLATFORM-031 says a membership change closes, with `stream.closed` and the reason `access_changed`, before any event that commits after it reaches them (ACCESS-006). | SEC-06, SEC-09, DATA-22, D35 | `workspaces_005_` |
| WORKSPACES-006 | A caller can't remove themselves (422 `workspaces.cannot_remove_self`), and can't remove a member of equal or higher rank (403 `request.permission_denied`, PLATFORM-010), which also protects the owner. A user ID that isn't a member answers 404 `workspaces.member_not_found`. | SEC-09, ERR-03 | `workspaces_006_` |
| WORKSPACES-007 | `GET /v1/workspaces/{workspace_id}` answers 200 with `{id, name, role, created_at}`, `role` being the caller's, and the workspace's `ETag` (PLATFORM-071). | API-04, API-13, D3 | `workspaces_007_` |
| WORKSPACES-008 | Renaming a workspace, `PATCH /v1/workspaces/{workspace_id}` with the merge patch `{"name": <name>}`, stores the name, normalized as WORKSPACES-002 says, and answers 200 with the workspace as WORKSPACES-007 gives it and its new `ETag`. It records `workspace.renamed` in the workspace's log, and publishes the workspace event `workspace.renamed`, which requires `workspace.read`. Neither holds the name. The owner and admins may rename; a member gets 403 `request.permission_denied`. | API-03, API-19, SEC-18, SEC-19, DATA-22 | `workspaces_008_` |

## Design

### Creating a workspace

One operation creates every workspace, inside the unit of work of whatever asks for it: it inserts the `workspaces` row, calls `access::add_owner`, records `workspace.created` with the scope `Principal::founder_scope` gives, and publishes `membership.created` (D33). Each part that creates workspaces calls it. Those parts are optional (ARCH-07), and the ones a fork keeps give its product its shape ([architecture §5](../architecture.md#workspace-shapes)).

Tests arrange workspaces and memberships through the `owner` connection (TEST-05), never through one of the parts, so removing a part leaves the other modules' tests passing.

### Users creating workspaces

`POST /v1/workspaces` opens the unit of work from the caller's `Principal`, calls the operation, and commits (WORKSPACES-001).

### The setup subcommand

In one unit of work, the subcommand creates a workspace with the name it is given, and its owner's account from an email and a name, through `accounts::create`. An operator runs it once for each workspace the deployment's organization needs, such as one per subsidiary. The account has no password. The subcommand enqueues an email that tells the owner the account exists, and carries no secret. The owner signs in for the first time through password reset in a client: the reset code sent to their address sets the first password, and verifies the address.
Their next step is inviting everyone else (INVITATIONS-001). <!-- part: invitations -->

When an account with that email already exists, the subcommand makes it the owner if its email is verified, or if it has no identity yet, as an account an earlier run created has until its owner sets a password, so one person can own several workspaces before signing in. Otherwise it refuses, so nobody can take a workspace by registering its owner's address first. It records each workspace it creates in `setup_runs`, a table of its own, with the name the run was given, which a unique constraint allows once: a run with a name an earlier run used, even one at the same time, is refused by that constraint and changes nothing, so running it twice by mistake creates one workspace (ERR-05). Deleting a workspace deletes its row with it, so its name can be given again. It prints nothing secret.

### No active workspace

The server stores no active or default workspace for a user (D3). Each request names the workspace it acts in, in its path (API-02), and a client remembers which one it works in, so a user's devices can each work in a different one.

### Listing the caller's workspaces

`access::joined` pages the caller's memberships, filtered by their user ID, through the index on their user and joining time, and `names` gives the page's workspace names (DATA-16, DATA-17). A search first takes the IDs of every workspace the caller belongs to, from `access::workspace_ids` (ACCESS-005), and `matching` keeps those whose name contains it, so `joined` pages only those, as the platform spec's "Lists" says.

### Reading and renaming a workspace

A read and a rename are workspace routes, so `Authorized<P>` proves the caller's membership and gives the workspace's ID, by which both statements filter (DATA-16). The read takes the row's name, creation time and `version`, and the role from the proof. The rename's merge patch requires `name`, the workspace's one editable member (D88). The rename is one `UPDATE` of the row, kept to the versions the request's `If-Match` names when it sent one (DATA-18): zero rows updated is the precondition's failure, and the unit of work rolls back with nothing written. Otherwise the update returns the row as it now stands, whose `version` the shared trigger raised (DATA-02), and the same unit of work records the entry and publishes the event, then commits (D1). The update locks the row, so two renames at once apply one after the other, and an `If-Match` sent with the second sees the first (DATA-20).

A name is text a user writes, so it is personal data (the glossary in [architecture](../architecture.md#11-glossary)): the entry's `details` are empty, its resource being the workspace, and the event's payload carries only the workspace's ID (SEC-18, API-19), so a client that receives `workspace.renamed` reads the name through `GET /v1/workspaces/{workspace_id}`.

### Listing members

`access::members` returns a page of the memberships, filtered by the workspace ID, and narrowed by a search as the access spec says ("Searching members"). `accounts::profiles` returns their names and emails in one batch call, given exactly those user IDs (ACCOUNTS-005, ARCH-06, DATA-16).

## Open design questions

| Question | Settled by |
|---|---|
| How a rename's patch takes a second editable member: `name` is required while it is the only one (D88), and from the first final release API-18 adds only optional request fields, so whether `name` becomes optional first, with an empty patch specified | the spec of the full `workspaces` module, before the first final release |
| How the setup subcommand opens its unit of work, as which role (DATA-12), and which actor its audit entries record, since it runs with no `Principal` (AUDIT-002), and creates an account without a password, which no route does, so it can't send the API requests as `seed` does ([system spec](system.md#seed)); and with it, the same for the superadmin subcommand ([architecture §9](../architecture.md#9-runtime-topology)) | the spec of the full `workspaces` module |
| How the read of every workspace that `superadmins` asks of this module, which no tenant filters, meets DATA-16 and `check-tenancy` (D62) | the spec of `superadmins`, in step 5 |
| How the owner sets their first password in a deployment with email turned off (PLATFORM-022), where password reset can't send its code | the spec of the full `auth` module, with password reset |
| How the owner signs in to the account the setup subcommand created, in a fork that signs in only through OAuth providers | the spec of the full `auth` module, with identity linking |
