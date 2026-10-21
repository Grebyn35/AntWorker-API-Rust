# Spec: `access`

## Purpose

`access` decides what a workspace member may do. It owns memberships, defines the roles and their permissions, and implements the `AccessPolicy` that `platform::authz` asks for every workspace request (D2). It has no routes of its own.

## Owns

| Table | Holds |
|---|---|
| `workspace_memberships` | One row per member of a workspace: `workspace_id`, `user_id`, `role`, `created_at`, `updated_at`, `version`. The primary key is `(workspace_id, user_id)`. |

## Interface

- The permission types used with `Authorized<P>`, one type per permission in the matrix below.
- `AccessPolicy`, implemented for `platform::authz`: `authorize`, and `memberships`, which returns the caller's `Memberships`.
- `add_owner(uow, workspace_id, user_id)`: makes the creator the owner of a new workspace. Called only by `workspaces`.
- `add_member(uow, workspace_id, user_id, role)`: adds a member. Called only by `invitations`, after accepting.
- `is_member(uow, workspace_id, user_id)`: whether the user belongs to the workspace.
- `remove(uow, &Authorized<MembersRemove>, user_id)`: removes a member, subject to rank.
- `members(uow, &Authorized<MembersRead>, page)`: a page of the workspace's memberships.
- `outranks(role, role)`: compares two roles (SEC-08).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| ACCESS-001 | Each membership holds exactly one role: `owner`, `admin` or `member`. Each workspace has exactly one owner. | D2 | `access_001_` |
| ACCESS-002 | A role grants exactly the permissions the matrix gives it, and nothing else. | D2 | `access_002_` |
| ACCESS-003 | A caller who isn't a member of the workspace in the path gets 404 `request.not_found`. A member without the route's permission gets 403 `request.permission_denied`. Both refusals are reported to the `SecurityEventSink`. | API-14, SEC-07, DATA-27 | `access_003_` |
| ACCESS-004 | A member acts only on members whose role ranks below their own, and assigns only roles below their own. A refusal goes through `authz::refuse`, and answers 403 `request.permission_denied`. | SEC-08, DATA-27, PLATFORM-025 | `access_004_` |
| ACCESS-005 | `memberships` returns every workspace the caller belongs to, with their role in each and when they joined, and nothing else. | D3, SEC-17 | `access_005_` |

## Design

### Roles and permissions

Rank, from highest to lowest: `owner`, `admin`, `member`.

| Permission | Allows | `owner` | `admin` | `member` |
|---|---|:-:|:-:|:-:|
| `workspace.read` | Reading the workspace | yes | yes | yes |
| `members.read` | Listing members, with their names, emails and roles | yes | yes | yes |
| `members.remove` | Removing a member of lower rank | yes | yes | |
| `invitations.create` | Inviting at a role below one's own | yes | yes | |

The matrix lives in `access/permissions.rs` as one table, so adding a permission is one variant and one row (D2). Later modules add their permissions to this table.

### Uniqueness of the owner

A partial unique index on `workspace_memberships (workspace_id) WHERE role = 'owner'` makes a second owner impossible. Ownership transfer, built with the full `workspaces` module, swaps two roles inside one unit of work.

### Policy

`AccessPolicy::authorize` runs inside the request's unit of work (PLATFORM-024). It reads the caller's membership row by workspace ID and user ID, and answers not a member, a member without the permission, or the member's role (platform spec, "Refusals").

## Open design questions

| Question | Settled by |
|---|---|
| Whether `AccessPolicy` should cache a membership for the length of a request, when one request authorizes twice | `access_003_`, with a request whose route checks two permissions |
