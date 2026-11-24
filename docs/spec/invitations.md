# Spec: `invitations`

## Purpose

`invitations` lets a member who holds `invitations.create` invite someone by email, and lets the invitee see the invitation, and accept or decline it. A workspace's members read its invitations, and those who may invite change an invitation's role, send it again and cancel it. An invitation expires after a number of days that the deployment sets, and an hourly sweep deletes it then.

## Owns

| Table | Holds |
|---|---|
| `invitations` | `id`, `workspace_id`, `email` (normalized), `role`, `inviter_id`, `created_at`, `expires_at`, `updated_at`, `version`. `(workspace_id, email)` is unique, and `email` has an index. `inviter_id` references `users` with `ON DELETE SET NULL`: an invitation outlives the account of the member who sent it, as the workspace's (ACCOUNTS-018). `email` is personal data (DATA-23). |

## Interface

- `Contract`, the personal data contract for `invitations` (D31), whose `export` gives the invitations of each workspace whose only member is the user ([accounts spec](accounts.md#what-an-export-holds)), and whose `erase` changes nothing: the foreign key sets the inviter of an invitation that a deleted user sent to null, and an invitation addressed to their email is the workspace's, and stays (ACCOUNTS-018).

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `POST /v1/workspaces/{workspace_id}/invitations` | `workspace::<InvitationsCreate>` | INVITATIONS-001 to INVITATIONS-003, INVITATIONS-008 to INVITATIONS-010, INVITATIONS-014 |
| `GET /v1/workspaces/{workspace_id}/invitations` | `workspace::<InvitationsRead>` | INVITATIONS-012 |
| `GET /v1/workspaces/{workspace_id}/invitations/{invitation_id}` | `workspace::<InvitationsRead>` | INVITATIONS-011 |
| `PATCH /v1/workspaces/{workspace_id}/invitations/{invitation_id}` | `workspace::<InvitationsUpdate>` | INVITATIONS-013, INVITATIONS-014 |
| `POST /v1/workspaces/{workspace_id}/invitations/{invitation_id}/resend` | `workspace::<InvitationsCreate>` | INVITATIONS-009, INVITATIONS-010, INVITATIONS-014, INVITATIONS-015 |
| `DELETE /v1/workspaces/{workspace_id}/invitations/{invitation_id}` | `workspace::<InvitationsCancel>` | INVITATIONS-014, INVITATIONS-016 |
| `GET /v1/users/me/invitations` | user | INVITATIONS-004 |
| `POST /v1/users/me/invitations/{invitation_id}/accept` | user | INVITATIONS-005, INVITATIONS-006 |
| `POST /v1/users/me/invitations/{invitation_id}/decline` | user | INVITATIONS-017 |

`POST .../invitations` accepts an `Idempotency-Key` (API-12).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| INVITATIONS-001 | Inviting an email with a role creates a pending invitation that expires after `INVITATION_EXPIRY_DAYS` (1 to 90, default 7). It answers 201 with `Location`, `/v1/workspaces/{workspace_id}/invitations/{id}`, the invitation's `ETag` (PLATFORM-071), and `{id, workspace_id, email, role, inviter_id, created_at, expires_at}`. It records `invitation.created`, publishes the workspace event `invitation.created`, which requires `invitations.read`, and enqueues the invitation email (INVITATIONS-010), in the same unit of work; with email off, the invitation is created all the same (PLATFORM-022). The role must rank below the inviter's, or the request answers 403 `request.permission_denied` (ACCESS-004). | SEC-09, DATA-22, D1, API-04 | `invitations_001_` |
| INVITATIONS-002 | Inviting an email that belongs to a member of the workspace answers 409 `invitations.already_member`, and writes no invitation, audit entry, event or job. | ERR-03 | `invitations_002_` |
| INVITATIONS-003 | Inviting an email that already has an invitation to this workspace, expired or not, refreshes it in place, when the role it offers ranks below the inviter's (INVITATIONS-014): the role, inviter and expiry are replaced, and the ID is kept. It answers 200 with the invitation, as INVITATIONS-001 shows it, and its new `ETag`, records `invitation.updated`, publishes the workspace event `invitation.updated`, which requires `invitations.read`, and enqueues the email again. | DATA-22, API-04 | `invitations_003_` |
| INVITATIONS-004 | `GET /v1/users/me/invitations` lists the caller's unexpired invitations in every workspace, with `{id, workspace_id, workspace_name, inviter_name, role, expires_at}`, `inviter_name` being null once the inviter's account is deleted, the latest created first, in cursor pages. Its `search` matches the workspace's name. It works when the caller belongs to no workspace. | D34, DATA-16, API-06, API-20 | `invitations_004_` |
| INVITATIONS-005 | Accepting adds the caller to the workspace with the invitation's role, and deletes the invitation, in one unit of work. It answers 200 with `{workspace_id, role}`. It records `invitation.accepted` in the workspace's log, publishes the workspace event `member.joined`, which requires `members.read`, and publishes the user event `membership.created`. | D34, DATA-22 | `invitations_005_` |
| INVITATIONS-006 | Accepting an expired invitation answers 422 `invitations.expired`. Accepting while already a member answers 409 `invitations.already_member` and changes nothing. An invitation not addressed to the caller answers 404 `invitations.not_found`, and so does one whose workspace went, with its owner's account or by its owner's deletion of it, while the acceptance waited (ACCESS-008, ACCESS-010). | D16, D34, DATA-16 | `invitations_006_` |
| INVITATIONS-007 | The periodic job `invitations.expire` deletes expired invitations every hour, across all workspaces. | D13, DATA-16 | `invitations_007_` |
| INVITATIONS-008 | When the invited email belongs to an account, the new or refreshed invitation also publishes the user event `invitation.received` to that user. | D34, SEC-19 | `invitations_008_` |
| INVITATIONS-009 | Each member who may invite does so, or sends an invitation again, at most `INVITATION_LIMIT` times together (1 to 100,000, default 100) per `INVITATION_WINDOW_SECONDS` (1 to 86,400, default 3,600), which bounds the emails a member's invitations send. Beyond that, the request answers 429 `request.rate_limited`, and writes no invitation, audit entry, event or job. Every request to invite whose fields are valid, and every request to send an invitation again, is counted as its service starts, whatever it then answers. One answered before, such as one refused for its fields, its body or its `Idempotency-Key` (PLATFORM-037), or by a limit of PLATFORM-054, writes no invitation, audit entry, event or job, and only PLATFORM-054's limits count it. | SEC-13, API-15, PLATFORM-011 | `invitations_009_` |
| INVITATIONS-010 | The invitation email names the inviter, the workspace, the role and the days until the invitation expires, and tells the invitee to sign in, or create an account, with the address it was sent to. It has a plain-text part, and an HTML part whose values are escaped. | D1, D26, D34 | `invitations_010_` |
| INVITATIONS-011 | Reading an invitation of the workspace, `GET .../invitations/{invitation_id}`, answers 200 with it, as INVITATIONS-001 shows it, `inviter_id` being null once the inviter's account is deleted, and its `ETag`, whether it has expired or not, until the sweep deletes it (INVITATIONS-007). An ID that names no invitation of the workspace answers 404 `invitations.not_found`. | API-13, DATA-16, ACCOUNTS-018 | `invitations_011_` |
| INVITATIONS-012 | Listing the workspace's invitations, `GET .../invitations`, gives every invitation of the workspace, as INVITATIONS-011 shows it, those that have expired among them until the sweep deletes them, the latest created first, in cursor pages. Its `search` matches the invitation's email. | DATA-16, DATA-17, API-06, API-20 | `invitations_012_` |
| INVITATIONS-013 | Changing an invitation's role, `PATCH .../invitations/{invitation_id}` with the merge patch `{"role": <role>}`, expired or not, stores the role, and answers 200 with the invitation, as INVITATIONS-011 shows it, and its new `ETag`; its inviter and its expiry stay, and no email is sent (D164). It records `invitation.role_changed`, with `{"from", "to"}`, publishes the workspace event `invitation.updated`, and, when an account holds the invitation's email, the user event `invitation.revised` to that user. A role that the invitation offers already answers 200 and changes nothing. An `If-Match` that names none of the invitation's versions answers 412 `request.precondition_failed`, and changes nothing. | API-13, DATA-18, DATA-22, SEC-19 | `invitations_013_` |
| INVITATIONS-014 | A member refreshes, changes, sends again or cancels only an invitation whose role ranks below their own, and changes it only to a role below their own, as they invite only at such a role (INVITATIONS-001). A refusal goes through `authz::refuse`, and answers 403 `request.permission_denied`. | SEC-09, ACCESS-004, PLATFORM-010, D165 | `invitations_014_` |
| INVITATIONS-015 | Sending an invitation again, `POST .../invitations/{invitation_id}/resend` with no body, expired or not, makes the caller its inviter, makes it expire `INVITATION_EXPIRY_DAYS` from then, and enqueues its email again (INVITATIONS-010), keeping its role and its ID. It answers 200 with the invitation, as INVITATIONS-011 shows it, and its new `ETag`, records `invitation.resent`, publishes the workspace event `invitation.updated`, and, when an account holds the invitation's email, the user event `invitation.received` to that user. | DATA-22, D1, SEC-13, SEC-19 | `invitations_015_` |
| INVITATIONS-016 | Cancelling an invitation, `DELETE .../invitations/{invitation_id}`, expired or not, deletes it and answers 204. It records `invitation.cancelled`, with the account that holds the invitation's email as its target when one does, publishes the workspace event `invitation.cancelled`, which requires `invitations.read`, and, to that account, the user event `invitation.withdrawn`. | API-04, DATA-22, SEC-19 | `invitations_016_` |
| INVITATIONS-017 | Declining an invitation addressed to the caller, `POST /v1/users/me/invitations/{invitation_id}/decline` with no body, expired or not, deletes it and answers 204. It records `invitation.declined` in the workspace's log, with the caller as its actor, and publishes the workspace event `invitation.declined`, which requires `invitations.read`. An invitation not addressed to the caller answers 404 `invitations.not_found`, and so does one whose workspace went, with its owner's account or by its owner's deletion of it, while the decline waited (ACCESS-008, ACCESS-010). | D6, D34, DATA-16, DATA-20 | `invitations_017_` |

## Design

### Who an invitation reaches

An invitation is addressed to an email, not to a user. It reaches whichever account holds the address when the invitee lists or accepts, whether or not one did when they were invited. With email on, `auth` serves an account only once it has proved its address (AUTH-025), so only the address's owner sees its invitations; with email off, an address is what its account claims ([auth spec](auth.md#email-verification), D67).

### Inviting

1. The service counts the request in the caller's bucket of INVITATIONS-009, in a statement that commits by itself, outside any transaction, before the unit of work opens (PLATFORM-011). Its key is a hash-based message authentication code (HMAC) of the caller's user ID (PLATFORM-057).
2. The service opens the unit of work with the `Authorize<InvitationsCreate>` the handler handed it, which authorizes the caller inside it (platform spec, Unit of work), and refuses a role that isn't one of those `access::offerable` gives the caller, through `authz::refuse` (INVITATIONS-014). `access` decides the rank, as it decides every role (SEC-08).
3. The invitation is inserted or refreshed by `(workspace_id, email)`, expiring `INVITATION_EXPIRY_DAYS` after `now()`. The statement locks the row that it refreshes, or keeps as it was, until the unit of work ends. The refresh's `WHERE` keeps it to an invitation that offers one of those roles, so one at another role, such as the owner's invitation of an admin, which an admin invites again, stays as it was, and the request is refused through `authz::refuse` (INVITATIONS-014), once step 4 has found the invitee no member.
4. `accounts::find_by_email_for_key_share` looks up the invitee's account, and holds its row: an account that a deletion took while the read waited is none, so the entry's target and the user event's user are there when their rows are written (DATA-20). It comes after the invitation's row, the order in which a change, a sending again and a cancellation take the two (Changing, sending again and cancelling), and in which an acceptance and a decline, which hold the invitation before they write a row that names the invitee, do. If there is an account, `access::is_member` decides INVITATIONS-002. An acceptance adds its invitee while it holds the invitation's row (Accepting), so this read sees the member of an acceptance that committed first, and an acceptance that reaches the row later waits for it until the unit of work ends (DATA-20).
5. The audit entry is recorded, the workspace event is published, and `Mail::send` enqueues the invitation email (The invitation email) under the proof's scope, the workspace and the inviter, with the inviter's name from `accounts::profiles`, the workspace's from `workspaces::names`, and the setting's days. If the invitee has an account, `invitation.received` is published to them (INVITATIONS-008). Then the unit of work commits.

The created invitation's `Location` is `/v1/workspaces/{workspace_id}/invitations/{id}`, the route that reads it (INVITATIONS-011). Its `ETag`, and that of every later answer that shows it, is the row's `version`, which a refresh, a change of its role and its sending again each move. The result that an idempotency key stores holds the version with the invitation, so a replay answers the same `ETag` (PLATFORM-036).

### The invitation email

`emails.rs` declares it, with its templates in `crates/app/templates/invitations/` ([platform spec](platform.md#sending-email)). Its plain-text part reads:

> Subject: Bob Jones invited you to Acme on AntWorker
>
> Bob Jones invited you to join the workspace Acme on AntWorker, as a member. The invitation expires in 7 days.
>
> To accept it, sign in to AntWorker with alice@example.com, or create an account with that address.

### Reading and listing a workspace's invitations

Both are workspace routes, so `Authorized<InvitationsRead>` proves the caller's membership, and gives the workspace's ID, by which each statement filters (DATA-16). A read takes the row with its `version`, which the `ETag` names. A list reads a page by `(workspace_id, created_at, id)`, through the index that an export reads too, and a search matches the row's own `email` in the same statement, with `ILIKE` under `pg_unicode_fast` (PLATFORM-047), so it needs no other module's batch. Neither filters on `expires_at`: the members who may send an invitation again see the ones that expired, until the sweep deletes them, and a client tells them apart by `expires_at`.

An invitation's address is personal data, which every member reads, as each member reads the members' addresses (`members.read`): `invitations.read` is every role's, so each member sees whom the workspace has invited.

### Changing, sending again and cancelling

Each is a workspace route, which opens its unit of work with the proof that authorizes the caller (platform spec, Unit of work), and acts on the invitation that its path names in the proof's workspace (DATA-16).

1. Sending again counts the request in the caller's bucket of INVITATIONS-009 first, before the unit of work opens, as inviting does (PLATFORM-011), so the emails a member's invitations send have one bound. A change refuses a role that isn't one of those `access::offerable` gives the caller before it reads anything (INVITATIONS-014).
2. The invitation is read `FOR UPDATE`, so a change, a sending again, a cancellation, a refresh, an acceptance and a decline of it take turns (DATA-20), and an ID that names no invitation of the workspace answers 404. An invitation whose role isn't one of those `access::offerable` gives the caller is refused through `authz::refuse` (INVITATIONS-014): the invitation offers its role, so acting on it is acting at that rank, as SEC-09 limits acting on a member.
3. A change checks the versions that `If-Match` names against the row it holds (DATA-18), answers a role that the invitation offers already as unchanged, and otherwise updates the row, keeping its `WHERE` to those versions. Sending again sets the inviter to the caller and the expiry to `INVITATION_EXPIRY_DAYS` after `now()`, as a refresh does. A cancellation deletes the row.
4. `accounts::find_by_email_for_key_share` looks up the account that holds the invitation's address, if any, and holds its row, as inviting does (Who an invitation reaches): the entry's target, and the user its event goes to.
5. The entry is recorded under the proof's scope, the workspace event is published, and the user event too when an account holds the address. Sending again enqueues the email as inviting does (Inviting), with the caller as its inviter. Then the unit of work commits.

A change's entry holds the two roles, as a member's change of role does, since a role is no personal data (SEC-18). The workspace events carry the workspace's and the invitation's IDs, and the user events the same as `invitation.received`, so a client reads the invitation again over REST (API-19). The address isn't in any of them.

The proof locks the caller's membership `FOR SHARE` before the invitation ([access spec](access.md#policy)). Each deletion of the workspace, whoever makes it, locks or ends the caller's membership before it deletes the workspace with its invitations ([access spec](access.md#leaving-transferring-and-dissolving)), so it waits for the caller's operation to commit, and holds nothing that the operation still needs.

### Listing the caller's invitations

The query filters on the caller's email, from `accounts::profile`, and on `expires_at` against `now()` (DATA-16). Workspace names come from `workspaces::names`, and inviter names from `accounts::profiles`, each passed only the IDs on the page. A search first reads the workspace IDs of every one of the caller's unexpired invitations, and `workspaces::matching` keeps those whose name contains it, so the page holds only their invitations.

### Accepting

1. The unit of work opens from the caller's `Principal`.
2. It reads the invitation's workspace by the invitation's ID and the caller's email (DATA-16), without a lock.
3. `access::hold_owner` locks the workspace's owner's membership, which a deletion of the owner's account, or of the workspace, locks before it deletes the workspace with its invitations, so the acceptance takes the two locks in the deletion's order (DATA-20); a workspace with no owner left went with that deletion, which answers 404 `invitations.not_found` (INVITATIONS-006, ACCESS-008, ACCESS-010). A transfer locks that membership first too, so an acceptance that waited for one joins under the new owner.
4. It reads the invitation again, `FOR UPDATE`, so two acceptances of it take turns (DATA-20), and checks the expiry in SQL.
5. `access::add_member` adds the caller, or reports that they are a member already, which refuses the acceptance and changes nothing.
6. It deletes the invitation, records the audit entry under `Principal::invitee_scope`, the workspace's log with the caller as its actor, publishes the events, and commits.

A client that lists the workspace's invitations reads them again on `member.joined`, since the invitation it shows went with the acceptance.

### Declining

Declining takes the locks that accepting takes, in its order (DATA-20): it reads the invitation's workspace by the invitation's ID and the caller's email, without a lock, and `access::hold_owner` locks the owner's membership, answering 404 `invitations.not_found` for a workspace that went meanwhile (Accepting). It then deletes the invitation by its ID and the caller's email, which locks it, and answers 404 when it deletes nothing, such as when a cancellation or an acceptance of it committed first. Without the owner's lock first, a decline would hold the invitation while its entry waited for the workspace's row, and a deletion of the workspace would hold that row while its cascade waited for the invitation.

The entry is the workspace's, as an acceptance's is, since the invitation is the workspace's (ACCOUNTS-018), and its members learn from it, as from the event, that the invitation went: `Principal::invitee_scope` names the workspace's log with the caller as its actor, though they are no member of it. The entry names no address (SEC-18).

### Expiry sweep

The periodic job `invitations.expire` runs hourly, and deletes rows whose `expires_at` has passed, in batches of at most 1,000, as the platform's sweeps do, the longest expired first, until a batch deletes none. It spans every workspace by nature, so it selects rows by expiry alone (DATA-16). Its `DELETE` tests the expiry of each row it deletes, besides the batch's subquery, so a row that a resend or a refresh renewed while the statement waited for its lock is tested as it then stands, and stays (DATA-20). Scheduled maintenance isn't audited (DATA-22), and publishes nothing. An expired invitation is already invisible to its invitee until then, since their list checks `expires_at`, and accepting refuses it, while the workspace's members see it, with its `expires_at`, and may send it again or cancel it; so the interval is a constant rather than a setting (D9).

## Open design questions

None.
