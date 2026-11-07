# Spec: `files`

## Purpose

`files` keeps the files that a workspace's members upload: their bytes in object storage, and what the API knows of each in Postgres. It accepts only the types it can check, scans each upload for malware before storing it, serves each file so that it can't run in a browser, and deletes a file's bytes only once no restore of the database can bring its row back. This spec covers a workspace's files and the deletion of bytes. A user's own files, public files and signed download URLs arrive with the next part of the module.

## Owns

| Table | Holds |
|---|---|
| `workspace_files` | `id`, `workspace_id`, `name`, `media_type`, `size_bytes`, `object_key`, `uploader_id`, `created_at`. The database generates the `id` (DATA-08), and deleting the workspace deletes its files' rows (DATA-05). `object_key` is unique. `uploader_id` references `users` with `ON DELETE RESTRICT`, until the full `accounts` module's erasure clears it (D31). Rows are never updated, so the table has no `updated_at` or `version`, and `app` holds no `UPDATE` on it (DATA-02, DATA-19). |
| `object_deletions` | `id`; `object_key`, the key of an object that no file's row names, which is unique; `deletable_at`, when its bytes may go; and `created_at`. Rows are never updated, and `app` holds no `UPDATE` on it (DATA-02, DATA-19). |

## Interface

None. No other module uses `files` yet.

## Routes

| Method and path | Access | Behaviors |
|---|---|---|
| `POST /v1/workspaces/{workspace_id}/files` | `workspace::<FilesUpload>` | FILES-001 to FILES-006, FILES-012, FILES-013 |
| `GET /v1/workspaces/{workspace_id}/files` | `workspace::<FilesRead>` | FILES-007 |
| `GET /v1/workspaces/{workspace_id}/files/{file_id}` | `workspace::<FilesRead>` | FILES-008 |
| `GET /v1/workspaces/{workspace_id}/files/{file_id}/content` | `workspace::<FilesRead>` | FILES-009, FILES-012 |
| `DELETE /v1/workspaces/{workspace_id}/files/{file_id}` | `workspace::<FilesDelete>` | FILES-010 |

`POST /v1/workspaces/{workspace_id}/files` accepts an `Idempotency-Key` (API-12). Its body is the file's bytes, the only kind of request body that isn't JSON (API-08), which it reads through `Upload` with a limit of `FILE_MAX_BYTES` (PLATFORM-074).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| FILES-001 | Uploading a file, with its bytes as the body and its name in `Content-Disposition`, stores it in the workspace, and answers 201 with `Location` and `{id, workspace_id, name, media_type, size_bytes, uploader_id, created_at}`, `media_type` being the type its accepted kind stores (FILES-003) and `uploader_id` the caller. It records `file.uploaded` in the workspace's log, and publishes the workspace event `file.uploaded`, which requires `files.read`. Neither holds the name. A retry with the same `Idempotency-Key` stores one file. The owner, admins and members may upload. | API-04, API-08, API-12, DATA-21, DATA-22, SEC-18, SEC-19 | `files_001_` |
| FILES-002 | A file's name comes from `Content-Disposition`'s `filename*`, as RFC 8187 encodes it in UTF-8, or else from its `filename`. It is trimmed, has internal whitespace collapsed to single spaces, is normalized to Unicode Normalization Form C (NFC), and must be 1 to 255 characters, with no control character. A request without a name, or whose name the rule refuses, answers 400 `files.invalid_name`. | D25, API-10 | `files_002_` |
| FILES-003 | A file is accepted only when its name's extension, ignoring case, is one of an accepted kind's (Design: Accepted kinds) and its bytes are of that kind, as the kind's check finds them. Otherwise the upload answers 422 `files.unsupported_type`, and stores nothing. The body is sent as `application/octet-stream` (PLATFORM-074), so the kind comes from the name and the bytes alone. | D63 | `files_003_` |
| FILES-004 | A body of more than `FILE_MAX_BYTES` (1 to 104,857,600, default 10,485,760) answers 413 `request.payload_too_large`, whether its `Content-Length` declares it or it grows past the limit while it is read (PLATFORM-048, PLATFORM-050). An empty body answers 400 `files.empty`. | API-08, D63 | `files_004_` |
| FILES-005 | Every upload is scanned for malware before it is stored. A file the scanner finds infected answers 422 `files.malware_detected`, stores nothing, and counts in `files.uploads` as `malware_detected` (FILES-013). While the scanner doesn't answer, an upload answers 503 `files.scanner_unavailable`, with `Retry-After`, and stores nothing. | D63, ERR-03 | `files_005_` |
| FILES-006 | An upload checks that the caller may upload before it reads the body, and refuses a caller as ACCESS-003 says, such as one who isn't a member with 404 `request.not_found`, with the body unread. The upload's unit of work authorizes the caller again before it records the file. | ACCESS-003, DATA-21 | `files_006_` |
| FILES-007 | Listing the workspace's files gives each as FILES-001 does, the latest uploaded first, in cursor pages. Its `search` matches the name. | API-05, API-06, API-20, DATA-16, DATA-17 | `files_007_` |
| FILES-008 | Reading a file answers 200 with the file as FILES-001 gives it. A file ID that names no file of the workspace, or isn't a UUID, answers 404 `files.not_found`, here and in FILES-009 and FILES-010. | API-04, DATA-16, PLATFORM-072 | `files_008_` |
| FILES-009 | Downloading a file answers 200 with its bytes, `Content-Type` its media type, `Content-Length` its size, `Content-Disposition` `inline` for a kind that Accepted kinds serves inline and `attachment` for every other, with its name, `X-Content-Type-Options: nosniff`, `Content-Security-Policy: default-src 'none'; sandbox` and `Cache-Control: no-store`. A file whose bytes the bucket doesn't hold answers 500 `request.internal_error`, whose cause the request log writes with the file's ID (ERR-10). | SEC-11, SEC-12, ERR-10, D63 | `files_009_` |
| FILES-010 | Deleting a file deletes its row and answers 204. It records `file.deleted` in the workspace's log, and publishes the workspace event `file.deleted`, which requires `files.read`. Its bytes stay until FILES-011 deletes them. The owner and admins may delete; a member gets 403 `request.permission_denied`. | API-04, DATA-06, DATA-22, SEC-19 | `files_010_` |
| FILES-011 | An object that no file's row names, whatever removed or never committed its row, is deleted from storage once `FILE_DELETION_DELAY_DAYS` (1 to 3,650, default 35) have passed since the periodic job `files.find_unreferenced_objects` first found it so, and never while a row names it. | D1, D30, OPS-15 | `files_011_` |
| FILES-012 | While the storage doesn't answer, a route that reads or writes bytes answers 503 `files.storage_unavailable`, with `Retry-After`. | ERR-03, ERR-04 | `files_012_` |
| FILES-013 | `files.uploads` counts the uploads that reached the check of their bytes, by outcome: `stored`, `unsupported_type`, `malware_detected`, or `failed`. | OBS-11, OBS-14 | `files_013_` |

## Design

### Accepted kinds

Requirement 5.1.1 of the Application Security Verification Standard (ASVS) asks each upload to document the types it accepts, their extensions, its size limit, and how the files are made safe to download. Every upload accepts these kinds, at most `FILE_MAX_BYTES` each (D95), and never decompresses a file, so requirement 5.2.3, on the size of what a compressed file unpacks to, has nothing to check. Each kind is served as Downloading says, and a file the scanner finds infected is refused, never stored (FILES-005).

| Kind | Extensions | Stored media type | Its bytes | Served |
|---|---|---|---|---|
| PNG | `png` | `image/png` | start with PNG's signature | inline |
| JPEG | `jpg`, `jpeg` | `image/jpeg` | start with JPEG's signature | inline |
| GIF | `gif` | `image/gif` | start with GIF's signature | inline |
| WebP | `webp` | `image/webp` | start with the signature of the Resource Interchange File Format (RIFF), of the form `WEBP` | inline |
| PDF | `pdf` | `application/pdf` | start with `%PDF-` | attachment |
| Word | `docx` | `application/vnd.openxmlformats-officedocument.wordprocessingml.document` | a ZIP archive from its first byte, whose central directory names `[Content_Types].xml` and `word/document.xml` | attachment |
| Excel | `xlsx` | `application/vnd.openxmlformats-officedocument.spreadsheetml.sheet` | as Word, with `xl/workbook.xml` | attachment |
| PowerPoint | `pptx` | `application/vnd.openxmlformats-officedocument.presentationml.presentation` | as Word, with `ppt/presentation.xml` | attachment |
| Text | `txt` | `text/plain; charset=utf-8` | UTF-8, with no control character but tab, line feed and carriage return | attachment |
| CSV | `csv` | `text/csv; charset=utf-8` | as Text | attachment |
| Markdown | `md` | `text/markdown; charset=utf-8` | as Text | attachment |

The list lives in `model/kinds.rs` as one table, so a fork adds a kind as one row and its check. A kind is added only when its bytes can be checked: Scalable Vector Graphics (SVG) and HTML are left out, since a browser runs the scripts they hold, and a format that a check can't tell from any other bytes is left out too.

### Storage

`platform::storage` holds the client of an S3-compatible bucket, bundled on the server or a provider's, as the deployment chooses (D98, [architecture §9](../architecture.md#9-runtime-topology)), which the settings `S3_ENDPOINT`, `S3_REGION`, `S3_BUCKET`, `S3_ACCESS_KEY_ID`, `S3_SECRET_ACCESS_KEY`, `S3_URI_STYLE` and `S3_CA_PATH` configure, each required but the last two: `S3_URI_STYLE` is `host` unless it is `path`, and `S3_CA_PATH` names a file of certificates that a bucket of the operator's own may chain to (OPS-01, SEC-01, PLATFORM-076). The endpoint must be `https` outside development and test (SEC-10). The client reports a bucket that doesn't answer as unavailable, and every other failure as internal, neither naming the key (ERR-04). Readiness doesn't wait on the bucket: replicas that couldn't reach it would all leave the load balancer, and every route would fail with them, rather than only those of FILES-012.

An object's key is 128 random bits from the operating system's source, in hexadecimal, under the prefix `workspaces/`, so it names no row and reveals nothing (DATA-08, CODE-06). An object is written once and never replaced.

### Uploading

1. The handler refuses a body whose `Content-Length` declares more than `FILE_MAX_BYTES` (FILES-004), then checks that the caller may upload, in a short unit of work of its own that authorizes them and commits nothing, and records a refusal as any workspace route does (FILES-006, D96).
2. It reads the name from `Content-Disposition` (FILES-002), then the body, in one of the replica's turns (PLATFORM-074).
3. It finds the kind from the name's extension, and checks the bytes against it (FILES-003), then sends them to the scanner (FILES-005).
4. It writes the object, outside any transaction (DATA-21).
5. The unit of work opens with `Authorize<FilesUpload>`, which authorizes the caller again, claims the `Idempotency-Key`, inserts the row, records the entry and publishes the event, then commits (FILES-001).

An upload that stops after its object is written, whether a refusal at step 5, a failed commit or a stopped replica, leaves an object no row names, which FILES-011 deletes. The handler doesn't delete it itself: when a commit's answer is lost, the row may exist, and deleting its object would leave a row without bytes. A retry whose `Idempotency-Key` already committed writes its object, then answers the first result, leaving its own object to FILES-011 too.

The fingerprint of a keyed upload covers a SHA-256 digest of its body, as of any keyed route (API-12).

### Scanning

The scanner is ClamAV's daemon, `clamd`, which the setting `CLAMD_ADDRESS`, a host and a port, names, and which the upload reaches over TCP with the `INSTREAM` command, sending the bytes it holds. The daemon decompresses the archives it scans within limits of its own configuration, which bound what a crafted archive can make it do. `clamd` refreshes its signatures through `freshclam`, so a deployment runs it with network access to ClamAV's mirrors.

### Downloading

The download reads the row through the workspace from the proof, then streams the object from the bucket, so a replica holds at most a chunk of it at a time. `Content-Disposition` gives the name twice: as `filename*`, the name in UTF-8 percent-encoded as RFC 8187 says, and as `filename`, an ASCII fallback in which every other character, `"` and `\` become `_`, as RFC 6266 recommends (ASVS 5.4.2). The API serves no ranges, so `Range` is ignored, as RFC 9110 allows.

### Deleting bytes

The bytes of a file outlive its row, so that a restore of the database to a moment before the row's deletion finds them (D93). A restore of the bundled database reaches back `BACKUP_RETENTION_DAYS` days, and up to 7 more, since a full backup expires only once a newer one, of the next week's, is that old ([backups.md](../operations/backups.md#what-is-backed-up)), so the deployment sets the delay to the retention plus 7 days, and the default, 35 days, covers the default retention of 28. With a managed database, `FILE_DELETION_DELAY_DAYS` is set to its provider's recovery window or more.

No statement deletes bytes when it deletes a row: a row may also go with its workspace's deletion, which cascades (DATA-05), or never commit, and the schema holds no trigger that could record either (DATA-07). Instead, two periodic jobs compare the bucket with the rows:

- `files.find_unreferenced_objects`, every day, lists the bucket's objects, and records in `object_deletions` each that no row names, with `deletable_at` `FILE_DELETION_DELAY_DAYS` days from now, by the database's clock (DATA-03). An object already recorded keeps its time.
- `files.delete_unreferenced_objects`, every hour, takes the recorded objects whose time has passed, in batches of at most 1,000, deletes from storage each that still no row names, and removes each from `object_deletions`, whichever it did. An object that a row names again, as after a restore, stays.

Both span every workspace by nature, so they select objects by key alone (DATA-16), and scheduled maintenance isn't audited (DATA-22). After a restore to an earlier moment, the objects of uploads made since then have no row, and go after the delay; `object_deletions` may have lost what it recorded since then, which the next run records again.

### Recording and events

Every route is a workspace route, so `Authorized<P>` proves the caller's membership and gives the workspace's ID, by which every statement filters, a file's ID among them, so a file of another workspace is never found (DATA-16). An upload and a deletion each record their entry and publish their event in the unit of work that changes the row (D1). A name is text a user writes, so it is personal data: the entries' `details` are empty, their resource being the file, and the events' payloads carry only IDs (SEC-18, API-19).

## Open design questions

| Question | Settled by |
|---|---|
| How an avatar and a workspace's picture are stored: as files of their user or workspace, which the lists and the deletion of files then leave out, or as objects of their own, and which kinds they accept | the spec of the full `accounts` module |
| How the avatar that an OAuth provider gives a new account is fetched and stored, through the untrusted client (SEC-10) | the spec of the full `auth` module, with the first OAuth provider |
| Whether an image that a browser shows in place is also decoded and written anew, as the cheat sheet on uploads of the Open Worldwide Application Security Project (OWASP) recommends beside its signature, which also drops what the image carries besides its pixels, such as a photo's location, at the cost of CPU-heavy work in each upload | the spec of the next part of `files`, with scanning |
