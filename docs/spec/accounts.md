# Spec: `accounts`

## Purpose

`accounts` owns the user record: who a user is, and how they appear to others. Registration creates the record through `auth`, other modules read display data through batch calls, and a user renames themselves here. It deletes an account once `auth` has confirmed the deletion, across every module, through the personal data contract (D16, D31). Data export arrives with the module's next part.
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
- `Deletion`, which the composition root builds from every module's personal data contract, in the order it registers them, and whose `delete(uow, &Principal)` deletes the caller's account in the unit of work of `auth` that confirmed it, and reports whether it did, or why not, which `auth` answers with codes of its own (ACCOUNTS-012 to ACCOUNTS-016, AUTH-023).
- `Contract`, the personal data contract for `users`, which the composition root registers last (D31).

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
| ACCOUNTS-012 | Deleting an account is refused while the user owns a workspace that has another member: `delete` reports it, which `auth` answers with 409 `auth.owns_shared_workspace` (AUTH-023), and nothing changes. A workspace whose only member is the user refuses nothing. | D16, ERR-03 | `accounts_012_` |
| ACCOUNTS-013 | Deleting an account deletes, in one unit of work, the user's record and everything their account holds: their identities, sessions, device tokens, own audit log, idempotency records, events and jobs, their memberships, and each workspace whose only member they are, with everything in it. Every session's streams close. Afterwards no row names the user, and none holds their email or name, but for what workspaces keep as their own (ACCOUNTS-014, D125), the events that name them, which go within a day (PLATFORM-019), and the email of ACCOUNTS-015 until it is sent, or, should it fail, for as long as a dead letter keeps its payload (PLATFORM-014). | D16, D31, DATA-06, DATA-23, SEC-06 | `accounts_013_` |
| ACCOUNTS-014 | In each workspace the user shares with others, the deletion ends their membership, records `member.left` in the workspace's log, and publishes the workspace event `member.left`, which requires `members.read`. What they made there is the workspace's, and stays, without their ID. The workspace's log keeps each entry that named them, without their ID, their IP address and their user agent (AUDIT-006). | D6, D16, D31, D125, SEC-18, SEC-19 | `accounts_014_` |
| ACCOUNTS-015 | With email on, a deletion enqueues, in its unit of work, as a job for no one, an email to the account's address that says the account and its data are deleted, which carries no link and is sent once the deletion commits (PLATFORM-021). With email off, nothing is sent (PLATFORM-022). | D1, PLATFORM-021 | `accounts_015_` |
| ACCOUNTS-016 | Once a deletion holds the user's row for its last step, it waits for no lock, but the millisecond that is the least `lock_timeout` Postgres takes. When another request holds a lock on a row that the deletion of the user's row deletes or changes, or a membership remains that a request of the user's added while the deletion ran, `delete` reports it, which `auth` answers with 409 `auth.account_changed`, and nothing changes, so the client sends the deletion again. | DATA-20, ERR-03, ERR-05 | `accounts_016_` |
| ACCOUNTS-017 | A deletion deletes the user's own files and their avatar, whose bytes FILES-011 deletes. A file they uploaded to a workspace that stays keeps its row, with its `uploader_id` null. <!-- part: files --> | D16, D31, D125 | `accounts_017_` |
| ACCOUNTS-018 | An invitation that the user sent to a workspace that stays keeps its row, with no inviter, and an invitation addressed to their email, the workspace's that sent it, stays as it was. <!-- part: invitations --> | D16, D31, D125 | `accounts_018_` |
| ACCOUNTS-019 | A deletion that waited for another deletion of the same account, which committed first, finds the account deleted, changes nothing, and answers as that deletion did, 204. | D16, DATA-20 | `accounts_019_` |

## Design

### Renaming

The patch's name is parsed into a `DisplayName` before the unit of work opens. The unit of work's first statement locks the user's row `FOR UPDATE`. The update then writes the name with the versions that `If-Match` names in its `WHERE`, so a stale `ETag` changes nothing and answers 412 (DATA-18, PLATFORM-071), and the row's trigger moves its `version` and `updated_at` (DATA-02). A rename writes, records and publishes even when the name is the one stored, as a workspace's rename does (WORKSPACES-008).

### Events

A user's name shows wherever they appear to others: in each workspace's member list (WORKSPACES-004), and on the user's own other devices. So a rename publishes `user.updated` to the user's own streams, and `member.updated` in every workspace they belong to, whose members with `members.read` read the member again, since the payloads carry IDs alone (API-19, D7). The workspaces come from `access::workspace_ids`, read in the rename's own unit of work once it holds the lock on the user's row, which a membership's insert waits for, since its foreign key reads the row `FOR KEY SHARE` (DATA-20). So a membership that commits before the rename's read is among the workspaces it publishes in, and one that commits after the rename does, since it waited, is read by its members with the new name.
A change to the user's avatar publishes the same two events, since the avatar shows wherever the name does (ACCOUNTS-008, ACCOUNTS-010). <!-- part: files -->

<!-- part: files -->
### Avatars

`files` keeps each avatar: its row in `avatars`, and its bytes in the bucket under the prefix `avatars/` ([files spec](files.md#avatars)). `accounts` owns setting and removing it, its entries in the user's log, and its events, since an avatar is part of how a user appears, as their name is, so both changes reach clients the same way (D120).

1. The handler refuses a body whose `Content-Length` declares more than ACCOUNTS-009's limit, then reads the body, in one of the replica's turns, which it holds until the avatar is stored (PLATFORM-074). `files::upload_avatar` finds its kind from its bytes, has the scanner scan it, and writes its object, outside any transaction (DATA-21).
2. It then runs what `accounts` gives it, which opens the unit of work. Its first statement locks the user's row, as a rename's does, so two changes of one user's record take turns (DATA-20), and the next moves the record's `version`, with the versions that `If-Match` names in its `WHERE` (DATA-18). `files::record_avatar` then deletes the user's avatar row, if any, and inserts the new one. The entry is recorded, the events are published, and the unit of work commits, which counts the upload as stored (FILES-013). A stale `ETag` answers 412 once the object is written, which FILES-011 then deletes.

An object written by an upload whose unit of work never commits, and the object of a replaced or removed avatar, are left to FILES-011, as every unreferenced object is. Removing an avatar locks the user's row, and moves their `version` when there was one to remove, so every change to what ACCOUNTS-001 gives moves its `ETag`.

An avatar is public, so a client shows it with an `<img>` that sends no credential, to the user's fellow members and anyone they show it to, and its link never expires. An avatar's ID changes with each image, so a client and a shared cache that hold one image never take it for the next.

ACCOUNTS-009's limit is fixed, beside `FILE_MAX_BYTES`, whose default is ten times it, since every member list downloads each member's avatar: a client shrinks a photo to the size it shows, such as 512 pixels square, which a JPEG or a WebP holds in about 100 kilobytes, and a PNG in at most about 800 (D121). The API reads no image's dimensions, since it never decodes one (D109).
<!-- end part: files -->

### Email verification

`email_verified` starts as `false`, and nothing enforces it in this slice, though ACCOUNTS-001 returns it. What it decides, once the full `auth` module arrives, is in the [auth spec](auth.md#email-verification).

### Deleting an account

`auth` confirms a deletion, as AUTH-023 says, opens the unit of work, and calls `Deletion::delete`, which:

1. Holds the user's row `FOR NO KEY UPDATE`, against every other change of it. When another deletion of the account deleted the row while this one waited for it, this deletion finds none, and is done (ACCOUNTS-019).
2. Has `access` lock every membership of the user, `FOR UPDATE`, and refuse the deletion while a workspace they own has another member (ACCOUNTS-012, ACCESS-008).
3. Runs each module's personal data contract, in the order the composition root registers them, each erasing what its module keeps of the user, as the table below says (D31). The deletion is immediate, with no grace period (D16).
4. Runs the contract of `accounts` last, which reads the user's email, enqueues the email of ACCOUNTS-015, locks the user's row `FOR UPDATE`, and deletes it, waiting for no lock (ACCOUNTS-016). The foreign keys then delete, or set to null, every row that still names the user (DATA-05).

| Contract | What it erases |
|---|---|
| `workspaces` | Has `access` end each membership of the user, with its access-change signal (ACCESS-006), then deletes each workspace they owned, whose only member they were, with everything in it, through its foreign keys, and the jobs enqueued for it (DATA-23). In each other workspace they belonged to, it records `member.left` and publishes it (ACCOUNTS-014). |
| `access` | Any membership of the user that remains, each with its access-change signal (ACCESS-006): none, once the contract of `workspaces` has run. |
| `files` | The user's files and avatar, whose bytes FILES-011 deletes. A file they uploaded to a workspace that stays keeps its row, and its foreign key sets its `uploader_id` to null as the user's row goes (ACCOUNTS-017). <!-- part: files --> |
| `invitations` | Nothing of its own: an invitation the user sent stays, the workspace's, and its foreign key sets its `inviter_id` to null as the user's row goes. An invitation addressed to their email is the workspace's too, and stays (ACCOUNTS-018, D31). <!-- part: invitations --> |
| `auth` | The user's identities, their sessions, each with its access-change signal, which closes its streams (SEC-06), and their device tokens. |
| `audit` | Nothing of its own: the foreign keys delete the user's own log, and anonymize the entries of other logs that name them, as the user's row goes (AUDIT-006). |
| `platform` | The jobs enqueued for the user. Their events and idempotency records go with the user's row, through their foreign keys. |
| `accounts` | The user's row, last, as step 4 says. |

What the user made in a workspace they share with others is the workspace's, as their export leaves it out (D31): it stays, and keeps nothing that names them (ACCOUNTS-014). The deletion records nothing in the user's own log, which goes with the account; its entries in the logs of the workspaces they shared remain. A user who shared no workspace leaves no row that names them behind: but for the job of ACCOUNTS-015's email, which names no one, until it is sent, and the rate-limit buckets that counted their requests, which hold only keyed hashes and expire on their own (DATA-23), nothing else is left but the request's log line and trace, which hold their ID alone (OBS-04).
The bytes of their files and avatar stay in the bucket until FILES-011 deletes them, as every object that no row names. <!-- part: files -->

The steps take their locks in the order that every other operation takes them on the same rows, so no operation waits for the deletion while the deletion waits for it (DATA-20):

- A rename or an avatar's change locks the user's row `FOR UPDATE` before anything else, and then may lock other rows, such as a workspace's through the foreign key of an event it publishes there. The deletion holds the user's row first, `FOR NO KEY UPDATE`, which such a change waits for, before it locks anything else.
- That mode doesn't conflict with `FOR KEY SHARE`, which the foreign key of every insert that names the user reads. So an operation that locks one of the user's rows and only then inserts a row that names them, such as a refresh, which rotates its session and then records its entry, or a sign-in, which locks the identity and then starts a session, never waits for the deletion's first lock.
- An operation in a workspace first locks its caller's membership (PLATFORM-009), and joining a workspace locks its owner's (ACCESS-008). So the deletion locks the memberships next, then deletes the user's rows that operations lock before they name the user, waiting for any operation that holds one.
- Its last step upgrades its lock on the user's row to `FOR UPDATE`, which waits for the operations that hold the row `FOR KEY SHARE`: each of them has inserted a row that names the user, and waits for none of the deletion's locks. Once the deletion holds that lock, no new row can name the user.

A request of the user's own that started before that lock can still have added a row that names them after the deletion passed its table, such as the membership of a workspace they created, and a further request can lock that row and then wait for the user's row. So, once it holds the user's row, the deletion waits for no lock: the deletion of that row, which its foreign keys carry to the rows that name the user, ends at the first lock another request holds. A membership that remains, whose foreign key, `workspace_memberships_user_id_fkey`, keeps it from going with the user, refuses it too. Both answer ACCOUNTS-016's 409, and the unit of work rolls back, rather than holding the user's row while it waits for another request.

### Export

Export arrives with the module's next part, through the personal data contract (D31). It covers the same workspaces that a deletion deletes: the content of every workspace whose only member is the user is part of their export, and the content of every other workspace isn't (D16). In the personal shape, that is everything the user made ([architecture §5](../architecture.md#workspace-shapes)).

## Open design questions

None.
