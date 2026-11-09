# Spec: `accounts`

## Purpose

`accounts` owns the user record: who a user is, and how they appear to others. Registration creates the record through `auth`, other modules read display data through batch calls, and a user renames themselves here. Account deletion and data export arrive with the module's next parts, through the personal data contract (D16, D31).
A user also sets and removes their avatar here, whose image `files` keeps (D117, D120). <!-- part: files -->

## Owns

| Table | Holds |
|---|---|
| `users` | `id`, `email`, `name`, `email_verified`, `created_at`, `updated_at`, `version`. `email` holds the normalized address and has the unique index `users_email_key`. `email` and `name` are personal data (DATA-23). |

## Interface

- `create(uow, &EmailAddress, &DisplayName)`: inserts a user and returns `Creation::Created` with the user's ID, or `Creation::EmailInUse` when the unique index refuses the email (ACCOUNTS-004).
- `find_by_email(uow, &EmailAddress)`: the ID of the user with this email, if there is one. `auth` uses it to sign in.
  `invitations` uses it to find an invitee's account. <!-- part: invitations -->
- `profile(uow, &Principal)`: the caller's own record, as ACCOUNTS-001 gives it. <!-- part: invitations -->
- `profiles(uow, &[UserId])`: names and emails for a batch of users. Callers pass only IDs that came from a query filtered by tenant, such as a workspace's member list (DATA-16).
  Each profile also holds the link to the user's avatar (ACCOUNTS-011). <!-- part: files -->
- `matching(uow, &[UserId], &Search)`: the IDs among a batch, passed under the same terms, of the users whose name or email contains the search (API-20).
- The domain type `DisplayName` (D25). `EmailAddress` is in `platform::types`.
- The personal data contract for `users`, registered with the composition root (D31).

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `GET /v1/users/me` | user | ACCOUNTS-001 |
| `PATCH /v1/users/me` | user | ACCOUNTS-003, ACCOUNTS-007 |
| `PUT /v1/users/me/avatar` | user | ACCOUNTS-008, ACCOUNTS-009 <!-- part: files --> |
| `DELETE /v1/users/me/avatar` | user | ACCOUNTS-010 <!-- part: files --> |

`PATCH /v1/users/me` accepts an `If-Match` (API-13, PLATFORM-071).
`PUT` and `DELETE /v1/users/me/avatar` accept one too, since each changes the record, its `version` with it. <!-- part: files -->
`PUT /v1/users/me/avatar` is an upload route, added through `Routes::user_upload`, whose body is the image's bytes, which it reads through `Upload` with the avatar's limit (ACCOUNTS-009, PLATFORM-074). <!-- part: files -->

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| ACCOUNTS-001 | `GET /v1/users/me` returns the caller's own `id`, `email`, `name` and `email_verified`, with the record's `ETag` (PLATFORM-071), and nothing about any other user. | API-05, API-13, DATA-16 | `accounts_001_` |
| ACCOUNTS-002 | An email address is trimmed and lowercased, must be 1 to 254 characters, and must be a valid email address as the HTML standard defines one for `<input type="email">`. | D25, API-10 | `accounts_002_` |
| ACCOUNTS-003 | A display name is trimmed, has internal whitespace collapsed to single spaces, is normalized to Unicode Normalization Form C (NFC), and must be 1 to 100 characters and not only whitespace. | D25, API-10 | `accounts_003_` |
| ACCOUNTS-004 | No two users have the same email. A second insert is refused by the unique index, and `create` reports that the email is in use, which its caller answers with a code of its own, such as `auth.email_in_use` (AUTH-002). | DATA-05, ERR-05 | `accounts_004_` |
| ACCOUNTS-005 | `profiles` returns exactly the requested users. | DATA-16 | `accounts_005_` |
| ACCOUNTS-006 | `matching` returns exactly the requested users whose name or email contains the search, ignoring case. | DATA-16, API-20 | `accounts_006_` |
| ACCOUNTS-007 | Renaming oneself, `PATCH /v1/users/me` with the merge patch `{"name": <name>}`, stores the name, normalized as ACCOUNTS-003 says, and answers 200 with the record as ACCOUNTS-001 gives it and its new `ETag`. It records `user.renamed` in the caller's own log, and publishes the user event `user.updated` to their own streams, and, in each workspace they belong to, the workspace event `member.updated`, which requires `members.read`. None of them holds the name. | API-03, API-13, API-19, SEC-18, SEC-19, DATA-22 | `accounts_007_` |
| ACCOUNTS-008 | Setting one's avatar, `PUT /v1/users/me/avatar` with the image's bytes as the body, stores it as the caller's avatar, in place of any they had, and answers 200 with the record as ACCOUNTS-001 gives it, whose `avatar_url` names the new image, and its new `ETag`. It records `avatar.uploaded` in the caller's own log, and publishes `user.updated` and `member.updated` as ACCOUNTS-007 does. The bytes of an avatar it replaces stay until FILES-011 deletes them. A request that no credential authenticates answers 401 with its body unread. <!-- part: files --> | API-03, API-08, DATA-21, DATA-22, SEC-18, SEC-19, D120 | `accounts_008_` |
| ACCOUNTS-009 | An avatar is a PNG, JPEG, GIF or WebP image of at most 1 MiB, of the kind whose signature its bytes start with, as files' Accepted kinds checks it. A body of more than 1 MiB answers 413 `request.payload_too_large`, whether its `Content-Length` declares it or it grows past the limit while it is read. An empty body, or one that is no such image, answers 422 `accounts.avatar_not_image`, and one in which the scanner finds malware 422 `accounts.avatar_malware_detected`. While the scanner or the storage doesn't answer, or not within its time, the upload answers 503 `accounts.avatar_unavailable`, with `Retry-After`, unless its route's 60 seconds pass first, when it answers 503 `request.timed_out` (PLATFORM-042). None of these stores anything. <!-- part: files --> | API-08, ERR-03, D63, D121 | `accounts_009_` |
| ACCOUNTS-010 | Removing one's avatar, `DELETE /v1/users/me/avatar`, answers 204, and when the caller has one, removes it, records `avatar.deleted` in their own log, and publishes `user.updated` and `member.updated` as ACCOUNTS-007 does. The removed image's bytes stay until FILES-011 deletes them. <!-- part: files --> | API-04, DATA-22, SEC-18, SEC-19, D120 | `accounts_010_` |
| ACCOUNTS-011 | The record that ACCOUNTS-001 gives, and each profile that `profiles` gives, carries `avatar_url`: the path of the public download of the user's avatar (FILES-024), or null when they have none. <!-- part: files --> | API-05, D120 | `accounts_011_` |

## Design

### Renaming

The patch's name is parsed into a `DisplayName` before the unit of work opens. The unit of work's first statement locks the user's row `FOR UPDATE` and reads whether its `version` is one that `If-Match` names, so a stale `ETag` changes nothing and answers 412 (PLATFORM-071). The update then writes the name, and the row's trigger moves its `version` and `updated_at` (DATA-02). A rename writes, records and publishes even when the name is the one stored, as a workspace's rename does (WORKSPACES-008).

### Events

A user's name shows wherever they appear to others: in each workspace's member list (WORKSPACES-004), and on the user's own other devices. So a rename publishes `user.updated` to the user's own streams, and `member.updated` in every workspace they belong to, whose members with `members.read` read the member again, since the payloads carry IDs alone (API-19, D7). The workspaces come from `access::workspace_ids`, read in the rename's own unit of work once it holds the lock on the user's row, which a membership's insert waits for, since its foreign key reads the row `FOR KEY SHARE` (DATA-20). So a membership that commits before the rename's read is among the workspaces it publishes in, and one that commits after the rename does, since it waited, is read by its members with the new name.
A change to the user's avatar publishes the same two events, since the avatar shows wherever the name does (ACCOUNTS-008, ACCOUNTS-010). <!-- part: files -->

<!-- part: files -->
### Avatars

`files` keeps each avatar: its row in `avatars`, and its bytes in the bucket under the prefix `avatars/` ([files spec](files.md#avatars)). `accounts` owns setting and removing it, its entries in the user's log, and its events, since an avatar is part of how a user appears, as their name is, so both changes reach clients the same way (D120).

1. The handler refuses a body whose `Content-Length` declares more than 1 MiB, then reads the body, in one of the replica's turns, which it holds until the avatar is stored (PLATFORM-074). `files::upload_avatar` finds its kind from its bytes, has the scanner scan it, and writes its object, outside any transaction (DATA-21).
2. It then runs what `accounts` gives it, which opens the unit of work. Its first statement locks the user's row and checks `If-Match`, as a rename's does, so two changes of one user's record take turns (DATA-20), and the next moves the record's `version`. `files::record_avatar` then deletes the user's avatar row, if any, and inserts the new one. The entry is recorded, the events are published, and the unit of work commits, which counts the upload as stored (FILES-013). A stale `ETag` answers 412 once the object is written, which FILES-011 then deletes.

An object written by an upload whose unit of work never commits, and the object of a replaced or removed avatar, are left to FILES-011, as every unreferenced object is. Removing an avatar locks the user's row, and moves their `version` when there was one to remove, so every change to what ACCOUNTS-001 gives moves its `ETag`.

An avatar is public, so a client shows it with an `<img>` that sends no credential, to the user's fellow members and anyone they show it to, and its link never expires. An avatar's ID changes with each image, so a client and a shared cache that hold one image never take it for the next.

ACCOUNTS-009's limit is fixed, beside `FILE_MAX_BYTES`, whose default is ten times it, since every member list downloads each member's avatar: a client shrinks a photo to the size it shows, such as 512 pixels square, which a JPEG or a WebP holds in about 100 kilobytes, and a PNG in at most about 800 (D121). The API reads no image's dimensions, since it never decodes one (D109).
<!-- end part: files -->

### Email verification

`email_verified` starts as `false`, and nothing enforces it in this slice, though ACCOUNTS-001 returns it. What it decides, once the full `auth` module arrives, is in the [auth spec](auth.md#email-verification).

### Export and erasure

Both arrive with the module's next parts, through the personal data contract (D31). Export covers the same workspaces that erasure deletes: the content of every workspace whose only member is the user is part of their export, and the content of every other workspace isn't (D16). In the personal shape, that is everything the user made ([architecture §5](../architecture.md#workspace-shapes)).

## Open design questions

None.
