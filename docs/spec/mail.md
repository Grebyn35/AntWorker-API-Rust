# Spec: `mail`

## Purpose

`mail` defines the emails the API sends, and sends them from background jobs, never inside a request (D1). A deployment turns email on or off, and what an address proves follows that choice (D67). This spec covers the email settings, sending, and the invitation email. The account and sign-in emails arrive with the full `auth` and `accounts` modules.

## Owns

Nothing. Jobs live in `platform`'s `jobs` table.

## Interface

- `Mail`, built once from the email settings, in `service.rs`, with `send(uow, scope, Message)`, which enqueues the message as a job in the unit of work, or does nothing with email off, and `is_on`, which the capabilities endpoint reads. `Message` is an enum with one variant per kind of email, each carrying its recipient and exactly the values its template needs.
- The job `mail.send`, declared in `jobs.rs` (OPS-09), which `Mail` registers with the job registry. It sends through `platform`'s mail transport ([platform spec](platform.md#mail)).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| MAIL-001 | An email is sent only by a job. A request that sends an email only enqueues it, so it never waits on the mail server, which it reaches over the Simple Mail Transfer Protocol (SMTP). A failed send is retried with backoff, up to 12 attempts, and ends in a dead letter; a rejection that the server calls permanent, a reply from 500 to 599, ends there at once. | D1, DATA-26, OPS-10 | `mail_001_` |
| MAIL-002 | The invitation email names the inviter, the workspace, the role and the days until the invitation expires, and tells the invitee to sign in, or create an account, with the address it was sent to. It has an HTML part and a plain-text part. | D1, D34 | `mail_002_` |
| MAIL-003 | Templates are checked at compile time. Values in the HTML part are escaped. | D24, D26 | `mail_003_` |
| MAIL-004 | With email turned off, enqueueing does nothing, and the capabilities endpoint reports `email` as `false`. The invitation itself is still created. A replica with email off that runs a mail job enqueued while it was on drops the job. | D9, OPS-04 | `mail_004_` |
| MAIL-005 | In development, a mail catcher on the developer's machine receives every email, and nothing leaves the machine. | D28 | `mail_005_` |
| MAIL-006 | `EMAIL` is required, `smtp` or `off`. With `smtp`, startup fails without `SMTP_HOST` or `EMAIL_FROM`, with only one of `SMTP_USERNAME` and `SMTP_PASSWORD`, and with credentials when `SMTP_SECURITY` is `none`, so no password crosses the network unencrypted. With `off`, no other email setting is read. | D9, OPS-01, OPS-03 | `mail_006_` |
| MAIL-007 | Every email comes from `EMAIL_FROM`, and carries a `Message-ID` fixed when it was enqueued, so an email that a retry delivers twice arrives with the same `Message-ID`. | D1, OPS-10 | `mail_007_` |

## Design

### Settings

| Setting | Meaning |
|---|---|
| `EMAIL` | `smtp` sends email through the server the settings below name; `off` sends none (MAIL-004). Required, with no default, so no deployment is without email by accident: a forgotten setting fails startup rather than leaving every password reset unanswered. |
| `SMTP_HOST` | The mail server's host name. Required with `smtp`. |
| `SMTP_SECURITY` | `tls`, a connection that starts with TLS; `starttls`, which upgrades to TLS before anything else is sent, and refuses a server that can't; or `none`, with no encryption, for a mail catcher on a private network. Default `starttls`. |
| `SMTP_PORT` | Default 465 with `tls`, 587 with `starttls`, and 25 with `none`. |
| `SMTP_USERNAME` and `SMTP_PASSWORD` | The credentials, both or neither; the password is a secret, read through `SMTP_PASSWORD_FILE` in a deployment (SEC-01). |
| `EMAIL_FROM` | The sender, as a name and an address, such as `AntWorker <noreply@example.com>`. Required with `smtp`. |

A setting for timeouts, connection pooling or retries would be one nobody plausibly needs to change (D9), so those are constants: a connection or a reply times out after 10 seconds, and a send after 30.

### Sending

`send` renders nothing: it enqueues a `mail.send` job whose payload holds the recipient, the template's values and a `Message-ID` of a new random UUID at `EMAIL_FROM`'s domain, never a secret, under the scope of the unit of work's proof. The job's time limit is 60 seconds, and its 12 attempts space their retries over about 2 to 3.5 hours (platform spec, Jobs), so a mail server down for a few hours delays an email rather than losing it. The handler renders the message, sends it, and needs no database access.

A job runs at least once, so an email may arrive twice: when a replica stops after the server accepted the email and before the job's success was written, its second run sends a copy. No protocol makes a send exactly once; the copy carries the same `Message-ID`, by which a receiving system recognizes it (MAIL-007, OPS-09).

It sends through `platform`'s SMTP transport, whose failures hold the server's reply code or the failure's kind only ([platform spec](platform.md#mail)). A reply from 500 to 599 is permanent (MAIL-001); any other failure is retried.

### Templates

Each kind of email has an HTML template and a plain-text template in `crates/app/templates/email/`, which `askama` compiles into the binary, checking each value the template names against the variant's fields (MAIL-003). The HTML templates escape every value; the plain-text ones escape none, since a plain-text part isn't interpreted. The subject is built in code from the same values. The templates name the product, which `just rename` rewrites (ARCH-15). Emails are in English only, and carry no link: the API serves clients whose addresses it doesn't know, such as the desktop client.

The invitation email (MAIL-002) reads, in its plain-text part:

> Subject: Bob Jones invited you to Acme on AntWorker
>
> Bob Jones invited you to join the workspace Acme on AntWorker, as a member. The invitation expires in 7 days.
>
> To accept it, sign in to AntWorker with alice@example.com, or create an account with that address.

### Development

`just services` starts Mailpit beside Postgres, with its SMTP server on `127.0.0.1:1025` and its web page on `127.0.0.1:8025`, where every email the API sent shows. `just setup` writes `.env` with email on and pointing at it, so nothing leaves the machine and nothing needs configuring (MAIL-005). The tests send to the same Mailpit, each test to addresses of its own, and read what arrived through its API.

## Open design questions

None.
