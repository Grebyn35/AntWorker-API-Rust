# Spec: `client_telemetry`

## Purpose

`client_telemetry` holds the telemetry of each user's clients, such as a desktop client's traces and logs: whether the user consents to their clients' sending it, which the module stores, and the telemetry proxy, through which a consenting user's client sends its spans and its log records to the deployment's collector, and which enforces the consent on the server (D7, D122). What a client sends of its metrics is specified when it is built.

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
| `POST /v1/users/me/telemetry/traces` | user | CLIENT_TELEMETRY-004 to CLIENT_TELEMETRY-012 |
| `POST /v1/users/me/telemetry/logs` | user | CLIENT_TELEMETRY-004 to CLIENT_TELEMETRY-012 |

`PUT /v1/users/me/telemetry-consent` accepts an `If-Match` (API-13, PLATFORM-071).

The proxy's two routes take a batch of telemetry, the body of a request of the OpenTelemetry Protocol (OTLP) over HTTP, in place of a JSON body (PLATFORM-090).

## Behaviors

| ID | Behavior | Rules | Test prefix |
|---|---|---|---|
| CLIENT_TELEMETRY-001 | `GET /v1/users/me/telemetry-consent` answers 200 with `{"consented": <boolean>}`, the caller's own consent, with its `ETag` (PLATFORM-071), and nothing about any other user. A caller who never chose reads `false`, at version 0, whose `ETag` is `"0"`, and the read writes nothing. | API-02, API-13, DATA-16, D7, D122, D174 | `client_telemetry_001_` |
| CLIENT_TELEMETRY-002 | Choosing, `PUT /v1/users/me/telemetry-consent` with `{"consented": <boolean>}`, stores it as the caller's consent, and answers 200 with the consent as CLIENT_TELEMETRY-001 gives it, and its new `ETag`. It records `telemetry_consent.updated` in the caller's own log, with the user as its resource and `{"consented": <boolean>}` as its details, and publishes the user event `telemetry_consent.updated`, with `{"consented": <boolean>}`, to the caller's own streams. It writes, records and publishes even when the consent is the one stored. An `If-Match` that names no current version answers 412, as PLATFORM-071 says, and `"0"` names the consent of a caller who never chose, until their first choice. A body that lacks `consented`, holds it as anything but a boolean, or holds another member, answers 400 `request.invalid_body`. A refused choice changes nothing. A choice that waits for a deletion of the caller's account, which commits first, at the user's row or at their row of `telemetry_consents`, changes nothing, and answers 401 `request.unauthenticated`, as a request sent after the deletion does (ACCOUNTS-019). | API-03, API-08, API-13, API-19, DATA-18, DATA-22, SEC-19, D122, D174 | `client_telemetry_002_` |
| CLIENT_TELEMETRY-003 | Deleting an account deletes its consent: its erasure deletes the user's row of `telemetry_consents`, waiting for a choice that holds that row, as a choice of a user who has a row does from its second statement on, and the deletion then answers 204. The export of a user's data holds `client_telemetry/telemetry_consents.json`: for a user who chose, one record with `consented`, `created_at` and `updated_at`, and for any other user, none. | D31, DATA-23 | `client_telemetry_003_` |
| CLIENT_TELEMETRY-004 | `POST /v1/users/me/telemetry/traces` and `POST /v1/users/me/telemetry/logs` take a batch of a client's spans, or of its log records, as the body of an OTLP/HTTP request, sent as `application/x-protobuf` or as `application/json`, with `Content-Encoding: gzip` or without one. For a caller who consents, the API sends the body, inflated when it came as gzip, to the collector: a `POST` of `<CLIENT_TELEMETRY_ENDPOINT>/v1/traces` or `<CLIENT_TELEMETRY_ENDPOINT>/v1/logs`, with the body's media type as its `Content-Type`, and without the caller's `Authorization` or `Cookie`. Once the collector answers 2xx, the API answers 200 with the protocol's full success, in the media type of the request: no bytes as `application/x-protobuf`, and `{}` as `application/json`. | API-02, D7, D175, SEC-10 | `client_telemetry_004_` |
| CLIENT_TELEMETRY-005 | With the setting, a batch that its route read, from a caller whose consent, as CLIENT_TELEMETRY-001 reads it, isn't `true`, one who never chose or whose last choice was `false`, answers 403 `client_telemetry.consent_required` on either route: it counts in no budget, and nothing reaches the collector. One user's consent admits no other user's batch. | D7, D122, D175, TEST-02 | `client_telemetry_005_` |
| CLIENT_TELEMETRY-006 | Either route first reads its request's body as a batch, whoever its caller is: it answers 400 `request.invalid_body` when the `Content-Type` is neither media type, when the `Content-Encoding` is anything but `gzip` or `identity`, and when gzip doesn't inflate, and 413 `request.payload_too_large` when the body holds more than 1,048,576 bytes, as sent or once inflated (PLATFORM-090). None of these reaches the collector, or counts in a budget. | API-08, D175 | `client_telemetry_006_` |
| CLIENT_TELEMETRY-007 | A consenting caller's batch counts its bytes, inflated, in its caller's budget, `telemetry_user`: `CLIENT_TELEMETRY_USER_BYTES_PER_MINUTE` bytes in each window of 60 seconds, for both routes and every session of the user, whichever replica serves them (PLATFORM-089). A batch that takes the count beyond the budget answers 429 `request.rate_limited`, with `Retry-After` and with the budget in `RateLimit` and `RateLimit-Policy`, whose item holds `qu="content-bytes"`, and doesn't reach the collector. It stays counted, so the window refuses the user's later batches too. Each user has a budget of their own. | API-15, D176 | `client_telemetry_007_` |
| CLIENT_TELEMETRY-008 | A batch that its caller's budget takes then counts its bytes in the replica's budget, `telemetry_replica`: `CLIENT_TELEMETRY_REPLICA_BYTES_PER_MINUTE` bytes in each window of 60 seconds, for every user's batches that the replica serves, counted in the replica's memory, so another replica's window is its own (PLATFORM-089). A batch that takes the count beyond the budget answers 429 as CLIENT_TELEMETRY-007's does, with this budget in its headers, and doesn't reach the collector. It stays counted in the replica's window, which refuses the replica's later batches too, and in its caller's budget. | API-15, D176 | `client_telemetry_008_` |
| CLIENT_TELEMETRY-009 | A batch that the collector refuses, with a 4xx other than 429, answers 400 `client_telemetry.batch_refused`, as the stack's collector refuses a body that isn't the protocol's request. A batch that the collector doesn't take otherwise answers 503 `client_telemetry.collector_unavailable`, with `Retry-After`: when no connection opens, when no answer comes within 5 seconds, and when the collector answers anything else but 2xx, such as 503 (PLATFORM-091). A batch that the collector refuses has counted in its caller's budget. | ERR-03, ERR-04, D175 | `client_telemetry_009_` |
| CLIENT_TELEMETRY-010 | Without `CLIENT_TELEMETRY_ENDPOINT`, the deployment forwards no client's telemetry: both routes answer 503 `request.feature_disabled`, with a `Retry-After` of an hour, to each batch that they read, whatever its caller's consent, and the capabilities' `features.client_telemetry` is `false`, where it is `true` with the setting. The consent's routes serve either way. `serve` refuses a `CLIENT_TELEMETRY_ENDPOINT` that isn't an `http` or `https` URL with a host, and no user, password, query or fragment. | OPS-01, OPS-04, D175 | `client_telemetry_010_` |
| CLIENT_TELEMETRY-011 | In the stack that Compose runs, a consenting user's spans and log records, which a client sends through the image's API as the service `app-desktop`, reach Tempo and Loki. The spans' resource holds the `service.name`, the `service.version` and the `service.instance.id` that the client sent, and the log records' resource its `service.name` and its `service.version`; neither holds another attribute of the resource it sent, such as `host.name`. | D7, D177 | `client_telemetry_011_` |
| CLIENT_TELEMETRY-012 | In the same stack, the collector stores nothing of a resource whose `service.name` isn't one that its [configuration of clients' telemetry](../../deploy/observability/collector/clients.yaml) lists, such as one that names the API itself, or one without a name, and the batch still answers 200. | D177 | `client_telemetry_012_` |

## Design

### The consent

A user consents to nothing until they choose to, so no client of theirs sends their telemetry before then (D7). The consent is the user's, for every client they sign in to, and each of their clients reads it once the user has signed in: neither `GET /v1/users/me` nor a session's response holds it, since `accounts` and `auth` don't know the part (D122). A client then follows each `telemetry_consent.updated` that reaches its streams, whose payload holds the consent, since the event reaches only the user's own streams (SEC-19), so a choice made on one device reaches the others without a read; a client tells the event of its own choice by its trace ID ([realtime spec](realtime.md#design)). The capabilities' member `client_telemetry` tells a client whether its deployment forwards its telemetry, and is absent from an API that doesn't have the part at all ([system spec](system.md#capabilities)).

The consent is always written whole, so a choice is a `PUT` of it (API-03). A choice writes, records and publishes even when the consent is the one stored, as a rename does (ACCOUNTS-007), so the caller's own log holds each choice they made, which tells them when they consented (DATA-22).

### Its version

A user who never chose has no row, and their consent reads as `false` at version 0. Their first choice inserts the row of the consent that no one chose, `consented` false at version 0, which the same unit of work then updates, so the trigger moves its version to 1 (DATA-02), and an `If-Match` of `"0"` names the consent until a choice commits. No row stays at version 0: a choice that its `If-Match` refuses rolls the insert back with the rest.

### A choice's unit of work

1. Its first statement inserts the row of the consent that no one chose, unless the user has a row. Its foreign key reads the user's row `FOR KEY SHARE`, which waits for a deletion of the account that holds the row `FOR UPDATE`, the deletion's last step, and then refuses the row once the deletion has committed (ERR-05).
2. The next locks the user's row of `telemetry_consents` `FOR UPDATE`, so two choices of one user take turns (DATA-20), and finds none once a deletion that committed meanwhile took it with the account.
3. The update writes the choice with the versions that `If-Match` names in its `WHERE` (DATA-18).

A refusal of the first two answers 401, as CLIENT_TELEMETRY-002 says, and of the third 412. The entry is recorded, the event is published, and the unit of work commits.

A choice locks its row of `telemetry_consents` before its entry and its event name the user, so a deletion of the account deletes that row in the contract's erasure, waiting for a choice that holds it, as the deletion's lock order asks of such a row ([accounts spec](accounts.md#deleting-an-account)). A choice that inserted the row holds the user's row `FOR KEY SHARE` from then on, which the deletion's last step waits for, as the same order says. A choice that waits for the erasure answers 401 once the deletion commits, as CLIENT_TELEMETRY-002 says.

### The proxy

The proxy makes the API the OTLP/HTTP endpoint of its users' clients, so a client holds no address and no credential of the collector: its exporter posts each batch to the API, with its user's session, as the client sends any other request (D175). Its routes are under `/v1/users/me/`, as the caller's own resources are (API-02): a batch is its caller's, counts in its caller's budget, and needs its caller's consent. An exporter takes each route as the endpoint of its signal, such as the `OTEL_EXPORTER_OTLP_TRACES_ENDPOINT` of OpenTelemetry's SDKs, since the two paths aren't the protocol's `/v1/traces` and `/v1/logs` under one base.

A request and its success are the protocol's: the two media types, gzip, and a 200 that holds the full success in the request's media type (CLIENT_TELEMETRY-004). A failure is a problem, as every route's is (API-07), where the protocol asks for its own `Status` message, and its status says what the protocol's does: an exporter sends a batch again after a 429 or a 503, once `Retry-After` has passed, and after no other failure.

The API doesn't parse a batch. It forwards the bytes, and the collector decides what it keeps of them ([below](#what-the-collector-keeps)), so the API holds no decoder of the protocol's messages, and a batch that isn't the protocol's request is refused by the collector (CLIENT_TELEMETRY-009). It answers the full success itself, rather than relaying the collector's body, so its answer is the same whatever collector a deployment runs.

A client sends nothing while its user doesn't consent ([the consent](#the-consent)), and the API forwards nothing of a batch that one sends anyway (CLIENT_TELEMETRY-005, D7).

### A batch's steps

1. The pipeline counts the request in its client IP's bucket, authenticates it, and counts it in its user's bucket, as it does any request (PLATFORM-054).
2. The route reads the body, up to 1,048,576 bytes, and inflates one that came as gzip, up to as many (CLIENT_TELEMETRY-006). A replica reads at most 8 batches at once, each in a turn that it holds until the collector has answered, so the batches in its memory take at most 8 times that, twice over while they inflate, whatever the concurrency limit lets in (PLATFORM-090).
3. Without `CLIENT_TELEMETRY_ENDPOINT`, the request ends with 503 (CLIENT_TELEMETRY-010).
4. The service reads the caller's consent, as CLIENT_TELEMETRY-001 does, and refuses a caller who doesn't consent (CLIENT_TELEMETRY-005).
5. It counts the bytes in the caller's budget, then in the replica's (CLIENT_TELEMETRY-007, CLIENT_TELEMETRY-008).
6. It forwards the batch, and answers as the collector took it (CLIENT_TELEMETRY-004, CLIENT_TELEMETRY-009).

The route reads the body before the service decides anything, since a handler calls one service function (ARCH-08), which takes the bytes; what a caller who doesn't consent sends is read within the same bounds, and dropped. The consent's read is the operation's one unit of work, which writes nothing: it decides whether the budgets count the batch at all, as the one read that ARCH-11 allows before a limit counts, so the batch of a caller who doesn't consent counts in none. A withdrawal that commits after the fourth step doesn't stop the batch: the proxy reads the consent once.

### The budgets

A user's budget bounds what one account can send, and the replica's what every account together can, such as the accounts that one person registers (D176). Both count a batch's bytes once inflated, which is what the collector and its stores take in.

The user's budget counts in the database, as every limit of one subject does (PLATFORM-055), so it is one budget whichever replica a batch reaches. The replica's counts in the replica's memory: the deployment forwards at most that many bytes a minute for each replica it runs. One budget for the whole deployment would be one row that every batch of every user updates, each waiting for the last one's commit.

Each setting is at least 1,048,576, the largest batch, so no batch is larger than a whole window. A batch that a budget refuses stays counted in it, as a refused request does in a bucket (PLATFORM-055), so the window refuses what comes after it too: an exporter waits for `Retry-After` before it sends again. A batch that the replica's budget refuses has counted in its user's first, and one that the collector doesn't take has counted in both.

### What the collector keeps

The collector takes the batches that the API forwards on a receiver of their own, and passes them through pipelines of their own, which [observability.md](../operations/observability.md#how-telemetry-flows) describes (D177). It keeps the spans and the log records of the services that its [configuration of clients' telemetry](../../deploy/observability/collector/clients.yaml) lists, which is `app-desktop` in the template, and a fork lists each of its clients there. It keeps of each resource only the attributes that CLIENT_TELEMETRY-011 names, so a store holds nothing that an SDK adds about the user's machine, such as its host's name. A batch from a service that isn't listed answers 200 all the same: the API doesn't read the batch, and the collector answers the full success for what it drops (CLIENT_TELEMETRY-012).

## Open design questions

None.
