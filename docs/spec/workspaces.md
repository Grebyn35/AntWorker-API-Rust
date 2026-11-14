# Spec: `workspaces`

## Purpose

`workspaces` owns the workspace itself and the members endpoints. This spec covers creating a workspace, listing the caller's workspaces, reading, renaming, transferring and deleting one, listing members, reading one, changing a member's role, removing a member, leaving, and what deleting a member's account does to their workspaces (Erasure). The setup subcommand arrives with the full module.
A workspace's owner and admins also set and remove its picture here, whose image `files` keeps (D117, D149). <!-- part: files -->

## Owns

| Table | Holds |
|---|---|
| `workspaces` | `id`, `name`, `created_at`, `updated_at`, `version`. The database generates the `id` (DATA-08). |

## Interface

- `names(uow, &[WorkspaceId])`: workspace names for a batch of IDs that came from a query filtered by tenant, used by `invitations` (DATA-16). <!-- part: invitations -->
- `matching(uow, &[WorkspaceId], &Search)`: the IDs among a batch, from a query filtered by tenant, of the workspaces whose name contains the search, used by `invitations` (API-20). <!-- part: invitations -->
- The domain type `WorkspaceName` (D25), which `invitations` uses. <!-- part: invitations -->
- `Contract`, the personal data contract for `workspaces` (D31), which exports the workspaces whose only member is the user ([accounts spec](accounts.md#what-an-export-holds)), and deletes the workspaces whose only member is a deleted user, and records and publishes their leaving the others (ACCOUNTS-014).

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `POST /v1/workspaces` | user | WORKSPACES-001, WORKSPACES-002 |
| `GET /v1/workspaces` | user | WORKSPACES-003 |
| `GET /v1/workspaces/{workspace_id}` | `workspace::<WorkspaceRead>` | WORKSPACES-007 |
| `PATCH /v1/workspaces/{workspace_id}` | `workspace::<WorkspaceUpdate>` | WORKSPACES-002, WORKSPACES-008 |
| `DELETE /v1/workspaces/{workspace_id}` | `workspace::<WorkspaceDelete>` | WORKSPACES-017, WORKSPACES-018 |
| `POST /v1/workspaces/{workspace_id}/transfer` | `workspace::<WorkspaceTransfer>` | WORKSPACES-015, WORKSPACES-016, WORKSPACES-018 |
| `POST /v1/workspaces/{workspace_id}/leave` | `workspace::<WorkspaceLeave>` | WORKSPACES-014 |
| `GET /v1/workspaces/{workspace_id}/members` | `workspace::<MembersRead>` | WORKSPACES-004 |
| `GET /v1/workspaces/{workspace_id}/members/{user_id}` | `workspace::<MembersRead>` | WORKSPACES-011 |
| `PATCH /v1/workspaces/{workspace_id}/members/{user_id}` | `workspace::<MembersUpdate>` | WORKSPACES-012, WORKSPACES-013 |
| `DELETE /v1/workspaces/{workspace_id}/members/{user_id}` | `workspace::<MembersRemove>` | WORKSPACES-005, WORKSPACES-006 |
| `PUT /v1/workspaces/{workspace_id}/picture` | `workspace::<WorkspaceUpdate>` | WORKSPACES-019, WORKSPACES-020 <!-- part: files --> |
| `DELETE /v1/workspaces/{workspace_id}/picture` | `workspace::<WorkspaceUpdate>` | WORKSPACES-021 <!-- part: files --> |

`POST /v1/workspaces` accepts an `Idempotency-Key` (API-12), and each `PATCH` an `If-Match` (API-13, PLATFORM-071).
`PUT` and `DELETE .../picture` accept an `If-Match` too, since a change of the picture changes the workspace, its `version` with it. <!-- part: files -->
`PUT .../picture` is an upload route, added through `Routes::workspace_upload`, whose body is the image's bytes, which it reads through `Upload` with the picture's limit (WORKSPACES-020, PLATFORM-074). <!-- part: files -->

## Behaviors

ID 009 is retired.

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| WORKSPACES-001 | Creating a workspace makes the caller its owner, in one unit of work. It answers 201 with `Location`, the workspace's `ETag` (PLATFORM-071), and the workspace as WORKSPACES-007 gives it, whose `role` is `owner`. It records `workspace.created` in the workspace's log, and publishes the user event `membership.created`, which names the session that caused it, so that device can tell it already knows. | D33, API-04, DATA-22 | `workspaces_001_` |
| WORKSPACES-002 | A workspace name is trimmed, has internal whitespace collapsed to single spaces, is normalized to Unicode Normalization Form C (NFC), and must be 1 to 255 characters and not only whitespace. | D25, API-10 | `workspaces_002_` |
| WORKSPACES-003 | `GET /v1/workspaces` lists every workspace the caller belongs to, with `{id, name, role, joined_at}`, the latest joined first, in cursor pages. Its `search` matches the name. | D3, API-05, API-06, API-20 | `workspaces_003_` |
| WORKSPACES-004 | `GET .../members` lists the workspace's members with `{user_id, name, email, role, joined_at}`, the latest joined first, in cursor pages. Its `search` matches the name or the email, and `role` keeps the members of one role. | API-05, API-06, API-20, DATA-16 | `workspaces_004_` |
| WORKSPACES-005 | Removing a member deletes their membership and answers 204. It records `member.removed` in the workspace's log, with the workspace as its resource and the removed user as its target (AUDIT-006), and publishes the workspace event `member.removed`, which requires `members.read`. The removal's access-change signal closes the removed user's streams that PLATFORM-031 says a membership change closes, with `stream.closed` and the reason `access_changed`, before any event that commits after it reaches them (ACCESS-006). | SEC-06, SEC-09, DATA-22, D35 | `workspaces_005_` |
| WORKSPACES-006 | A caller can't remove themselves (422 `workspaces.cannot_remove_self`), and can't remove a member of equal or higher rank (403 `request.permission_denied`, PLATFORM-010), which also protects the owner. A user ID that isn't a member answers 404 `workspaces.member_not_found`. | SEC-09, ERR-03 | `workspaces_006_` |
| WORKSPACES-007 | `GET /v1/workspaces/{workspace_id}` answers 200 with `{id, name, role, created_at}`, `role` being the caller's, and the workspace's `ETag` (PLATFORM-071). | API-04, API-13, D3 | `workspaces_007_` |
| WORKSPACES-008 | Renaming a workspace, `PATCH /v1/workspaces/{workspace_id}` with the merge patch `{"name": <name>}`, stores the name, normalized as WORKSPACES-002 says, and answers 200 with the workspace as WORKSPACES-007 gives it and its new `ETag`. It records `workspace.renamed` in the workspace's log, and publishes the workspace event `workspace.updated`, which requires `workspace.read`. Neither holds the name. The owner and admins may rename; a member gets 403 `request.permission_denied`. | API-03, API-19, SEC-18, SEC-19, DATA-22 | `workspaces_008_` |
| WORKSPACES-010 | A member carries `avatar_url`, as ACCOUNTS-011 gives it, in the list of WORKSPACES-004 and alone, as WORKSPACES-011 reads them. <!-- part: files --> | API-05, D120 | `workspaces_010_` |
| WORKSPACES-011 | `GET .../members/{user_id}` answers 200 with the member, as WORKSPACES-004 lists them, and their `ETag`, the version of their membership (PLATFORM-071). A user ID that isn't a member's answers 404 `workspaces.member_not_found`. | API-04, API-13, DATA-16 | `workspaces_011_` |
| WORKSPACES-012 | Changing a member's role, `PATCH .../members/{user_id}` with the merge patch `{"role": <role>}`, stores the role, and answers 200 with the member as WORKSPACES-011 gives them and their new `ETag`. It records `member.role_changed` in the workspace's log, with the workspace as its resource, the member as its target and `{"from": <role>, "to": <role>}` as its details, and publishes the workspace event `member.role_changed`, with the member and their new role, which requires `members.read`. The change's access-change signal closes the member's streams, as a removal's does (WORKSPACES-005), so each opens again under the new role (ACCESS-006). A role that the member holds already changes nothing: it answers 200 with the member as they stand, and records, publishes and signals nothing. | API-03, SEC-06, SEC-18, SEC-19, DATA-22, D35 | `workspaces_012_` |
| WORKSPACES-013 | A caller changes the role only of a member who ranks below them, and only to a role that ranks below their own. Any other change answers 403 `request.permission_denied` (PLATFORM-010): of their own role, of the owner's, and to a role that doesn't rank below the caller's, such as `owner`, which only a transfer gives (WORKSPACES-015), whichever user's ID the path holds. A path that holds no user's ID, since it isn't a UUID, answers 404 `workspaces.member_not_found` whatever the role (PLATFORM-072), and so does, for a role the caller may assign, the ID of a user who isn't a member. An `If-Match` that names no current version of the membership answers 412 `request.precondition_failed` (PLATFORM-071), and a `role` that isn't one 400 at `/role`. None of them changes anything. A member, who holds no `members.update`, gets 403 `request.permission_denied`. | SEC-09, ERR-03, API-10, API-13, DATA-18 | `workspaces_013_` |
| WORKSPACES-014 | Leaving a workspace, `POST .../leave` with no body, deletes the caller's membership and answers 204. It records `member.left` in the workspace's log, with the workspace as its resource and the caller as its actor and its target, and publishes the workspace event `member.left`, which requires `members.read`. Its access-change signal closes the caller's streams (ACCESS-006). The owner, who holds no `workspace.leave`, gets 403 `request.permission_denied`, and stays: they transfer the workspace first (WORKSPACES-015), or delete it (WORKSPACES-017). | SEC-06, SEC-09, DATA-22, D35, API-04 | `workspaces_014_` |
| WORKSPACES-015 | Transferring a workspace, `POST .../transfer` with `{"user_id": <ID>}`, makes that member, an admin, the owner, and the caller an admin, in one unit of work, and answers 204. It records `workspace.transferred` in the workspace's log, with the workspace as its resource and the new owner as its target, and publishes the workspace event `workspace.transferred`, with the new owner and the owner before, which requires `members.read`. An access-change signal for each of the two closes their streams (ACCESS-006). An admin or a member, who holds no `workspace.transfer`, gets 403 `request.permission_denied`. | SEC-06, SEC-09, DATA-22, D16, D35, API-03, API-04 | `workspaces_015_` |
| WORKSPACES-016 | A transfer to a member who isn't an admin, the caller among them, answers 422 `workspaces.transfer_requires_admin`, one to a user ID that isn't a member's 404 `workspaces.member_not_found`, and one whose `user_id` isn't a UUID in its form of 36 characters 400 at `/user_id`. None of them changes anything. | SEC-09, ERR-03, API-10, API-11 | `workspaces_016_` |
| WORKSPACES-017 | Deleting a workspace, `DELETE /v1/workspaces/{workspace_id}`, deletes it with everything in it, in one unit of work: its memberships, its log, its events, the rows of every module's tables that name it, and the jobs enqueued for it. It answers 204. It records `workspace.deleted` in the caller's own log, with the workspace as its resource, since the workspace's log goes with it. An access-change signal for each member closes their streams (ACCESS-006). An admin or a member, who holds no `workspace.delete`, gets 403 `request.permission_denied`. | SEC-06, DATA-06, DATA-22, DATA-23, D35, API-04 | `workspaces_017_` |
| WORKSPACES-018 | A transfer and a deletion each need the proof that AUTH-023 needs, of the session that authenticates them: without it, each answers 403 `request.reauthentication_required`, and changes nothing. | D123, D148 | `workspaces_018_` |
| WORKSPACES-019 | Setting a workspace's picture, `PUT .../picture` with the image's bytes as the body, stores it as the workspace's picture, in place of any it had, and answers 200 with the workspace as WORKSPACES-007 gives it, whose `picture_url` names the new image, and its new `ETag`. It records `picture.uploaded` in the workspace's log, with the picture as its resource, and publishes `workspace.updated` as WORKSPACES-008 does. The bytes of a picture it replaces stay until FILES-011 deletes them. An `If-Match` that names no current version of the workspace answers 412 `request.precondition_failed` (PLATFORM-071), and changes nothing. The owner and admins may set it: a member gets 403 `request.permission_denied`, and a caller who isn't a member 404 `request.not_found`, each with the body unread (PLATFORM-073). <!-- part: files --> | API-03, API-08, API-13, DATA-21, DATA-22, SEC-18, SEC-19, D149 | `workspaces_019_` |
| WORKSPACES-020 | A workspace's picture is a PNG, JPEG, GIF or WebP image of at most 1 MiB, of the kind whose signature its bytes start with, as files' Accepted kinds checks it. A body of more than 1 MiB answers 413 `request.payload_too_large`, whether its `Content-Length` declares it or it grows past the limit while it is read. An empty body, or one that is no such image, answers 422 `workspaces.picture_not_image`, and one in which the scanner finds malware 422 `workspaces.picture_malware_detected`. While the scanner or the storage doesn't answer, or not within its time, the upload answers 503 `workspaces.picture_unavailable`, with `Retry-After`, unless its route's 60 seconds pass first, when it answers 503 `request.timed_out` (PLATFORM-042). None of these stores anything. <!-- part: files --> | API-08, ERR-03, D63, D149 | `workspaces_020_` |
| WORKSPACES-021 | Removing a workspace's picture, `DELETE .../picture`, answers 204, and when the workspace has one, removes it, records `picture.deleted` in the workspace's log, with the picture as its resource, and publishes `workspace.updated` as WORKSPACES-008 does. The removed image's bytes stay until FILES-011 deletes them. An `If-Match` that names no current version of the workspace answers 412 `request.precondition_failed` (PLATFORM-071). The owner and admins may remove it; a member gets 403 `request.permission_denied`. <!-- part: files --> | API-04, API-13, DATA-22, SEC-18, SEC-19, D149 | `workspaces_021_` |
| WORKSPACES-022 | A workspace carries `picture_url`, the path of the public download of its picture (FILES-027), or null when it has none: as WORKSPACES-007 gives it, and in the list of WORKSPACES-003. <!-- part: files --> | API-05, D149 | `workspaces_022_` |

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
`files::picture_urls` gives the links of the page's workspaces' pictures in one batch call, given exactly those workspaces' IDs (WORKSPACES-022, DATA-16). <!-- part: files -->

### Reading and renaming a workspace

A read and a rename are workspace routes, so `Authorized<P>` proves the caller's membership and gives the workspace's ID, by which both statements filter (DATA-16). The read takes the row's name, creation time and `version`, and the role from the proof. The rename's merge patch requires `name`, the one member of a workspace that a patch edits (D88). The rename is one `UPDATE` of the row, kept to the versions the request's `If-Match` names when it sent one (DATA-18): zero rows updated is the precondition's failure, and the unit of work rolls back with nothing written. Otherwise the shared trigger has raised the row's `version` (DATA-02), and the same unit of work records the entry, publishes the event, reads the workspace as it then stands, which the rename answers, and commits (D1). The update locks the row, so two renames at once apply one after the other, and an `If-Match` sent with the second sees the first (DATA-20).

A name is text a user writes, so it is personal data (the glossary in [architecture](../architecture.md#11-glossary)): the entry's `details` are empty, its resource being the workspace, and the event's payload carries only the workspace's ID (SEC-18, API-19), so a client that receives `workspace.updated` reads the workspace through `GET /v1/workspaces/{workspace_id}`. The event says that what the workspace's members read of it changed, whatever changed, as `user.updated` says of a user's record (ACCOUNTS-007), so a client handles one event for every such change, and a fork that gives a workspace another field publishes the same one.

<!-- part: files -->
### The picture

`files` keeps each workspace's picture: its row in `workspace_pictures`, and its bytes in the bucket under the prefix `pictures/` ([files spec](files.md#workspace-pictures)). `workspaces` owns setting and removing it, its entries in the workspace's log, and its event, since a picture is part of how a workspace appears, as its name is, so both changes reach the members the same way (D149).

1. The handler refuses a body whose `Content-Length` declares more than WORKSPACES-020's limit, then checks that the caller may update the workspace, through `Authorize::check`, which records a refusal as any workspace route does (PLATFORM-073). It then reads the body, in one of the replica's turns, which it holds until the picture is stored (PLATFORM-074). `files::upload_picture` finds its kind from its bytes, has the scanner scan it, and writes its object, outside any transaction (DATA-21).
2. It then runs what `workspaces` gives it, which opens the unit of work with `Authorize<WorkspaceUpdate>`, which authorizes the caller again. Its next statement moves the workspace's `version`, with the versions that `If-Match` names in its `WHERE` (DATA-18): it locks the workspace's row, as a rename does, so two changes of one workspace's picture take turns, and so do a change of its picture and a rename (DATA-20). `files::record_picture` then deletes the workspace's picture row, if any, and inserts the new one. The entry is recorded, the event is published, and the unit of work commits, which counts the upload as stored (FILES-013). A stale `ETag` answers 412 once the object is written, which FILES-011 then deletes.

An object written by an upload whose unit of work never commits, and the object of a replaced or removed picture, are left to FILES-011, as every unreferenced object is. Removing a picture moves the workspace's `version` first too, and its unit of work rolls back when there was none to remove, so a change of the workspace's picture moves its `ETag`, as a rename does, and a removal that removes nothing leaves it. Deleting a workspace deletes its picture's row with it, through the row's foreign key (WORKSPACES-017).

A picture is public, so a client shows it with an `<img>` that sends no credential, wherever the workspace's name shows, such as in the list of the caller's workspaces, and its link never expires. A picture's ID changes with each image, so a client and a shared cache that hold one image never take it for the next. An entry's resource is the picture, by its ID, and its `details` are empty; the event's payload carries the workspace's ID alone, as a rename's does (SEC-18, API-19).

WORKSPACES-020's kinds and limit are an avatar's (ACCOUNTS-009): a list of the caller's workspaces downloads each one's picture, as a member list downloads each member's avatar, so a client shrinks an image to the size it shows before it sends it. The API reads no image's dimensions, since it never decodes one (D109).
<!-- end part: files -->

### Listing members

`access::members` returns a page of the memberships, filtered by the workspace ID, and narrowed by a search as the access spec says ("Searching members"). `accounts::profiles` returns their names and emails in one batch call, given exactly those user IDs (ACCOUNTS-005, ARCH-06, DATA-16).

### Reading a member, and changing a role

A member is a membership, which `access` owns, shown with its user's name and email, which `accounts` owns. `access::member` reads the membership of the user that the path names, in the workspace that the proof names (DATA-16), with its version, and `accounts::profiles` gives the user's profile, as it gives a page's (Listing members). The member's `ETag` is the membership's version: it names what a change of the role edits, and a change of the user's own record, which the member only shows, doesn't move it.

The role's merge patch requires `role`, the one field of a member that a client edits (D88). `access::change_role` decides the change and makes it ([access spec](access.md#changing-a-role)): it refuses a role that doesn't rank below the caller's, locks the membership only where its role ranks below the caller's, checks the versions that the request's `If-Match` names against the row it holds (DATA-18), and updates the row unless it holds the role already. The same unit of work records the entry, whose details hold the two roles, publishes the event, and commits (D1). A role is no personal data, so the entry and the event hold it (SEC-18, API-19), and a client that receives `member.role_changed` changes the member it shows in place.

### Leaving

`access::leave` deletes the caller's membership, which the proof holds locked ([access spec](access.md#policy)), with its signal. The same unit of work records `member.left` under the proof's scope, so the leaver is its actor, with them as its target too, as the entry of a member who left with their account names them (Erasure), and publishes `member.left`. The matrix gives `workspace.leave` to every role but the owner's, so `access` decides who may leave, as it decides every permission (SEC-08), and no workspace is left without its owner (ACCESS-001).

### Transferring a workspace

Once the caller's proof is checked (The proof of a transfer and of a deletion), `access::transfer` swaps the two roles ([access spec](access.md#leaving-transferring-and-dissolving)), and the same unit of work records the entry, publishes the event, and commits. Each of the two members then holds another role, so each gets a signal. The workspace's row doesn't change: a workspace's owner is the member whose role is `owner` (ACCESS-001), which no column of the workspace names, so its `ETag` stays, and a rename and a transfer write different rows: neither undoes the other, whichever commits first.

### Deleting a workspace

Once the caller's proof is checked (The proof of a transfer and of a deletion), the deletion:

1. Records `workspace.deleted` in the caller's own log, under `Principal::audit_scope`. The entry's foreign keys read the caller's row, so it is written before the workspace's row is locked: a change of the caller's own record that holds their row, and publishes in the workspace, then never waits for the deletion while the deletion waits for it (DATA-20).
2. Has `access::dissolve` delete the workspace's memberships, from the highest rank to the lowest, each with its signal ([access spec](access.md#leaving-transferring-and-dissolving)). It waits there for each member's operation in the workspace, which holds that member's membership, and no new one starts.
3. Deletes the workspace's row, which the foreign keys delete everything in with: its log, its events, and the rows of every module's tables that name it.
4. Deletes the jobs enqueued for the workspace, which no foreign key reaches, through `jobs::forget_workspace` (DATA-23): after the row, so that an operation that committed a job for the workspace while step 3 waited for it leaves none behind.

Steps 3 and 4 are the ones that delete a workspace as its only member's account is deleted (Erasure).
The bytes of the workspace's files, and of its picture, stay in the bucket until FILES-011 deletes them, as every object that no row names. <!-- part: files -->

No event tells the members: an event of the workspace would go with it. Each member's streams close with `access_changed`, and their client, which opens a stream again and reads the caller's workspaces, finds the workspace gone, as a removed member's does (REALTIME-005).

### The proof of a transfer and of a deletion

A transfer hands a workspace, with all that its members share, to another account, and a deletion destroys it, each for good. So each needs more than a session, whose tokens a thief may hold: `auth::require_recent_proof` reads, in the operation's unit of work and without a lock, whether the caller's session proved who its user is within the window of the [auth spec's Re-authentication](auth.md#re-authentication) (D148). The check follows the authorization, so a member who may not transfer or delete learns that, rather than being asked for a proof that would change nothing.

### Erasure

The contract runs once `access::erasable` has locked the user's memberships, and found that no workspace they own has another member (ACCOUNTS-012). It reads those memberships through `access::held`, and ends each through `access::end_membership`, with its signal (ACCESS-006). It deletes each workspace the user owned, which the foreign keys delete everything in with: its log, its events, and the rows of every module's tables that name it. The jobs enqueued for it have no foreign key, so `jobs::forget_workspace` deletes them (DATA-23). In each other workspace, it records `member.left` in the workspace's log, with the scope that `Principal::leaver_scope` gives, the workspace as its resource and the user as its target, and publishes the workspace event `member.left`, which requires `members.read`, as `member.removed` does, so the members' clients read the members again (ACCOUNTS-014, API-19). The entry's actor and target become null once the user's row goes (AUDIT-006).

## Open design questions

| Question | Settled by |
|---|---|
| How a patch takes a second editable field: a rename's `name`, and a member's `role`, are each required while they are the only one (D88), and from the first final release API-18 adds only optional request fields, so whether each becomes optional first, with an empty patch specified | the spec of the full `workspaces` module, before the first final release |
| How the setup subcommand opens its unit of work, as which role (DATA-12), and which actor its audit entries record, since it runs with no `Principal` (AUDIT-002), and creates an account without a password, which no route does, so it can't send the API requests as `seed` does ([system spec](system.md#seed)); and with it, the same for the superadmin subcommand ([architecture §9](../architecture.md#9-runtime-topology)) | the spec of the full `workspaces` module |
| How the read of every workspace that `superadmins` asks of this module, which no tenant filters, meets DATA-16 and `check-tenancy` (D62) | the spec of `superadmins`, in step 5 |
| How the owner sets their first password in a deployment with email turned off (PLATFORM-022), where a password reset answers 503 (AUTH-034, D9): such as the subcommand printing, for its operator to hand over, a one-time code that a reset takes in place of an emailed one | the spec of the full `workspaces` module, with the setup subcommand |
| How the owner signs in to the account the setup subcommand created, in a fork that signs in only through OAuth providers: a sign-in through a provider reaches only the account that holds the provider's identity, whatever address the provider gives (AUTH-073), and linking one needs a session (AUTH-087), so the owner of such an account sets a password through a reset first, which such a fork lacks | the spec of the setup subcommand, in step 5 |
