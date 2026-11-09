# Spec: `invitations`

## Purpose

`invitations` lets a member invite someone by email, and lets the invitee see and accept the invitation. This spec covers inviting, with its email, the invitee's live notification, listing the caller's own invitations, accepting, and the expiry sweep. Listing a workspace's invitations, updating, resending, cancelling and declining arrive with the full module.

## Owns

| Table | Holds |
|---|---|
| `invitations` | `id`, `workspace_id`, `email` (normalized), `role`, `inviter_id`, `created_at`, `expires_at`, `updated_at`, `version`. `(workspace_id, email)` is unique, and `email` has an index. `inviter_id` references `users` with `ON DELETE SET NULL`: an invitation outlives the account of the member who sent it, as the workspace's (ACCOUNTS-014). `email` is personal data (DATA-23). |

## Interface

- `Contract`, the personal data contract for `invitations` (D31), whose `erase` changes nothing: the foreign key sets the inviter of an invitation that a deleted user sent to null, and an invitation addressed to their email is the workspace's, and stays (ACCOUNTS-014).

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `POST /v1/workspaces/{workspace_id}/invitations` | `workspace::<InvitationsCreate>` | INVITATIONS-001 to INVITATIONS-003, INVITATIONS-008 to INVITATIONS-010 |
| `GET /v1/users/me/invitations` | user | INVITATIONS-004 |
| `POST /v1/users/me/invitations/{invitation_id}/accept` | user | INVITATIONS-005, INVITATIONS-006 |

`POST .../invitations` accepts an `Idempotency-Key` (API-12).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| INVITATIONS-001 | Inviting an email with a role creates a pending invitation that expires after `INVITATION_EXPIRY_DAYS` (1 to 90, default 7). It answers 201 with `Location` and `{id, workspace_id, email, role, inviter_id, created_at, expires_at}`, records `invitation.created`, and enqueues the invitation email (INVITATIONS-010) in the same unit of work; with email off, the invitation is created all the same (PLATFORM-022). The role must rank below the inviter's, or the request answers 403 `request.permission_denied` (ACCESS-004). | SEC-09, DATA-22, D1, API-04 | `invitations_001_` |
| INVITATIONS-002 | Inviting an email that belongs to a member of the workspace answers 409 `invitations.already_member`, and writes nothing. | ERR-03 | `invitations_002_` |
| INVITATIONS-003 | Inviting an email that already has an invitation to this workspace refreshes it in place: the role, inviter and expiry are replaced, and the ID is kept. It answers 200 with the invitation, as INVITATIONS-001 shows it, records `invitation.updated`, and enqueues the email again. | DATA-22, API-04 | `invitations_003_` |
| INVITATIONS-004 | `GET /v1/users/me/invitations` lists the caller's unexpired invitations in every workspace, with `{id, workspace_id, workspace_name, inviter_name, role, expires_at}`, `inviter_name` being null once the inviter's account is deleted, the latest created first, in cursor pages. Its `search` matches the workspace's name. It works when the caller belongs to no workspace. | D34, DATA-16, API-06, API-20 | `invitations_004_` |
| INVITATIONS-005 | Accepting adds the caller to the workspace with the invitation's role, and deletes the invitation, in one unit of work. It answers 200 with `{workspace_id, role}`. It records `invitation.accepted` in the workspace's log, publishes the workspace event `member.joined`, which requires `members.read`, and publishes the user event `membership.created`. | D34, DATA-22 | `invitations_005_` |
| INVITATIONS-006 | Accepting an expired invitation answers 422 `invitations.expired`. Accepting while already a member answers 409 `invitations.already_member` and changes nothing. An invitation not addressed to the caller answers 404 `invitations.not_found`, and so does one whose workspace went with its owner's account while the acceptance waited (ACCESS-008). | D16, D34, DATA-16 | `invitations_006_` |
| INVITATIONS-007 | The periodic job `invitations.expire` deletes expired invitations every hour, across all workspaces. | D13, DATA-16 | `invitations_007_` |
| INVITATIONS-008 | When the invited email belongs to an account, the new or refreshed invitation also publishes the user event `invitation.received` to that user. | D34, SEC-19 | `invitations_008_` |
| INVITATIONS-009 | Each member may invite `INVITATION_LIMIT` times (1 to 100,000, default 100) per `INVITATION_WINDOW_SECONDS` (1 to 86,400, default 3,600), which bounds the emails a member's invitations send. Beyond that, inviting answers 429 `request.rate_limited` before anything is written. Every request to invite whose fields are valid is counted, whatever it answers. One refused for its fields, or its body, writes and sends nothing, and only PLATFORM-054's user limit counts it. | SEC-13, API-15, PLATFORM-011 | `invitations_009_` |
| INVITATIONS-010 | The invitation email names the inviter, the workspace, the role and the days until the invitation expires, and tells the invitee to sign in, or create an account, with the address it was sent to. It has a plain-text part, and an HTML part whose values are escaped. | D1, D26, D34 | `invitations_010_` |

## Design

### Who an invitation reaches

An invitation is addressed to an email, not to a user. It reaches whichever account holds the address when the invitee lists or accepts, whether or not one did when they were invited. With email on, the full `auth` module serves an account only once it has proven its address, so only the address's owner sees its invitations; with email off, an address is what its account claims ([auth spec](auth.md#email-verification), D67).

### Inviting

1. The service counts the request in the caller's bucket of INVITATIONS-009, in its own short transaction, before the unit of work opens (PLATFORM-011). Its key is a hash-based message authentication code (HMAC) of the caller's user ID (PLATFORM-057).
2. The service opens the unit of work with the `Authorize<InvitationsCreate>` the handler handed it, which authorizes the caller inside it (platform spec, Unit of work).
3. `accounts::find_by_email` looks up the invitee's account. If there is one, `access::is_member` decides INVITATIONS-002.
4. The invitation is inserted or refreshed by `(workspace_id, email)`, expiring `INVITATION_EXPIRY_DAYS` after `now()`.
5. The audit entry is recorded, and `Mail::send` enqueues the invitation email (The invitation email) under the proof's scope, the workspace and the inviter, with the inviter's name from `accounts::profiles`, the workspace's from `workspaces::names`, and the setting's days. If the invitee has an account, `invitation.received` is published to them (INVITATIONS-008). Then the unit of work commits.

The created invitation's `Location` is `/v1/workspaces/{workspace_id}/invitations/{id}`, the route that reads one invitation, which arrives with the full module.

### The invitation email

`emails.rs` declares it, with its templates in `crates/app/templates/invitations/` ([platform spec](platform.md#sending-email)). Its plain-text part reads:

> Subject: Bob Jones invited you to Acme on AntWorker
>
> Bob Jones invited you to join the workspace Acme on AntWorker, as a member. The invitation expires in 7 days.
>
> To accept it, sign in to AntWorker with alice@example.com, or create an account with that address.

### Listing the caller's invitations

The query filters on the caller's email, from `accounts::profile`, and on `expires_at` against `now()` (DATA-16). Workspace names come from `workspaces::names`, and inviter names from `accounts::profiles`, each passed only the IDs on the page. A search first reads the workspace IDs of every one of the caller's unexpired invitations, and `workspaces::matching` keeps those whose name contains it, so the page holds only their invitations.

### Accepting

1. The unit of work opens from the caller's `Principal`.
2. It reads the invitation by ID and the caller's email (DATA-16), `FOR UPDATE`, so two acceptances of it take turns (DATA-20), and checks the expiry in SQL.
3. `access::add_member` adds the caller, or reports that they are a member already, which refuses the acceptance and changes nothing.
4. It deletes the invitation, records the audit entry, publishes the events, and commits.

### Expiry sweep

The periodic job `invitations.expire` runs hourly, and deletes rows whose `expires_at` has passed, in batches of at most 1,000, as the platform's sweeps do. It spans every workspace by nature, so it selects rows by expiry alone (DATA-16). Scheduled maintenance isn't audited (DATA-22). An expired invitation is already invisible until then, since every query checks `expires_at`, so the interval is a constant rather than a setting (D9).

## Open design questions

None.
