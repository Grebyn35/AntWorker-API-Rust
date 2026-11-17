# Spec: `client_telemetry`

## Purpose

`client_telemetry` holds the telemetry of each user's clients, such as a desktop client's traces and logs: whether the user consents to their clients' sending it, which the module stores, and enforces on the server (D7, D122). This spec covers the consent. The telemetry proxy, through which a client sends its telemetry to the deployment's collector, and which [architecture §5](../architecture.md#what-each-module-owns) gives the module too, is specified when it is built.

## Owns

| Table | Holds |
|---|---|
| `telemetry_consents` | One row for each user who chose whether their clients send their telemetry: `id`, `user_id`, which is unique, `consented`, `created_at`, when they first chose, `updated_at`, when they last did, and `version`. `user_id` references `users` with `ON DELETE CASCADE`. A user without a row never chose. |

## Interface

- `Contract`, the personal data contract for `client_telemetry` (D31), which exports the user's consent, and whose erasure deletes it (CLIENT_TELEMETRY-003).

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `GET /v1/users/me/telemetry-consent` | user | CLIENT_TELEMETRY-001 |
| `PUT /v1/users/me/telemetry-consent` | user | CLIENT_TELEMETRY-002 |

`PUT /v1/users/me/telemetry-consent` accepts an `If-Match` (API-13, PLATFORM-071).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| CLIENT_TELEMETRY-001 | `GET /v1/users/me/telemetry-consent` answers 200 with `{"consented": <boolean>}`, the caller's own consent, with its `ETag` (PLATFORM-071), and nothing about any other user. A caller who never chose reads `false`, at version 0, whose `ETag` is `"0"`, and the read writes nothing. | API-02, API-13, DATA-16, D7, D122 | `client_telemetry_001_` |
| CLIENT_TELEMETRY-002 | Choosing, `PUT /v1/users/me/telemetry-consent` with `{"consented": <boolean>}`, stores it as the caller's consent, and answers 200 with the consent as CLIENT_TELEMETRY-001 gives it, and its new `ETag`. It records `telemetry_consent.updated` in the caller's own log, with the user as its resource and `{"consented": <boolean>}` as its details, and publishes the user event `telemetry_consent.updated`, with `{"consented": <boolean>}`, to the caller's own streams. It writes, records and publishes even when the consent is the one stored. An `If-Match` that names no current version answers 412, as PLATFORM-071 says, and `"0"` names the consent of a caller who never chose, until their first choice. A body that lacks `consented`, holds it as anything but a boolean, or holds another member, answers 400 `request.invalid_body`. A refused choice changes nothing. A choice that waits for a deletion of the caller's account, which commits first, at the user's row or at their row of `telemetry_consents`, changes nothing, and answers 401 `request.unauthenticated`, as a request sent after the deletion does (ACCOUNTS-019). | API-03, API-08, API-13, API-19, DATA-18, DATA-22, SEC-19, D122 | `client_telemetry_002_` |
| CLIENT_TELEMETRY-003 | Deleting an account deletes its consent: its erasure deletes the user's row of `telemetry_consents`, waiting for a choice that holds that row, as a choice of a user who has a row does from its second statement on, and the deletion then answers 204. The export of a user's data holds `client_telemetry/telemetry_consents.json`: for a user who chose, one record with `consented`, `created_at` and `updated_at`, and for any other user, none. | D31, DATA-23 | `client_telemetry_003_` |

## Design

### The consent

A user consents to nothing until they choose to, so no client of theirs sends their telemetry before then (D7). The consent is the user's, for every client they sign in to, and each of their clients reads it once the user has signed in: neither `GET /v1/users/me` nor a session's response holds it, since `accounts` and `auth` don't know the part (D122). A client then follows each `telemetry_consent.updated` that reaches its streams, whose payload holds the consent, since the event reaches only the user's own streams (SEC-19), so a choice made on one device reaches the others without a read; a client tells the event of its own choice by its trace ID ([realtime spec](realtime.md#design)). The capabilities' member `client_telemetry` tells a client whether its API has the part at all ([system spec](system.md#capabilities)).

The consent is always written whole, so a choice is a `PUT` of it (API-03). A choice writes, records and publishes even when the consent is the one stored, as a rename does (ACCOUNTS-007), so the caller's own log holds each choice they made, which tells them when they consented (DATA-22).

### Its version

A user who never chose has no row, and their consent reads as `false` at version 0. Their first choice inserts the row of the consent that no one chose, `consented` false at version 0, which the same unit of work then updates, so the trigger moves its version to 1 (DATA-02), and an `If-Match` of `"0"` names the consent until a choice commits. No row stays at version 0: a choice that its `If-Match` refuses rolls the insert back with the rest.

### A choice's unit of work

1. Its first statement inserts the row of the consent that no one chose, unless the user has a row. Its foreign key reads the user's row `FOR KEY SHARE`, which waits for a deletion of the account that holds the row `FOR UPDATE`, the deletion's last step, and then refuses the row once the deletion has committed (ERR-05).
2. The next locks the user's row of `telemetry_consents` `FOR UPDATE`, so two choices of one user take turns (DATA-20), and finds none once a deletion that committed meanwhile took it with the account.
3. The update writes the choice with the versions that `If-Match` names in its `WHERE` (DATA-18).

A refusal of the first two answers 401, as CLIENT_TELEMETRY-002 says, and of the third 412. The entry is recorded, the event is published, and the unit of work commits.

A choice locks its row of `telemetry_consents` before its entry and its event name the user, so a deletion of the account deletes that row in the contract's erasure, waiting for a choice that holds it, as the deletion's lock order asks of such a row ([accounts spec](accounts.md#deleting-an-account)). A choice that inserted the row holds the user's row `FOR KEY SHARE` from then on, which the deletion's last step waits for, as the same order says. A choice that waits for the erasure answers 401 once the deletion commits, as CLIENT_TELEMETRY-002 says.

## Open design questions

None.
