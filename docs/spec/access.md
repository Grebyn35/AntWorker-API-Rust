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
- `add_owner(uow, workspace_id, user_id)`: makes a user the owner of a new workspace, and signals the change of their access. Called only by `workspaces`, in the one operation that creates a workspace.
- `hold_owner(uow, workspace_id)`: locks the membership of the workspace's owner `FOR KEY SHARE`, whoever owns it once the lock is taken, and answers whether the workspace is still there (ACCESS-008, ACCESS-010). Called only by `invitations`, as accepting's first lock. <!-- part: invitations -->
- `add_member(uow, workspace_id, user_id, role)`: adds a member, or reports that the user is one already, which changes nothing. Called only by `invitations`, after accepting, once `hold_owner` has locked the owner's membership. <!-- part: invitations -->
- `is_member(uow, workspace_id, user_id)`: whether the user belongs to the workspace. <!-- part: invitations -->
- `remove(uow, &Authorized<MembersRemove>, user_id)`: removes a member, subject to rank.
- `change_role(uow, &Authorized<MembersUpdate>, user_id, role, versions)`: changes a member's role, subject to rank, and to the versions that an `If-Match` names (ACCESS-004, DATA-18).
- `leave(uow, &Authorized<WorkspaceLeave>)`: ends the caller's membership, with its signal.
- `transfer(uow, &Authorized<WorkspaceTransfer>, user_id)`: makes an admin the owner, and the caller an admin (SEC-09).
- `dissolve(uow, &Authorized<WorkspaceDelete>)`: ends every membership of the workspace, each with its signal, before `workspaces` deletes it.
- `members(uow, &Authorized<MembersRead>, &MemberFilter, &Page)`: a page of the workspace's memberships, newest first, kept to a role and to a set of users when the filter names them.
- `member(uow, &Authorized<MembersRead>, user_id)`: one member's membership, with its version, which their `ETag` names.
- `member_ids(uow, &Authorized<MembersRead>)`: the user IDs of every member of the workspace, which a search of the members hands `accounts` (Design: Searching members).
- `joined(uow, &Principal, Option<&[WorkspaceId]>, &Page)`: a page of the caller's own memberships, the latest joined first, kept to the given workspaces when a search named them, for listing the caller's workspaces.
- `workspace_ids(uow, &Principal)`: the ID of every workspace the caller belongs to (ACCESS-005), which a search of the caller's workspaces hands `workspaces`, and in each of which `accounts` publishes a change of the caller's record (ACCOUNTS-007).
- `erasable(uow, &Principal)`: locks every membership of the caller, and answers whether a workspace they own has another member (ACCESS-008). Called only by `accounts`, as the first lock of deleting the caller's account after the user's own row.
- `held(uow, &Principal)`: every membership of the caller, with its workspace and role, which the personal data contract of `workspaces` reads once `erasable` has locked them.
- `end_membership(uow, &Principal, workspace_id)`: ends the caller's membership in the workspace, with its signal (ACCESS-006), which the personal data contract of `workspaces` calls for each workspace the caller belongs to, as their account is deleted.
- `workspace_ids_of(uow, UserId)`: the ID of every workspace the user belongs to, as `workspace_ids` gives a caller's (ACCESS-005), for a job that runs for the user with no `Principal`: in each of them `accounts` publishes that a provider's picture became the user's avatar (ACCOUNTS-028). <!-- part: files -->
- `sole_workspaces(uow, UserId)`: the IDs of the workspaces whose only member is the user, read by their own memberships, whose content their export holds (D40, ACCOUNTS-024).
- `Contract`, the personal data contract for `workspace_memberships` (D31), which exports the user's memberships ([accounts spec](accounts.md#what-an-export-holds)), and deletes them, each with its access-change signal.
- `Role::outranks(role)`: compares two roles (SEC-09).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| ACCESS-001 | Each membership holds exactly one role: `owner`, `admin` or `member`. Each workspace has exactly one owner. | D2 | `access_001_` |
| ACCESS-002 | A role grants exactly the permissions the matrix gives it, and nothing else. | D2 | `access_002_` |
| ACCESS-003 | A caller who isn't a member of the workspace in the path gets 404 `request.not_found`. A member without the route's permission gets 403 `request.permission_denied`. Both refusals are reported to the `SecurityEventSink`. A workspace ID that isn't a UUID names no workspace, and answers 404 `request.not_found` unrecorded. | API-14, SEC-08, DATA-22 | `access_003_` |
| ACCESS-004 | A member acts only on members whose role ranks below their own, and assigns only roles below their own, but for the three exceptions that SEC-09 names: a transfer, by which the owner and an admin swap their roles; leaving, by which a member who isn't the owner ends their own membership; and a workspace's deletion, by which the owner ends every membership, their own among them. A refusal goes through `authz::refuse`, and answers 403 `request.permission_denied`. | SEC-09, DATA-22, PLATFORM-010 | `access_004_` |
| ACCESS-005 | `workspace_ids` returns the ID of every workspace the caller belongs to, and nothing else. | D3, DATA-16 | `access_005_` |
| ACCESS-006 | Adding a membership, removing one and changing one's role each send an access-change signal for its user and workspace in the same unit of work, which, once it commits, closes the streams of that user that PLATFORM-031 says a membership change closes. | SEC-06, D35, PLATFORM-031 | `access_006_` |
| ACCESS-007 | `memberships` gives each user it is asked about every workspace they belong to, with the permissions their role holds there and the transaction that added the membership, and nothing else. | SEC-19, DATA-16 | `access_007_` |
| ACCESS-008 | `erasable` locks every membership of the user, `FOR UPDATE`, and answers whether a workspace they own has another member. Until the unit of work that called it ends, no one else joins a workspace whose owner's membership it locked, and a join that waited for it, once that unit of work deleted the workspace, adds no member; and no operation of the user's in any of their workspaces runs. | D16, DATA-20 | `access_008_` |
| ACCESS-009 | Leaving, a transfer and a workspace's deletion each hold their caller's membership `FOR UPDATE` from the moment they are authorized. So of two such requests of one member sent at once, the second waits for the first, and answers as a request sent after it does: 404 `request.not_found` once the membership is gone, and 403 `request.permission_denied` once its role lacks the permission. | DATA-20, SEC-09 | `access_009_` |
| ACCESS-010 | A join waits for a transfer, and for a deletion, of the workspace it joins, as it waits for a deletion of the owner's account (ACCESS-008): after a transfer it joins, and after a deletion it adds no member. <!-- part: invitations --> | DATA-20, D16 | `access_010_` |

## Design

### Roles and permissions

Rank, from highest to lowest: `owner`, `admin`, `member`.

| Permission | Allows | `owner` | `admin` | `member` |
|---|---|:-:|:-:|:-:|
| `workspace.read` | Reading the workspace, and receiving its events about itself, such as `workspace.updated` | yes | yes | yes |
| `workspace.update` | Changing what the workspace's members read of it, such as its name | yes | yes | |
| `workspace.leave` | Leaving the workspace | | yes | yes |
| `workspace.transfer` | Transferring the workspace to an admin | yes | | |
| `workspace.delete` | Deleting the workspace, with everything in it | yes | | |
| `members.read` | Listing members, with their names, emails and roles | yes | yes | yes |
| `members.update` | Changing the role of a member of lower rank, to a role below one's own | yes | yes | |
| `members.remove` | Removing a member of lower rank | yes | yes | |
| `invitations.create` | Inviting at a role below one's own | yes | yes | <!-- part: invitations --> |
| `files.read` | Listing, reading and downloading the workspace's files, and receiving their events | yes | yes | yes <!-- part: files --> |
| `files.upload` | Uploading a file, private or public | yes | yes | yes <!-- part: files --> |
| `files.delete` | Deleting a file | yes | yes | <!-- part: files --> |

The matrix lives in `access/permissions.rs` as one table, so adding a permission is one type and one row (D2). Later modules add their permissions to this table. A permission whose operation changes or ends its caller's own membership is also an entry of `OWN_MEMBERSHIP` there, which the policy reads its lock from (Policy). A permission is what a role may do, so a higher rank doesn't hold every permission of a lower one: the owner alone can't leave, since a workspace keeps its owner (ACCESS-001, SEC-09).

### Removing a member

The delete itself checks the rank: it deletes the membership only where its role ranks below the caller's (DATA-18, DATA-20). A row whose role doesn't qualify is skipped without being locked, so a removal never waits on a member it may not remove, and two members who each try to remove the other can't wait on each other: at most one outranks the other. When nothing was deleted, a read without a lock tells a member it outranked nobody from a user who isn't a member (ACCESS-004, WORKSPACES-006).

### Changing a role

`change_role` refuses a role that doesn't rank below the caller's before it reads anything (ACCESS-004). It then locks the member's row, `FOR UPDATE`, only where its role ranks below the caller's, so a change, as a removal, never waits on a member it may not change, and a read without a lock tells a member the caller doesn't outrank from a user who isn't a member. Holding the row, it checks the versions that the request's `If-Match` names (DATA-18), answers a role that the member holds already as unchanged, and otherwise updates the row, keeping its `WHERE` to those versions, and signals the change. With the template's three roles, an admin outranks members alone, and assigns `member` alone, so only the owner's change moves a role; the rule, not the roles, decides it, so a fork that adds a role between two of them changes no code here.

### Leaving, transferring and dissolving

Three operations change or end their caller's own membership: leaving, a transfer, and a workspace's deletion. The policy reads that membership `FOR UPDATE` for them (Policy), so each holds it from its first statement.

- `leave` deletes the caller's membership.
- `transfer` locks the membership of the user it is given, `FOR UPDATE`, only where its role is `admin`, and a read without a lock tells a member who isn't an admin, the caller among them, from a user who isn't a member. It then makes the caller an admin and that member the owner, in that order (Uniqueness of the owner), and signals both.
- `dissolve` deletes the workspace's memberships from the highest rank to the lowest, in a statement for each role, and signals each. A member's operation locks its caller's membership first, and after it only memberships that rank below (ACCESS-004). So a deletion that reaches a member's row before every row below it waits for that member's operation while it holds nothing the operation still needs. One statement over every role would lock the rows in the order it found them, and could hold a member's row while it waited for the admin who is removing that member, who would wait for it in turn (DATA-20).

### Signals

The functions that add, remove or change a membership send its signal themselves, so no caller can change who belongs to a workspace, or at which role, without the hub hearing of it (SEC-06). Those that add one also call `UnitOfWork::order_with_events`, so the membership commits in the order of the commits that publish, and each event's snapshot tells whether the membership had committed before it (the platform spec's "A member's events").

### Erasure

`erasable` locks the user's memberships, in the order of their workspaces' IDs, then counts, for each workspace the user owns, the members who aren't them (ACCESS-008). The user's own operations in their workspaces lock their membership first (Policy), so none runs while the deletion holds it.
Joining a workspace inserts a membership, whose foreign keys lock the workspace's row and the joiner's, not the owner's membership, so accepting an invitation first locks the workspace's owner's membership `FOR KEY SHARE`, through `hold_owner`, before it locks the invitation: a join and a deletion of the owner's account then take turns, taking the two in the same order, and a deletion that finds the owner alone deletes a workspace nobody is joining. A transfer and a deletion of the workspace hold the owner's membership `FOR UPDATE` first too (Policy), so a join takes turns with each (ACCESS-010). A join that waited for a transfer finds the membership it waited for an admin's, and locks none: `hold_owner` then reads whether the workspace has an owner, and locks that one's, so a join holds the membership of whoever owns the workspace as it commits. Every workspace has an owner (ACCESS-001), so a join that finds none, once it has waited, finds the workspace gone, with its owner's account or by its owner's deletion of it, and adds nothing. <!-- part: invitations -->
The contract of `workspaces` ends each membership of the user through `end_membership`, which signals it, as removing a member does (ACCESS-006), before it deletes a workspace the user owned. This module's contract then deletes any membership of the user that remains, signalling each: none, after that.

### Memberships for the hub

`AccessPolicy::memberships` reads the memberships of the users it is given, filtered by their IDs, which came from the hub's own streams' principals (DATA-16), through the index on the members' user, with the transaction that added each, and turns each role into the names of its permissions from the matrix, so `platform`, which holds no roles, filters events by permission names alone (ARCH-02).

### Uniqueness of the owner

A partial unique index on `workspace_memberships (workspace_id) WHERE role = 'owner'` makes a second owner impossible. A transfer swaps two roles inside one unit of work, in two statements: Postgres checks the index as each row changes, so the owner becomes an admin first, and the admin then becomes the owner.

### Policy

`AccessPolicy::authorize` runs inside the request's unit of work (PLATFORM-009). It reads the caller's membership row by workspace ID and user ID, `FOR SHARE`, and answers not a member, a member without the permission, or the member's role (platform spec, "Refusals"). For the permission of an operation that changes or ends that membership, `workspace.leave`, `workspace.transfer` or `workspace.delete`, it reads the row `FOR UPDATE` instead: two such requests that each held it `FOR SHARE` would each wait for the other before changing it, so the second waits at its first statement, and is authorized against what the first left (ACCESS-009, DATA-20). `Authorized<P>` carries the role by its name, which `access` reads back into its `Role`, so `platform` holds no roles of its own (ARCH-02).

### Searching members

The members' names and emails belong to `accounts`, and their memberships to `access`, so no one query joins them (ARCH-06). A search takes three: `member_ids` reads the workspace's members, `accounts::matching` keeps those whose name or email contains the search, and `members` pages the memberships of the users it kept. Each query filters by tenant itself: the first and the third by the workspace from the proof, and the second by IDs that came from the first (DATA-16). A list without a search skips the first two.

## Open design questions

| Question | Settled by |
|---|---|
| Whether `AccessPolicy` should cache a membership for the length of a request, when one request authorizes twice | `access_003_`, with a request whose route checks two permissions |
