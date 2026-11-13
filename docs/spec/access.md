# Spec: `access`

## Purpose

`access` decides what a workspace member may do. It owns memberships, defines the roles and their permissions, and implements the `AccessPolicy` that `platform::authz` asks for every workspace request (D2). It has no routes of its own.

## Owns

| Table | Holds |
|---|---|
| `workspace_memberships` | One row per member of a workspace: `id`, `workspace_id`, `user_id`, `role`, `joined_xact`, the ID of the transaction that added the member, `created_at`, when the member joined, `updated_at` and `version`. `(workspace_id, user_id)` is unique. A member's role can change, so the table has an `id` rather than a composite key (DATA-02), which breaks a tie between memberships that share a joining time in a list's order (API-06). |

## Interface

- The permission types used with `Authorized<P>`, one type per permission of the matrix below that a route requires.
- `AccessPolicy`, implemented for `platform::authz`: `authorize`, and `memberships`, which gives each of a set of users their `Memberships`, for the hub's streams (ACCESS-007).
- `add_owner(uow, workspace_id, user_id)`: makes the creator the owner of a new workspace. Called only by `workspaces`.
- `hold_owner(uow, workspace_id)`: locks the workspace's owner's membership `FOR KEY SHARE`, and answers whether the workspace is still there (ACCESS-008). Called only by `invitations`, as accepting's first lock. <!-- part: invitations -->
- `add_member(uow, workspace_id, user_id, role)`: adds a member, or reports that the user is one already, which changes nothing. Called only by `invitations`, after accepting, once `hold_owner` has locked the owner's membership. <!-- part: invitations -->
- `is_member(uow, workspace_id, user_id)`: whether the user belongs to the workspace. <!-- part: invitations -->
- `remove(uow, &Authorized<MembersRemove>, user_id)`: removes a member, subject to rank.
- `members(uow, &Authorized<MembersRead>, &MemberFilter, &Page)`: a page of the workspace's memberships, newest first, kept to a role and to a set of users when the filter names them.
- `member_ids(uow, &Authorized<MembersRead>)`: the user IDs of every member of the workspace, which a search of the members hands `accounts` (Design: Searching members).
- `joined(uow, &Principal, Option<&[WorkspaceId]>, &Page)`: a page of the caller's own memberships, the latest joined first, kept to the given workspaces when a search named them, for listing the caller's workspaces.
- `workspace_ids(uow, &Principal)`: the ID of every workspace the caller belongs to (ACCESS-005), which a search of the caller's workspaces hands `workspaces`, and in each of which `accounts` publishes a change of the caller's record (ACCOUNTS-007).
- `erasable(uow, &Principal)`: locks every membership of the caller, and answers whether a workspace they own has another member (ACCESS-008). Called only by `accounts`, as the first lock of deleting the caller's account after the user's own row.
- `held(uow, &Principal)`: every membership of the caller, with its workspace and role, which the personal data contract of `workspaces` reads once `erasable` has locked them.
- `leave(uow, &Principal, workspace_id)`: ends the caller's membership in the workspace, with its signal (ACCESS-006), which the personal data contract of `workspaces` calls for each workspace the caller belongs to, as their account is deleted.
- `workspace_ids_of(uow, UserId)`: the same, of the user whose job the caller runs, in each of whose workspaces `accounts` publishes that a provider's picture became their avatar (ACCOUNTS-028). <!-- part: files -->
- `sole_workspaces(uow, UserId)`: the IDs of the workspaces whose only member is the user, read by their own memberships, whose content their export holds (D40, ACCOUNTS-024).
- `Contract`, the personal data contract for `workspace_memberships` (D31), which exports the user's memberships ([accounts spec](accounts.md#what-an-export-holds)), and deletes them, each with its access-change signal.
- `Role::outranks(role)`: compares two roles (SEC-09).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| ACCESS-001 | Each membership holds exactly one role: `owner`, `admin` or `member`. Each workspace has exactly one owner. | D2 | `access_001_` |
| ACCESS-002 | A role grants exactly the permissions the matrix gives it, and nothing else. | D2 | `access_002_` |
| ACCESS-003 | A caller who isn't a member of the workspace in the path gets 404 `request.not_found`. A member without the route's permission gets 403 `request.permission_denied`. Both refusals are reported to the `SecurityEventSink`. A workspace ID that isn't a UUID names no workspace, and answers 404 `request.not_found` unrecorded. | API-14, SEC-08, DATA-22 | `access_003_` |
| ACCESS-004 | A member acts only on members whose role ranks below their own, and assigns only roles below their own. A refusal goes through `authz::refuse`, and answers 403 `request.permission_denied`. | SEC-09, DATA-22, PLATFORM-010 | `access_004_` |
| ACCESS-005 | `workspace_ids` returns the ID of every workspace the caller belongs to, and nothing else. | D3, DATA-16 | `access_005_` |
| ACCESS-006 | Adding a membership and removing one each sends an access-change signal for its user and workspace in the same unit of work, which, once it commits, closes the streams of that user that PLATFORM-031 says a membership change closes. | SEC-06, D35, PLATFORM-031 | `access_006_` |
| ACCESS-007 | `memberships` gives each user it is asked about every workspace they belong to, with the permissions their role holds there and the transaction that added the membership, and nothing else. | SEC-19, DATA-16 | `access_007_` |
| ACCESS-008 | `erasable` locks every membership of the user, `FOR UPDATE`, and answers whether a workspace they own has another member. Until the unit of work that called it ends, no one else joins a workspace whose owner's membership it locked, and a join that waited for it, once that unit of work deleted the workspace, adds no member; and no operation of the user's in any of their workspaces runs. | D16, DATA-20 | `access_008_` |

## Design

### Roles and permissions

Rank, from highest to lowest: `owner`, `admin`, `member`.

| Permission | Allows | `owner` | `admin` | `member` |
|---|---|:-:|:-:|:-:|
| `workspace.read` | Reading the workspace, and receiving its events about itself, such as `workspace.renamed` | yes | yes | yes |
| `workspace.update` | Renaming the workspace | yes | yes | |
| `members.read` | Listing members, with their names, emails and roles | yes | yes | yes |
| `members.remove` | Removing a member of lower rank | yes | yes | |
| `invitations.create` | Inviting at a role below one's own | yes | yes | <!-- part: invitations --> |
| `files.read` | Listing, reading and downloading the workspace's files, and receiving their events | yes | yes | yes <!-- part: files --> |
| `files.upload` | Uploading a file, private or public | yes | yes | yes <!-- part: files --> |
| `files.delete` | Deleting a file | yes | yes | <!-- part: files --> |

The matrix lives in `access/permissions.rs` as one table, so adding a permission is one type and one row (D2). Later modules add their permissions to this table.

### Removing a member

The delete itself checks the rank: it deletes the membership only where its role ranks below the caller's (DATA-18, DATA-20). A row whose role doesn't qualify is skipped without being locked, so a removal never waits on a member it may not remove, and two members who each try to remove the other can't wait on each other: at most one outranks the other. When nothing was deleted, a read without a lock tells a member it outranked nobody from a user who isn't a member (ACCESS-004, WORKSPACES-006).

### Signals

The functions that add or remove a membership send its signal themselves, so no caller can change who belongs to a workspace without the hub hearing of it. Those that add one also call `UnitOfWork::order_with_events`, so the membership commits in the order of the commits that publish, and each event's snapshot tells whether the membership had committed before it (the platform spec's "A member's events"). A role's change, with the full `workspaces` module, signals the same way (SEC-06).

### Erasure

`erasable` locks the user's memberships, in the order of their workspaces' IDs, then counts, for each workspace the user owns, the members who aren't them (ACCESS-008). The user's own operations in their workspaces lock their membership first, `FOR SHARE` (Policy), so none runs while the deletion holds it.
Joining a workspace inserts a membership, whose foreign keys lock the workspace's row and the joiner's, not the owner's membership, so accepting an invitation first locks the workspace's owner's membership `FOR KEY SHARE`, through `hold_owner`, before it locks the invitation: a join and a deletion of the owner's account then take turns, taking the two in the same order, and a deletion that finds the owner alone deletes a workspace nobody is joining. Every workspace has an owner (ACCESS-001), so a join that finds no owner's membership, once it has waited, finds the workspace gone with its owner's account, and adds nothing. <!-- part: invitations -->
The contract of `workspaces` ends each membership of the user through `leave`, which signals it, as removing a member does (ACCESS-006), before it deletes a workspace the user owned. This module's contract then deletes any membership of the user that remains, signalling each: none, after that.

### Memberships for the hub

`AccessPolicy::memberships` reads the memberships of the users it is given, filtered by their IDs, which came from the hub's own streams' principals (DATA-16), through the index on the members' user, with the transaction that added each, and turns each role into the names of its permissions from the matrix, so `platform`, which holds no roles, filters events by permission names alone (ARCH-02).

### Uniqueness of the owner

A partial unique index on `workspace_memberships (workspace_id) WHERE role = 'owner'` makes a second owner impossible. Ownership transfer, built with the full `workspaces` module, swaps two roles inside one unit of work.

### Policy

`AccessPolicy::authorize` runs inside the request's unit of work (PLATFORM-009). It reads the caller's membership row by workspace ID and user ID, `FOR SHARE`, and answers not a member, a member without the permission, or the member's role (platform spec, "Refusals"). `Authorized<P>` carries the role by its name, which `access` reads back into its `Role`, so `platform` holds no roles of its own (ARCH-02).

### Searching members

The members' names and emails belong to `accounts`, and their memberships to `access`, so no one query joins them (ARCH-06). A search takes three: `member_ids` reads the workspace's members, `accounts::matching` keeps those whose name or email contains the search, and `members` pages the memberships of the users it kept. Each query filters by tenant itself: the first and the third by the workspace from the proof, and the second by IDs that came from the first (DATA-16). A list without a search skips the first two.

## Open design questions

| Question | Settled by |
|---|---|
| Whether `AccessPolicy` should cache a membership for the length of a request, when one request authorizes twice | `access_003_`, with a request whose route checks two permissions |
