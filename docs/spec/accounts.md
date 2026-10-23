# Spec: `accounts`

## Purpose

`accounts` owns the user record: who a user is, and how they appear to others. Registration creates the record through `auth`, and other modules read display data through batch calls. Profile editing, preferences, avatars, account deletion and data export arrive with the full module (D16, D31).

## Owns

| Table | Holds |
|---|---|
| `users` | `id`, `email`, `name`, `email_verified`, `created_at`, `updated_at`, `version`. `email` holds the normalized address and has a unique index. |

## Interface

- `create(uow, &EmailAddress, &DisplayName)`: inserts a user and returns the user's ID, or reports `accounts.email_in_use`.
- `find_by_email(uow, &EmailAddress)`: the ID of the user with this email, if there is one. `auth` uses it to sign in, and `invitations` to find an invitee's account.
- `profile(uow, &Principal)`: the caller's own `id`, `email`, `name` and `email_verified`.
- `profiles(uow, &[UserId])`: names and emails for a batch of users. Callers pass only IDs that came from a query filtered by tenant, such as a workspace's member list (DATA-22).
- The domain type `DisplayName` (D25). `EmailAddress` is in `platform::types`.
- The personal data contract for `users`, registered with the composition root (D31).

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `GET /v1/users/me` | user | ACCOUNTS-001 |

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| ACCOUNTS-001 | `GET /v1/users/me` returns the caller's own `id`, `email`, `name` and `email_verified`, and nothing about any other user. | API-05, DATA-22 | `accounts_001_` |
| ACCOUNTS-002 | An email address is trimmed and lowercased, must be 1 to 254 characters, and must be a valid address. | D25, API-10 | `accounts_002_` |
| ACCOUNTS-003 | A display name is trimmed, has internal whitespace collapsed to single spaces, is normalized to Unicode NFC, and must be 1 to 100 characters and not only whitespace. | D25, API-10 | `accounts_003_` |
| ACCOUNTS-004 | No two users have the same email. A second insert is refused by the unique index, and reported as `accounts.email_in_use`. | DATA-05, ERR-05 | `accounts_004_` |
| ACCOUNTS-005 | `profiles` returns exactly the requested users. | DATA-22 | `accounts_005_` |

## Design

### Email verification

`email_verified` starts as `false` and isn't enforced anywhere in this slice. The verification flow, and the gate that enforces it, come with the full `auth` module.

## Open design questions

None.
