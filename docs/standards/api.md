# API rules (`API`)

The HTTP contract every route follows. The generated OpenAPI document is the authoritative description of each route, and these rules govern its shape.

## Paths and methods

### API-01 Paths are versioned, plural and lowercase
Every route lives under `/v1`, except the health probes `/health/live` and `/health/ready` (OPS-05), which load balancers call without a version, and the API reference that development serves at `/docs` (PLATFORM-052), which no client is built against. Collections are plural nouns, and segments of more than one word use kebab-case (`/order-items`). Path parameters are named `{<resource>_id}`.
- Why: a path reads as a predictable address for a resource (D8).
- Check: `ci: check-openapi` (a path lint over the committed document).

### API-02 The scope of a resource is in its path
Workspace resources live under `/v1/workspaces/{workspace_id}/`, and the caller's own resources under `/v1/users/me/`.
A public file's bytes, and an avatar's, which anyone may download without signing in, live under `/v1/public/`, which holds nothing else, and whose requests count in a bucket of the IP limit of their own (PLATFORM-082). <!-- part: files -->
No header selects a workspace.
- Why: every request names exactly what it acts on, and authorization checks that name (D3).
- Check: `ci: check-openapi`, plus `clippy` (API-14).

### API-03 Methods carry their HTTP meaning
- `GET` reads, and is safe and idempotent.
- `POST` creates a resource, or runs an action on a named sub-resource (`/transfer`, `/accept`).
- `PATCH` updates part of a resource using JSON Merge Patch (RFC 7396).
- `PUT` replaces a whole resource, and is used only where a resource is always written whole.
- `DELETE` removes.
- Why: clients, proxies and caches rely on these meanings (D8).
- Check: `ci: check-openapi`, plus `review`.

### API-04 Status codes follow the outcome
| Outcome | Status |
|---|---|
| A read, or an update that returns the resource | 200 |
| A range of a download's bytes, with `Content-Range` (PLATFORM-081) | 206 |
| A download whose bytes the client holds, as its `If-None-Match` names them (PLATFORM-081) | 304 |
| A create, with a `Location` header and the created resource | 201 |
| Asynchronous work accepted, such as an export of a user's data | 202 |
| A delete, or an action with nothing to return | 204 |
| An error | from its category (ERR-03) |

- Why: the status alone tells the client what happened (D8).
- Check: `test`, plus `ci: check-openapi`.

## Bodies

### API-05 Responses are the resource itself
A success body is the resource object, with no envelope. A list is `{"items": [...], "next_cursor": "..." | null}`. Fields are `snake_case`. Clients ignore fields they don't recognize.
- Why: one shape for every resource and every list (D8).
- Check: `ci: check-openapi`.

### API-06 Lists use opaque cursors
Every list is paginated by keyset. `limit` defaults to 50 and is capped at 100. `cursor` is an opaque base64url string that encodes the full sort key, with the row's ID as the tiebreaker. Lists move forward only, unless a module spec defines backward paging for a route.
- Why: pages stay stable while rows are added, and no row is skipped or repeated (D8, DATA-17).
- Check: `ci: check-openapi` (a list takes `limit` and `cursor`), plus `test` (each list has a test that pages across rows sharing a timestamp).

### API-07 Errors are RFC 9457 problems
An error response, except from the health probes (OPS-05), has `Content-Type: application/problem+json` and these members:
- `type`: `/problems/<code>`.
- `title`: a short summary.
- `status`: the HTTP status.
- `detail`: a human-readable explanation, when there is one (ERR-09).
- `code`: the stable error code (ERR-02).
- `trace_id`: the request's trace ID.
- `errors`: present only for validation failures. A list of `{field, code, message}`, where `field` is a JSON Pointer to the invalid input: into the body, or, for `request.invalid_query`, into the query's parameters, such as `/limit`.
- Why: one standard format that every client parses the same way (D8, D10).
- Check: `compiler` (ERR-07), plus `test`.

### API-08 Request bodies are strict
JSON bodies reject unknown fields. Each route group has a body size limit, and an oversized body gets the 413 problem before it is buffered.
Uploads are the only non-JSON bodies: an upload route's group has no body limit of its own, since its handler refuses a caller before it reads the body, which it reads through `Upload` with the limit its module sets, such as a setting's, refusing an oversized body as a group's limit would (PLATFORM-074). <!-- part: files -->
- Why: a mistyped field fails loudly instead of being silently ignored (D8, D12).
- Check: `ci: check-layers` (every type in a module's routes that derives `Deserialize` carries `#[serde(deny_unknown_fields)]`), plus `test`.

### API-09 Query parameters are strict
Unknown query parameters are rejected with 400. Filters are explicit, named parameters. Booleans are `true` or `false`.
- Why: a misspelled filter must never return unfiltered data (D8).
- Check: `ci: check-layers` (the API-08 check covers query types), plus `test`.

### API-10 Input becomes domain types at the boundary
A module's routes convert each request's input into the domain types its service takes, and report every value they refuse in one response (API-07): a body's request type converts through `TryFrom`, and a query's parameters, each read as text, through `Page` and `Search`, which read a list's paging and its `search` ([platform spec](../spec/platform.md#lists)), and each filter's domain type. Service functions, and the values an email is rendered from, take domain types, never raw strings for values that have a domain type (D25).
- Why: a value can't skip validation, whatever its source.
- Check: `test` (each route's invalid-input test asserts that every invalid field is reported), plus `review`.

### API-11 Scalar formats
Timestamps are RFC 3339 in UTC with a `Z` suffix, at microsecond precision.
A signed URL's `expires` is the exception, the Unix time in seconds, which its signature covers, while the file that gives the URL gives the same time as RFC 3339 (PLATFORM-080, FILES-021). <!-- part: files -->
IDs are UUID strings, which the database generates (DATA-08), with two exceptions: a trace ID is the 32 lowercase hexadecimal digits of W3C Trace Context (API-07, API-16), and a device ID is the string its client chooses for its installation, which the auth spec bounds (AUTH-017). Closed sets are `snake_case` strings. Two kinds of string that a schema lists aren't closed sets of this rule: a problem's codes, which ERR-02 shapes, and the type of a frame of the stream, which API-19 shapes.
- Why: one representation per kind of value.
- Check: `ci: check-openapi` (formats in the committed schema), plus `review` (the shared serializers in `platform::types` are used).

## Behavior

### API-12 Idempotency keys follow the IETF draft
`POST` routes marked idempotent in their module spec accept `Idempotency-Key`, as the IETF's draft-ietf-httpapi-idempotency-key-header-08 defines it, which isn't an RFC yet: the key is a string of structured fields, such as `Idempotency-Key: "8e03978e-40d5-43e8-bc93-6894a57f9324"`, of 1 to 255 characters.
- A key is unique per authenticated user, across their sessions.
- The fingerprint covers the method, the path, the query and a digest of the body.
  On an upload route, whose body is a file's bytes alone, it also covers its `Content-Disposition`, which names the file. <!-- part: files -->
- The same key with the same fingerprint returns the stored result of the first request. A different fingerprint gets 422. A key whose first request is still running gets 409.
- A committed change always has its stored result, so a retry never repeats it. A request that failed without committing can be retried.
- Stored results expire after 24 hours.
- Token-issuing auth routes never take keys.

[The platform spec](../spec/platform.md#idempotency) describes the mechanism.
- Why: clients can safely retry creates, and a committed change can never run twice (D12).
- Check: `test` (PLATFORM-036).

### API-13 Editable resources support optimistic concurrency
A resource a client can edit returns an `ETag` built from its `version`. An update carrying `If-Match` with a stale ETag fails with 412. An update without `If-Match` applies as sent.
- Why: concurrent edits can't silently overwrite each other (D24).
- Check: `test`.

### API-14 Every route declares its access level in its type
Every route except the health probes (OPS-05) and the API reference that development serves (PLATFORM-052) is registered through one of these builders:
- `public`: no authentication.
- `user`: requires a `Principal`, whose account has proved its email address while email is on (AUTH-025).
- `verifying`: as `user`, for a route that proving the caller's email address, or correcting it, needs, which a caller whose account must still prove it may call (AUTH-025).
- `workspace::<P>`: requires `Authorized<P>` for the workspace in the path, where `P` is a permission.
- `workspace_upload::<P>` and `user_upload`: as `workspace::<P>` and `user`, for a route whose body is a file's bytes, or an avatar's, which its handler reads through `Upload` in a group of its own: once `Authorize::check` has authorized a member, on a workspace's, and once `Principal` has authenticated the caller, on a user's (PLATFORM-073, PLATFORM-074). <!-- part: files -->
- `user_download`: as `user`, for a route that serves bytes that never change, such as an export's archive, or a range of them (PLATFORM-081).
- `workspace_signed_download::<P>`, `user_signed_download` and `public_download`: as `workspace::<P>`, `user` and `public`, for a route that serves a file's bytes, or an avatar's, or a range of them (PLATFORM-081). On the first two, a signed URL may also prove the `Principal` (PLATFORM-080), and the last serves under `/v1/public/`. <!-- part: files -->

A caller who isn't a member of the workspace gets 404. A member without permission `P` gets 403. Each workspace builder gives its routes the access level `workspace`, each user builder the access level `user`, and each public builder the access level `public`.
- Why: no route can forget authentication or authorization, and non-members learn nothing (D2, D3).
- Check: `clippy` (`disallowed_methods` bans the routing methods of axum and utoipa-axum outside `platform::http`), plus `test`: a generated test calls every `user` route without a credential and expects 401, and TEST-08 covers the `workspace` routes.

### API-15 Rate limits are visible
Rate-limited responses carry the `RateLimit` and `RateLimit-Policy` headers of the IETF's draft-ietf-httpapi-ratelimit-headers-11, which isn't an RFC yet. Each holds one item, for the bucket that decided the response, named by the bucket's kind: `RateLimit-Policy: "ip";q=600;w=60` gives its quota and window in seconds, and `RateLimit: "ip";r=598;t=41` the requests left and the seconds until the window ends. A 429 also carries `Retry-After`.
- Why: clients can back off correctly (D11).
- Check: `test`.

### API-16 Every response carries its trace ID
Every response except the health probes (OBS-15), including streams, carries its trace context in `server-timing: trace;desc=<traceparent value>`, the form the W3C Trace Context editors' draft defines. Error bodies also carry `trace_id` (API-07).
- Why: any response leads back to its trace and logs (D10).
- Check: `test`.

### API-17 The OpenAPI document is generated and committed
`openapi.json` is generated from code and committed, and a test fails if the document the binary writes differs from it. Every route under `/v1` documents its requests, responses, error codes and access level, and a response header the document lists is one the API always sends with that response. Each operation's `operationId` is its own, and each schema is one type's, so no operation documents another's request or response. The stream of server-sent events documents every frame it sends, in its event catalog (REALTIME-008). Every field and every parameter typed with a domain type, a header's among them, such as `Idempotency-Key`, carries that type's limits in the schema (D25), and every string of a JSON request body has a `maxLength`.
An upload's body, a binary string, states its limit in its description, with the setting that gives it, if one does. <!-- part: files -->
- Why: the contract is reviewed in pull requests, and clients are built against it (D8).
- Check: `clippy` (only the builders of API-14 register routes, each at a path its documentation names), `test` (every documented path serves, of the methods OpenAPI can document, exactly those it documents, the committed document is the one the code writes, and every request an integration test sends through the test support's typed calls, every response the in-process API gives it, and every frame of a stream it reads are checked against the document of the routes that answered it; a test of malformed input sends its body raw, unchecked), plus `ci: check-openapi` (each operation's access level and response descriptions, each `operationId` its own and each schema name one type's, every reference in the document resolves, every string of a request body has a `maxLength`, and every event a module declares is in the event catalog), plus `review` (fields and parameters of domain types carry their limits).

### API-18 `/v1` only grows
Within `/v1`, changes are additive: new routes, new optional request fields, new response fields. Removing or changing anything existing requires `/v2`.
- Why: a client built against `/v1` keeps working.
- Check: `ci: check-openapi` (a breaking-change diff against the last final release).

### API-19 Event names and payloads
Server-sent events (SSE) are named `<resource>.<past-tense verb>`, for example `member.removed`. A payload carries IDs and non-sensitive fields only, and is a type that its event declares, whose schema the event catalog documents (REALTIME-008). Clients fetch details over REST.
- Why: events never leak data that the REST permission checks would withhold (D7), and a client knows each payload's shape from the document (D8).
- Check: `compiler` (an event is published only with the payload type it declares), `ci: check-openapi` (each frame type's shape, `<resource>.<verb>` in `snake_case`), `review` (the verb's tense, and what a payload holds), plus `test` (TEST-08 covers the REST side, and REALTIME-008 the payloads' schemas).

### API-20 Lists of named resources take `search`
A list whose items have a name or an email takes `search`, which keeps the items that contain it in one of the fields the route's documentation names, ignoring case, as the platform spec defines it (PLATFORM-047). It works on every page of the list, with the list's order and cursors (API-06).
- Why: a client finds an item among many in one way on every list, and a fork's lists take the same parameter as the template's.
- Check: `ci: check-openapi` (a list whose items have a property named `name` or `email`, or ending in `_name` or `_email`, takes `search`), plus `test` (each list's search has a test that pages through matches), plus `review` (a list of named resources whose names the check doesn't recognize).
