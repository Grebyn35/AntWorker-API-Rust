# Spec: `invitations`

## Purpose

`invitations` lets a member invite someone by email, and lets the invitee see and accept the invitation. This spec covers inviting, listing the caller's own invitations, accepting, and the expiry sweep. Listing a workspace's invitations, updating, resending, cancelling and declining arrive with the full module.

## Owns

| Table | Holds |
|---|---|
| `invitations` | `id`, `workspace_id`, `email`, `email_hash`, `role`, `invited_by`, `workspace_name`, `inviter_name`, `created_at`, `expires_at`, `updated_at`, `version`. `(workspace_id, email_hash)` is unique, and `email_hash` has an index. |

## Interface

- `Admission` is built here, only from an invitation the caller's context can see (DATA-16).

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `POST /v1/workspaces/{workspace_id}/invitations` | `workspace::<InvitationsCreate>` | INVITATIONS-001 to INVITATIONS-003 |
| `GET /v1/users/me/invitations` | user | INVITATIONS-004 |
| `POST /v1/users/me/invitations/{invitation_id}/accept` | user | INVITATIONS-005, INVITATIONS-006 |

`POST .../invitations` accepts an `Idempotency-Key` (API-12).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| INVITATIONS-001 | Inviting an email with a role creates a pending invitation that expires after the configured number of days (1 to 90, default 7). It answers 201, records `invitation.created`, and enqueues the invitation email (MAIL-002) in the same unit of work. The role must rank below the inviter's, or the request answers 403 `access.permission_denied` (ACCESS-004). | SEC-08, DATA-27, D1 | `invitations_001_` |
| INVITATIONS-002 | Inviting an email that belongs to a member of the workspace answers 409 `invitations.already_member`, and writes nothing. The check uses `accounts::co_member_by_email`. | ERR-03, ARCH-06 | `invitations_002_` |
| INVITATIONS-003 | Inviting an email that already has an invitation to this workspace refreshes it in place: the role, inviter, names and expiry are replaced and the ID is kept. It answers 200, records `invitation.updated`, and enqueues the email again. | DATA-27, API-04 | `invitations_003_` |
| INVITATIONS-004 | `GET /v1/users/me/invitations` lists the caller's unexpired invitations in every workspace, with `{id, workspace_id, workspace_name, inviter_name, role, expires_at}`, in cursor pages. It works when the caller belongs to no workspace. | D34, DATA-22, API-06 | `invitations_004_` |
| INVITATIONS-005 | Accepting adds the caller to the workspace with the invitation's role, and deletes the invitation, in one unit of work. It answers 200 with `{workspace_id, role}`. It records `invitation.accepted` in the workspace's log, publishes the workspace event `member.joined`, which requires `members.read`, and publishes the user event `membership.created`. | D34, DATA-16, DATA-27 | `invitations_005_` |
| INVITATIONS-006 | Accepting an expired invitation answers 422 `invitations.expired`. Accepting while already a member answers 409 `invitations.already_member` and changes nothing. An invitation not addressed to the caller answers 404. | D16, D34, DATA-22 | `invitations_006_` |
| INVITATIONS-007 | A scheduled job deletes expired invitations every hour, configurable, across all workspaces. | OPS-11, DATA-18 | `invitations_007_` |

## Design

### Names on the invitation

The invitee isn't a member, so the invitee's context can't read the workspace or the inviter (DATA-15). The invitation therefore stores `workspace_name` and `inviter_name` when it is created or refreshed. If the workspace is renamed afterwards, the invitation shows the old name until it is refreshed. The invitee sees the current name once they join.

### Accepting

1. The unit of work opens with `for_user`.
2. It reads the invitation by ID, filtered by the caller's email hashes (DATA-22).
3. It checks the expiry in SQL, and checks for an existing membership.
4. It builds the `Admission` and calls `join`, which moves the unit of work into the invitation's workspace context (DATA-16).
5. It calls `access::add_member`, deletes the invitation, records the audit entry, publishes the events, and commits (PLATFORM-008).

### Expiry sweep

The job `invitations.expire` runs with `for_scheduled_job` and deletes rows whose `expires_at` has passed (DATA-18, DATA-27: scheduled maintenance isn't audited).

## Open design questions

| Question | Settled by |
|---|---|
| How an invitee's open streams learn about a new invitation. The invitation is created in the inviter's workspace context, which can't see the invitee's user ID, so it can't publish a user event to them. Until this is settled, the invitee sees new invitations by listing them and by email. | a test that an invitee with an open stream receives `invitation.received`, once a design for it exists |
