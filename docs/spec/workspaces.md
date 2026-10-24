# Spec: `workspaces`

## Purpose

`workspaces` owns the workspace itself and the members endpoints. This spec covers creating a workspace, listing the caller's workspaces, listing members, and removing a member. Renaming, the picture, transfer, leaving and deletion arrive with the full module.

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

One unit of work, opened from the caller's `Principal`, inserts the `workspaces` row, calls `access::add_owner`, records the audit entry, publishes the event, and commits.

### Listing members

`access::members` returns the memberships, filtered by the workspace ID. `accounts::profiles` returns their names and emails in one batch call, given exactly those user IDs (ARCH-06, DATA-22).

## Open design questions

None.
