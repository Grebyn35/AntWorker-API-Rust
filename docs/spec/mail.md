# Spec: `mail`

## Purpose

`mail` defines the emails the API sends, and sends them from background jobs, never inside a request (D1). This spec covers the invitation email. The account and sign-in emails arrive with the full `auth` and `accounts` modules.

## Owns

Nothing. Jobs live in `platform`'s `jobs` table.

## Interface

- `send(uow, Message)`: enqueues the message as a job in the given unit of work. `Message` is an enum with one variant per kind of email, each carrying exactly the values its template needs.
- The job handler `mail.send`, registered with the job registry.

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| MAIL-001 | An email is sent only by a job. A request that sends an email only enqueues it, so it never waits on SMTP. A failed send is retried with backoff and ends in a dead letter (OPS-10). | D1, DATA-26, OPS-10 | `mail_001_` |
| MAIL-002 | The invitation email names the inviter, the workspace, the role and the days until expiry, in an HTML part and a plain-text part. | D1 | `mail_002_` |
| MAIL-003 | Templates are checked at compile time. Values in the HTML part are escaped. | D24, SEC-10 | `mail_003_` |
| MAIL-004 | With email turned off, enqueueing does nothing, and the capabilities endpoint reports email as off. The invitation itself is still created. | D9, OPS-04 | `mail_004_` |
| MAIL-005 | In development, Mailpit receives every email, and nothing leaves the machine. | D28 | `mail_005_` |

## Design

The job payload holds the rendered recipient and the template's values, never a secret. A successful send deletes the payload (OPS-10). Sending needs no database access.

## Open design questions

None.
