# Spec: `invitations`

## Purpose

`invitations` lets a member invite someone by email, and lets the invitee see and accept the invitation. This spec covers inviting, the invitee's live notification, listing the caller's own invitations, accepting, and the expiry sweep. Listing a workspace's invitations, updating, resending, cancelling and declining arrive with the full module.

## Owns

| Table | Holds |
|---|---|
| `invitations` | `id`, `workspace_id`, `email` (normalized), `role`, `invited_by`, `created_at`, `expires_at`, `updated_at`, `version`. `(workspace_id, email)` is unique, and `email` has an index. |

## Interface

None.

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `POST /v1/workspaces/{workspace_id}/invitations` | `workspace::<InvitationsCreate>` | INVITATIONS-001 to INVITATIONS-003, INVITATIONS-008 |
| `GET /v1/users/me/invitations` | user | INVITATIONS-004 |
| `POST /v1/users/me/invitations/{invitation_id}/accept` | user | INVITATIONS-005, INVITATIONS-006 |

`POST .../invitations` accepts an `Idempotency-Key` (API-12).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| INVITATIONS-001 | Inviting an email with a role creates a pending invitation that expires after `INVITATION_EXPIRY_DAYS` (1 to 90, default 7). It answers 201 with `Location` and `{id, workspace_id, email, role, invited_by, created_at, expires_at}`, records `invitation.created`, and enqueues the invitation email (MAIL-002) in the same unit of work; with email off, the invitation is created all the same (MAIL-004). The role must rank below the inviter's, or the request answers 403 `request.permission_denied` (ACCESS-004). | SEC-08, DATA-27, D1, API-04 | `invitations_001_` |
| INVITATIONS-002 | Inviting an email that belongs to a member of the workspace answers 409 `invitations.already_member`, and writes nothing. | ERR-03 | `invitations_002_` |
| INVITATIONS-003 | Inviting an email that already has an invitation to this workspace refreshes it in place: the role, inviter and expiry are replaced, and the ID is kept. It answers 200 with the invitation, as INVITATIONS-001 shows it, records `invitation.updated`, and enqueues the email again. | DATA-27, API-04 | `invitations_003_` |
| INVITATIONS-004 | `GET /v1/users/me/invitations` lists the caller's unexpired invitations in every workspace, with `{id, workspace_id, workspace_name, inviter_name, role, expires_at}`, in cursor pages. Its `search` matches the workspace's name. It works when the caller belongs to no workspace. | D34, DATA-22, API-06, API-20 | `invitations_004_` |
| INVITATIONS-005 | Accepting adds the caller to the workspace with the invitation's role, and deletes the invitation, in one unit of work. It answers 200 with `{workspace_id, role}`. It records `invitation.accepted` in the workspace's log, publishes the workspace event `member.joined`, which requires `members.read`, and publishes the user event `membership.created`. | D34, DATA-27 | `invitations_005_` |
| INVITATIONS-006 | Accepting an expired invitation answers 422 `invitations.expired`. Accepting while already a member answers 409 `invitations.already_member` and changes nothing. An invitation not addressed to the caller answers 404 `invitations.not_found`. | D16, D34, DATA-22 | `invitations_006_` |
| INVITATIONS-007 | The periodic job `invitations.expire` deletes expired invitations every hour, across all workspaces. | OPS-11, DATA-22 | `invitations_007_` |
| INVITATIONS-008 | When the invited email belongs to an account, the new or refreshed invitation also publishes the user event `invitation.received` to that user. | D34, SEC-17 | `invitations_008_` |

## Design

### Who an invitation reaches

An invitation is addressed to an email, not to a user. It reaches whichever account holds the address when the invitee lists or accepts, whether or not one did when they were invited. With email on, the full `auth` module serves an account only once it has proven its address, so only the address's owner sees its invitations; with email off, an address is what its account claims ([auth spec](auth.md#email-verification), D67).

### Inviting

1. The unit of work is the one `Authorized<InvitationsCreate>` opened.
2. `accounts::find_by_email` looks up the invitee's account. If there is one, `access::is_member` decides INVITATIONS-002.
3. The invitation is inserted or refreshed by `(workspace_id, email)`, expiring `INVITATION_EXPIRY_DAYS` after `now()`.
4. The audit entry is recorded, and `Mail::send` enqueues the invitation email under the proof's scope, the workspace and the inviter, with the inviter's name from `accounts::profiles`, the workspace's from `workspaces::names`, and the setting's days. If the invitee has an account, `invitation.received` is published to them (INVITATIONS-008). Then the unit of work commits.

The created invitation's `Location` is `/v1/workspaces/{workspace_id}/invitations/{id}`, the route that reads one invitation, which arrives with the full module.

### Listing the caller's invitations

The query filters on the caller's email, from `accounts::profile`, and on `expires_at` against `now()` (DATA-22). Workspace names come from `workspaces::names`, and inviter names from `accounts::profiles`, each passed only the IDs on the page. A search first reads the workspace IDs of every one of the caller's unexpired invitations, and `workspaces::matching` keeps those whose name contains it, so the page holds only their invitations.

### Accepting

1. The unit of work opens from the caller's `Principal`.
2. It reads the invitation by ID and the caller's email (DATA-22), and checks the expiry in SQL.
3. `access::is_member` refuses an existing member. Otherwise `access::add_member` adds the caller.
4. It deletes the invitation, records the audit entry, publishes the events, and commits.

### Expiry sweep

The periodic job `invitations.expire` runs hourly, and deletes rows whose `expires_at` has passed, in batches of at most 1,000, as the platform's sweeps do. It spans every workspace by nature, so it selects rows by expiry alone (DATA-22). Scheduled maintenance isn't audited (DATA-27). An expired invitation is already invisible until then, since every query checks `expires_at`, so the interval is a constant rather than a setting (D9).

## Open design questions

None.
