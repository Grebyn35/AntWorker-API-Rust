# Spec: `accounts`

## Purpose

`accounts` owns the user record: who a user is, and how they appear to others. Registration creates the record through `auth`, and other modules read display data through a batch call. Profile editing, preferences, avatars, account deletion and data export arrive with the full module (D16, D31).

## Owns

| Table | Holds |
|---|---|
| `users` | `id`, `email`, `email_hash`, `name`, `email_verified`, `created_at`, `updated_at`, `version`. `email_hash` has a unique index and holds an HMAC under the key version current when it was written (D32). |

## Interface

- `create(uow, &Email, &DisplayName)`: inserts a user in a key-scoped unit of work, opened with the new email as its key, and returns the user's ID.
- `find_by_email(uow)`: the user matching the presented sign-in email, in a key-scoped unit of work.
- `profiles(uow, &[UserId])`: names and emails for a batch of users. Under workspace context, only co-members are visible (DATA-15).
- `co_member_by_email(uow, &Email)`: the co-member with this email, if there is one, under workspace context. `invitations` uses it to refuse inviting an existing member.
- The domain types `Email` and `DisplayName` (D25), used across modules.
- The personal data contract for `users`, registered with the composition root (D31).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| ACCOUNTS-001 | `GET /v1/users/me` returns the caller's own `id`, `email`, `name` and `email_verified`, and nothing about any other user. | API-05, DATA-22 | `accounts_001_` |
| ACCOUNTS-002 | An email is trimmed and lowercased, must be 1 to 254 characters, and must be a valid address. | D25, API-10 | `accounts_002_` |
| ACCOUNTS-003 | A display name is trimmed, has internal whitespace collapsed to single spaces, is normalized to Unicode NFC, and must be 1 to 100 characters and not only whitespace. | D25, API-10 | `accounts_003_` |
| ACCOUNTS-004 | No two users have the same email, under any key version. A second insert is refused as `accounts.email_in_use`. | DATA-05, ERR-05, D32 | `accounts_004_` |
| ACCOUNTS-005 | `profiles` returns exactly the requested users that the caller's context may see, and silently omits the rest. | DATA-15, DATA-22 | `accounts_005_` |

## Design

### Uniqueness across key versions

The unique index on `email_hash` stops two rows with the same hash under the same key version. During a rotation, the same email can hash differently under two versions. `create` therefore first looks the email up under every active version, in the same key-scoped unit of work, before it inserts. The two steps can race, but only when two registrations for one email arrive during a rotation, and the result is a rare duplicate. Retiring a key version requires re-hashing the rows stored under it. That job, and how it handles such a pair, are specified with key rotation.

### Email verification

`email_verified` starts as `false` and isn't enforced anywhere in this slice. The verification flow, and the gate that enforces it, come with the full `auth` module.

## Open design questions

| Question | Settled by |
|---|---|
| Whether the registration race during a key rotation needs to be closed with an advisory lock on the email hash | `accounts_004_`, run concurrently during a simulated rotation |
