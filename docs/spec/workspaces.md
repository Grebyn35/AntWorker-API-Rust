# Spec: `workspaces`

## Purpose

`workspaces` owns the workspace itself and the members endpoints. This spec covers creating a workspace, listing the caller's workspaces, listing members, and removing a member. Renaming, the picture, transfer, leaving, deletion and the setup subcommand arrive with the full module.

## Owns

| Table | Holds |
|---|---|
| `workspaces` | `id`, `name`, `created_at`, `updated_at`, `version`. The database generates the `id` (DATA-08). |

## Interface

- `names(uow, &[WorkspaceId])`: workspace names for a batch of IDs that came from a query filtered by tenant, used by `invitations` (DATA-22).
- The domain type `WorkspaceName` (D25).

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `POST /v1/workspaces` | user | WORKSPACES-001, WORKSPACES-002 |
| `GET /v1/workspaces` | user | WORKSPACES-003 |
| `GET /v1/workspaces/{workspace_id}/members` | `workspace::<MembersRead>` | WORKSPACES-004 |
| `DELETE /v1/workspaces/{workspace_id}/members/{user_id}` | `workspace::<MembersRemove>` | WORKSPACES-005, WORKSPACES-006 |

`POST /v1/workspaces` accepts an `Idempotency-Key` (API-12).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| WORKSPACES-001 | Creating a workspace makes the caller its owner, in one unit of work. It answers 201 with `Location` and `{id, name, role: "owner", created_at}`. It records `workspace.created` in the workspace's log, and publishes the user event `membership.created` to the caller's other devices. | D33, API-04, DATA-27 | `workspaces_001_` |
| WORKSPACES-002 | A workspace name is trimmed, has internal whitespace collapsed to single spaces, is normalized to Unicode Normalization Form C (NFC), and must be 1 to 255 characters and not only whitespace. | D25, API-10 | `workspaces_002_` |
| WORKSPACES-003 | `GET /v1/workspaces` lists every workspace the caller belongs to, with `{id, name, role, joined_at}`, newest first, in cursor pages. | D3, API-05, API-06 | `workspaces_003_` |
| WORKSPACES-004 | `GET .../members` lists the workspace's members with `{user_id, name, email, role, joined_at}`, newest first, in cursor pages. | API-05, API-06, DATA-22 | `workspaces_004_` |
| WORKSPACES-005 | Removing a member deletes their membership and answers 204. It records `member.removed` in the workspace's log, with the removed user as the target, and publishes the workspace event `member.removed`, which requires `members.read`. It also sends an access-change signal that closes or re-filters the removed user's streams. | SEC-06, SEC-08, DATA-27, D35 | `workspaces_005_` |
| WORKSPACES-006 | A caller can't remove themselves (422 `workspaces.cannot_remove_self`), and can't remove a member of equal or higher rank (403 `request.permission_denied`, PLATFORM-025), which also protects the owner. A user ID that isn't a member answers 404 `workspaces.member_not_found`. | SEC-08, ERR-03 | `workspaces_006_` |

## Design

### Creating a workspace

One operation creates every workspace, inside the unit of work of whatever asks for it: it inserts the `workspaces` row, calls `access::add_owner`, records `workspace.created`, and publishes `membership.created` (D33). Two optional parts call it (ARCH-07), and the ones a fork keeps, with or without `invitations`, give its product its shape ([architecture §5](../architecture.md#workspace-shapes)):
- **Users creating workspaces.** `POST /v1/workspaces` opens the unit of work from the caller's `Principal`, calls the operation, and commits (WORKSPACES-001).
- **The setup subcommand.** In one unit of work, it creates the deployment's one workspace with the name it is given, and its owner's account from an email and a name, through `accounts::create`. The account has no password. The subcommand enqueues an email that tells the owner the account exists, and carries no secret. The owner signs in for the first time through password reset in a client: the reset code sent to their address sets the first password, and verifies the address. Their next step is inviting everyone else (INVITATIONS-001). When an account with that email already exists, the subcommand makes it the owner if its email is verified, and otherwise refuses, so nobody can take the workspace by registering the owner's address first. When any workspace already exists, it refuses and changes nothing, and two runs at the same time create at most one. It prints nothing secret.

The test support creates workspaces through the operation, never through one of the parts, so deleting either part leaves every other test passing.

### Listing members

`access::members` returns the memberships, filtered by the workspace ID. `accounts::profiles` returns their names and emails in one batch call, given exactly those user IDs (ARCH-06, DATA-22).

## Open design questions

| Question | Settled by |
|---|---|
| Which actor the setup subcommand's audit entries record, since the subcommand runs with no `Principal` (AUDIT-002) | the spec of the full `workspaces` module |
| How the owner sets their first password in a deployment with email turned off (MAIL-004), where password reset can't send its code | the spec of the full `auth` module, with password reset |
| How the owner signs in to the account the setup subcommand created, in a fork that signs in only through OAuth providers | the spec of the full `auth` module, with identity linking |
