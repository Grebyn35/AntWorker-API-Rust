# Spec: `access`

## Purpose

`access` decides what a workspace member may do. It owns memberships, defines the roles and their permissions, and implements the `AccessPolicy` that `platform::authz` asks for every workspace request (D2). It has no routes of its own.

## Owns

| Table | Holds |
|---|---|
| `workspace_memberships` | One row per member of a workspace: `id`, `workspace_id`, `user_id`, `role`, `created_at`, when the member joined, `updated_at` and `version`. `(workspace_id, user_id)` is unique. A member's role can change, so the table has an `id` rather than a composite key (DATA-02), which breaks a tie between memberships that share a joining time in a list's order (API-06). |

## Interface

- The permission types used with `Authorized<P>`, one type per permission of the matrix below that a route requires.
- `AccessPolicy`, implemented for `platform::authz`: `authorize`, and from step 2.4 on `memberships`, which returns the caller's `Memberships`.
- `add_owner(uow, workspace_id, user_id)`: makes the creator the owner of a new workspace. Called only by `workspaces`.
- `add_member(uow, workspace_id, user_id, role)`: adds a member. Called only by `invitations`, after accepting.
- `is_member(uow, workspace_id, user_id)`: whether the user belongs to the workspace.
- `remove(uow, &Authorized<MembersRemove>, user_id)`: removes a member, subject to rank.
- `members(uow, &Authorized<MembersRead>, &MemberFilter, &Page)`: a page of the workspace's memberships, newest first, kept to a role and to a set of users when the filter names them.
- `member_ids(uow, &Authorized<MembersRead>)`: the user IDs of every member of the workspace, which a search of the members hands `accounts` (Design: Searching members).
- `joined(uow, &Principal, Option<&[WorkspaceId]>, &Page)`: a page of the caller's own memberships, the latest joined first, kept to the given workspaces when a search named them, for listing the caller's workspaces.
- `workspace_ids(uow, &Principal)`: the ID of every workspace the caller belongs to (ACCESS-005), which a search of the caller's workspaces hands `workspaces`. `AccessPolicy::memberships`, with the caller's role in each, arrives with the hub that needs it, in step 2.4.
- `Role::outranks(role)`: compares two roles (SEC-08).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| ACCESS-001 | Each membership holds exactly one role: `owner`, `admin` or `member`. Each workspace has exactly one owner. | D2 | `access_001_` |
| ACCESS-002 | A role grants exactly the permissions the matrix gives it, and nothing else. | D2 | `access_002_` |
| ACCESS-003 | A caller who isn't a member of the workspace in the path gets 404 `request.not_found`. A member without the route's permission gets 403 `request.permission_denied`. Both refusals are reported to the `SecurityEventSink`. A workspace ID that isn't a UUID names no workspace, and answers 404 `request.not_found` unrecorded. | API-14, SEC-07, DATA-27 | `access_003_` |
| ACCESS-004 | A member acts only on members whose role ranks below their own, and assigns only roles below their own. A refusal goes through `authz::refuse`, and answers 403 `request.permission_denied`. | SEC-08, DATA-27, PLATFORM-025 | `access_004_` |
| ACCESS-005 | `workspace_ids` returns the ID of every workspace the caller belongs to, and nothing else. | D3, DATA-22 | `access_005_` |

## Design

### Roles and permissions

Rank, from highest to lowest: `owner`, `admin`, `member`.

| Permission | Allows | `owner` | `admin` | `member` |
|---|---|:-:|:-:|:-:|
| `workspace.read` | Reading the workspace | yes | yes | yes |
| `members.read` | Listing members, with their names, emails and roles | yes | yes | yes |
| `members.remove` | Removing a member of lower rank | yes | yes | |
| `invitations.create` | Inviting at a role below one's own | yes | yes | |

The matrix lives in `access/permissions.rs` as one table, so adding a permission is one type and one row (D2). Later modules add their permissions to this table.

### Removing a member

The delete itself checks the rank: it deletes the membership only where its role ranks below the caller's (DATA-24, DATA-25). A row whose role doesn't qualify is skipped without being locked, so a removal never waits on a member it may not remove, and two members who each try to remove the other can't wait on each other: at most one outranks the other. When nothing was deleted, a read without a lock tells a member it outranked nobody from a user who isn't a member (ACCESS-004, WORKSPACES-006).

### Uniqueness of the owner

A partial unique index on `workspace_memberships (workspace_id) WHERE role = 'owner'` makes a second owner impossible. Ownership transfer, built with the full `workspaces` module, swaps two roles inside one unit of work.

### Policy

`AccessPolicy::authorize` runs inside the request's unit of work (PLATFORM-024). It reads the caller's membership row by workspace ID and user ID, `FOR SHARE`, and answers not a member, a member without the permission, or the member's role (platform spec, "Refusals"). `Authorized<P>` carries the role by its name, which `access` reads back into its `Role`, so `platform` holds no roles of its own (ARCH-02).

### Searching members

The members' names and emails belong to `accounts`, and their memberships to `access`, so no one query joins them (ARCH-06). A search takes three: `member_ids` reads the workspace's members, `accounts::matching` keeps those whose name or email contains the search, and `members` pages the memberships of the users it kept. Each query filters by tenant itself: the first and the third by the workspace from the proof, and the second by IDs that came from the first (DATA-22). A list without a search skips the first two.

## Open design questions

| Question | Settled by |
|---|---|
| Whether `AccessPolicy` should cache a membership for the length of a request, when one request authorizes twice | `access_003_`, with a request whose route checks two permissions |
