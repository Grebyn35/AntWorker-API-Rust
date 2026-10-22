# API rules (`API`)

The HTTP contract every route follows. The generated OpenAPI document is the authoritative description of each route, and these rules govern its shape.

## Paths and methods

### API-01 Paths are versioned, plural and lowercase
Every route lives under `/v1`, except the health probes `/health/live` and `/health/ready` (OPS-05), which load balancers call without a version. Collections are plural nouns, and segments of more than one word use kebab-case (`/data-exports`). Path parameters are named `{<resource>_id}`.
- Why: a path reads as a predictable address for a resource (D8).
- Check: `ci: openapi` (a path lint over the generated document).

### API-02 The scope of a resource is in its path
Workspace resources live under `/v1/workspaces/{workspace_id}/`, and the caller's own resources under `/v1/users/me/`. No header selects a workspace.
- Why: every request names exactly what it acts on, and authorization checks that name (D3).
- Check: `ci: openapi`, plus `compiler` (API-14).

### API-03 Methods carry their HTTP meaning
- `GET` reads, and is safe and idempotent.
- `POST` creates a resource, or runs an action on a named sub-resource (`/transfer`, `/accept`).
- `PATCH` updates part of a resource using JSON Merge Patch (RFC 7396).
- `PUT` replaces a whole resource, and is used only where a resource is always written whole.
- `DELETE` removes.
- Why: clients, proxies and caches rely on these meanings (D8).
- Check: `ci: openapi`, plus `review`.

### API-04 Status codes follow the outcome
| Outcome | Status |
|---|---|
| A read, or an update that returns the resource | 200 |
| A create, with a `Location` header and the created resource | 201 |
| Asynchronous work accepted, such as a data export | 202 |
| A delete, or an action with nothing to return | 204 |
| An error | from its category (ERR-03) |
- Why: the status alone tells the client what happened (D8).
- Check: `test`, plus `ci: openapi`.

## Bodies

### API-05 Responses are the resource itself
A success body is the resource object, with no envelope. A list is `{"items": [...], "next_cursor": "..." | null}`. Fields are `snake_case`. Clients ignore fields they don't recognize.
- Why: one shape for every resource and every list (D8).
- Check: `ci: openapi`.

### API-06 Lists use opaque cursors
Every list is paginated by keyset. `limit` defaults to 50 and is capped at 100. `cursor` is an opaque base64url string that encodes the full sort key, with the row's ID as the tiebreaker. Lists move forward only, unless a module spec defines backward paging for a route.
- Why: pages stay stable while rows are added, and no row is skipped or repeated (D8, DATA-23).
- Check: `test` (each list has a test that pages across rows sharing a timestamp).

### API-07 Errors are RFC 9457 problems
An error response, except from the health probes (OPS-05), has `Content-Type: application/problem+json` and these members:
- `type`: `/problems/<code>`.
- `title`: a short summary.
- `status`: the HTTP status.
- `detail`: a human-readable explanation, when there is one (ERR-09).
- `code`: the stable error code (ERR-02).
- `trace_id`: the request's trace ID.
- `errors`: present only for validation failures. A list of `{field, code, message}`, where `field` is a JSON Pointer to the invalid input.
- Why: one standard format that every client parses the same way (D8, D10).
- Check: `compiler` (ERR-07), plus `test`.

### API-08 Request bodies are strict
JSON bodies reject unknown fields. Each route group has a body size limit, and an oversized body gets the 413 problem before it is buffered. Uploads are the only non-JSON bodies.
- Why: a mistyped field fails loudly instead of being silently ignored (D8, D12).
- Check: `ci: check-layers` (every type in a module's routes that derives `Deserialize` carries `#[serde(deny_unknown_fields)]`), plus `test`.

### API-09 Query parameters are strict
Unknown query parameters are rejected with 400. Filters are explicit, named parameters. Booleans are `true` or `false`.
- Why: a misspelled filter must never return unfiltered data (D8).
- Check: `ci: check-layers` (the API-08 check covers query types), plus `test`.

### API-10 Input becomes domain types at the boundary
Each request type converts into the service's domain input through `TryFrom`, and every field error is collected into one response (API-07). Service functions take domain types, never raw strings for values that have a domain type (D25).
- Why: a value can't skip validation, whatever its source.
- Check: `test` (each route's invalid-input test asserts that every invalid field is reported), plus `review`.

### API-11 Scalar formats
Timestamps are RFC 3339 in UTC with a `Z` suffix, at microsecond precision. IDs are UUID strings. Closed sets are `snake_case` strings.
- Why: one representation per kind of value.
- Check: `ci: openapi` (formats in the generated schema), plus `review` (the shared serializers in `platform::types` are used).

## Behavior

### API-12 Idempotency keys follow the IETF draft
`POST` routes marked idempotent in their module spec accept `Idempotency-Key`:
- A key is unique per authenticated principal.
- The fingerprint covers the method, the route, the query and a digest of the body.
- The same key with the same fingerprint returns the stored result of the first request. A different fingerprint gets 422. A key whose first request is still running gets 409.
- A committed change always has its stored result, so a retry never repeats it. A request that failed without committing can be retried.
- Stored results expire after 24 hours.
- Token-issuing auth routes never take keys.

[The platform spec](../spec/platform.md#idempotency) describes the mechanism.
- Why: clients can safely retry creates, and a committed change can never run twice (D12).
- Check: `test` (PLATFORM-018).

### API-13 Editable resources support optimistic concurrency
A resource a client can edit returns an `ETag` built from its `version`. An update carrying `If-Match` with a stale ETag fails with 412. An update without `If-Match` applies as sent.
- Why: concurrent edits can't silently overwrite each other (D24).
- Check: `test`.

### API-14 Every route declares its access level in its type
Every route is registered through one of three builders:
- `public`: no authentication.
- `user`: requires a `Principal`.
- `workspace::<P>`: requires `Authorized<P>` for the workspace in the path, where `P` is a permission.

A caller who isn't a member of the workspace gets 404. A member without permission `P` gets 403.
- Why: no route can forget authentication or authorization, and non-members learn nothing (D2, D3).
- Check: `compiler` (no other way to register a route), plus `test` (TEST-09).

### API-15 Rate limits are visible
Rate-limited responses carry the `RateLimit` and `RateLimit-Policy` headers from the IETF draft, describing the bucket that decided the response. A 429 also carries `Retry-After`.
- Why: clients can back off correctly (D11).
- Check: `test`.

### API-16 Every response carries its trace ID
Every response except the health probes (OBS-15), including streams and downloads, carries the W3C `traceresponse` header. Error bodies also carry `trace_id` (API-07).
- Why: any response leads back to its trace and logs (D10).
- Check: `test`.

### API-17 The OpenAPI document is generated and committed
`openapi.json` is generated from code and committed. CI fails if the generated document differs from the committed one. Every route documents its requests, responses, error codes and access level. Every field typed with a domain type carries that type's limits in the schema (D25).
- Why: the contract is reviewed in pull requests, and clients are built against it (D8).
- Check: `ci: openapi`.

### API-18 `/v1` only grows
Within `/v1`, changes are additive: new routes, new optional request fields, new response fields. Removing or changing anything existing requires `/v2`.
- Why: a client built against `/v1` keeps working.
- Check: `ci: openapi` (a breaking-change diff against the last release).

### API-19 Event names and payloads
SSE events are named `<resource>.<past-tense verb>`, for example `member.removed`. A payload carries IDs and non-sensitive fields only. Clients fetch details over REST.
- Why: events never leak data that the REST permission checks would withhold (D7).
- Check: `review`, plus `test` (TEST-09 covers the REST side).
