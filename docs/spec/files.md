# Spec: `files`

## Purpose

`files` holds each workspace's files: its members create, list, read, rename and delete them.

## Owns

| Table | Holds |
|---|---|
| `files` | `id`, `workspace_id`, `name`, `created_at`, `updated_at`, `version`. The database generates the `id` (DATA-08), and deleting the workspace deletes its files (DATA-05). |

## Interface

None. No other module uses `files`.

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `POST /v1/workspaces/{workspace_id}/files` | `workspace::<FilesCreate>` | FILES-001, FILES-002 |
| `GET /v1/workspaces/{workspace_id}/files` | `workspace::<FilesRead>` | FILES-003 |
| `GET /v1/workspaces/{workspace_id}/files/{file_id}` | `workspace::<FilesRead>` | FILES-004 |
| `PATCH /v1/workspaces/{workspace_id}/files/{file_id}` | `workspace::<FilesUpdate>` | FILES-002, FILES-005 |
| `DELETE /v1/workspaces/{workspace_id}/files/{file_id}` | `workspace::<FilesDelete>` | FILES-006 |

`POST /v1/workspaces/{workspace_id}/files` accepts an `Idempotency-Key` (API-12), and `PATCH /v1/workspaces/{workspace_id}/files/{file_id}` an `If-Match` (API-13, PLATFORM-071).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| FILES-001 | Creating a file with `{"name": <name>}` stores it in the workspace, and answers 201 with `Location`, the file's `ETag` (PLATFORM-071), and `{id, name, created_at, updated_at}`. It records `file.created` in the workspace's log, and publishes the workspace event `file.created`, which requires `files.read`. Neither holds the name. A retry with the same `Idempotency-Key` creates one file. | API-04, API-12, DATA-22, SEC-18, SEC-19 | `files_001_` |
| FILES-002 | A file's name is trimmed, has internal whitespace collapsed to single spaces, is normalized to Unicode Normalization Form C (NFC), and must be 1 to 255 characters and not only whitespace. | D25, API-10 | `files_002_` |
| FILES-003 | Listing the workspace's files gives each as FILES-001 does, the latest created first, in cursor pages. Its `search` matches the name. | API-05, API-06, API-20, DATA-16, DATA-17 | `files_003_` |
| FILES-004 | Reading a file answers 200 with the file as FILES-001 gives it, and its `ETag`. A file ID that names no file of the workspace, or isn't a UUID, answers 404 `files.not_found`, here and in FILES-005 and FILES-006. | API-04, API-13, DATA-16 | `files_004_` |
| FILES-005 | Renaming a file with the merge patch `{"name": <name>}` stores the name, normalized as FILES-002 says, and answers 200 with the file as FILES-004 gives it and its new `ETag`. An `If-Match` that names none of its versions answers 412 `request.precondition_failed` and changes nothing. It records `file.renamed` in the workspace's log, and publishes the workspace event `file.renamed`, which requires `files.read`. Neither holds the name. | API-03, API-13, DATA-18, DATA-22, SEC-18, SEC-19 | `files_005_` |
| FILES-006 | Deleting a file deletes its row and answers 204. It records `file.deleted` in the workspace's log, and publishes the workspace event `file.deleted`, which requires `files.read`. The owner and admins may delete; a member gets 403 `request.permission_denied`. | API-04, DATA-06, DATA-22, SEC-19 | `files_006_` |

## Design

Every route is a workspace route, so `Authorized<P>` proves the caller's membership and gives the workspace's ID, by which every statement filters, a file's ID among them, so a file of another workspace is never found (DATA-16). A create, a rename and a delete each record their entry and publish their event in the unit of work that changes the row, then commit (D1).

The list reads a page through the index on the workspace and the creation time (DATA-17), and a search narrows the rows it reaches with the name (PLATFORM-047). A rename is one `UPDATE`, kept to the versions the request's `If-Match` names when it sent one (DATA-18): zero rows updated is either a file that isn't there or the precondition's failure, which a read of the row tells apart before the unit of work rolls back. The update returns the row as it now stands, whose `version` the shared trigger raised (DATA-02).

A name is text a user writes, so it is personal data (the glossary in [architecture](../architecture.md#11-glossary)): the entries' `details` are empty, their resource being the file, and the events' payloads carry only IDs (SEC-18, API-19), so a client that receives one reads the file through its route.

## Open design questions

None.
