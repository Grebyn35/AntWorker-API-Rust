# API rules (`API`)

The HTTP contract every route follows. The generated OpenAPI document is the authoritative description of each route, and these rules govern its shape.

## Paths and methods

### API-01 Paths are versioned, plural and lowercase
Every route lives under `/v1`, except the health probes `/health/live` and `/health/ready` (OPS-05), which load balancers call without a version, and the API reference that development serves at `/docs` (PLATFORM-052), which no client is built against. Collections are plural nouns, and segments of more than one word use kebab-case (`/order-items`). A path parameter is named `{<resource>_id}`, after the resource whose ID it holds, which the segment before it may name in other words: `/members/{user_id}` holds the ID of the member's user.
- Why: a path reads as a predictable address for a resource (D8).
- Check: `ci: check-openapi` (each path of the committed document is under `/v1`, its segments are lowercase kebab-case, and its parameters are `snake_case` names that end in `_id`), plus `review` (a collection is a plural noun, and a parameter is named after the resource whose ID it holds).

### API-02 The scope of a resource is in its path
Workspace resources live under `/v1/workspaces/{workspace_id}/`, and the caller's own resources under `/v1/users/me/`.
A public file's bytes, an avatar's and a workspace's picture's, which anyone may download without signing in, live under `/v1/public/`, which holds nothing else, and whose requests count in a bucket of the IP limit of their own (PLATFORM-082). <!-- part: files -->
A superadmin's routes, which act on the deployment's workspaces, live under `/v1/superadmin/`, which holds nothing else, and one that acts on one workspace names it there, under `/v1/superadmin/workspaces/{workspace_id}/`. <!-- part: superadmins -->
No header selects a workspace.
- Why: every request names exactly what it acts on, and authorization checks that name (D3).
- Check: `ci: check-openapi`, plus `clippy` (API-14).

### API-03 Methods carry their HTTP meaning
- `GET` reads, and is safe and idempotent.
- `POST` creates a resource, or runs an action on a named sub-resource (`/transfer`, `/accept`). Adding a way to sign in to one's account, a password or a provider's identity (`/identities/password`, `/identities/provider`), is such an action, which answers 200 with the account's identities (AUTH-086, AUTH-087): no route reads one identity, so a create would have no resource of its own to answer, nor a `Location` for one (API-04).
- `PATCH` updates part of a resource using JSON Merge Patch (RFC 7396).
- `PUT` replaces a whole resource, and is used only where a resource is always written whole.
- `DELETE` removes.
- Why: clients, proxies and caches rely on these meanings (D8).
- Check: `ci: check-openapi`, plus `review`.

### API-04 Status codes follow the outcome
| Outcome | Status |
|---|---|
| A read, an update that returns the resource, or an action that answers with a body | 200 |
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
A success body is the resource object, with no envelope. A list is `{"items": [...], "next_cursor": "..." | null}`. A resource may hold its sub-resources as an array where a constraint of the database bounds their number, as a user's identities hold one provider identity at most for each provider (AUTH-085); a collection that nothing bounds is a list (API-06). Fields are `snake_case`. A field that every response holds, as its value or as `null`, is `required` in its schema, and a field that a response may leave out is documented as one that is never `null`. Clients ignore fields they don't recognize.
The telemetry proxy's routes answer what the OpenTelemetry Protocol asks of an endpoint, a 200 with its full success in the media type of the request, since an exporter built for the protocol reads it (CLIENT_TELEMETRY-004). <!-- part: client_telemetry -->
- Why: one shape for every resource and every list (D8).
- Check: `ci: check-openapi` (the names of properties, in each place that the comment of [its `schemas.rs`](../../crates/xtask/src/openapi/schemas.rs) lists, a list's shape, and that a property which may be `null` is `required` in each schema that no request gives, as the comment of [its `presence.rs`](../../crates/xtask/src/openapi/presence.rs) says which those are), plus `review` (an array of sub-resources that a resource holds, and a field that a response may leave out).

### API-06 Lists use opaque cursors
Every list is paginated by keyset. `limit` defaults to 50, and one outside 1 to 100 answers 400 `request.invalid_query`, naming `/limit` with the code `out_of_range` (PLATFORM-045, PLATFORM-046). `cursor` is an opaque base64url string that encodes the full sort key, with the row's ID as the tiebreaker. Lists move forward only, unless a module spec defines backward paging for a route.
- Why: pages stay stable while rows are added, and no row is skipped or repeated (D8, DATA-17).
- Check: `ci: check-openapi` (a list takes `limit` and `cursor`), plus `test` (each list has a test that pages across rows sharing a timestamp, such as PLATFORM-046's and AUTH-043's).

### API-07 Errors are RFC 9457 problems
An error response, except from the health probes (OPS-05) and the HTTP server's own refusal of a request that never reaches the router (PLATFORM-099), has `Content-Type: application/problem+json` and these members:
- `type`: `/problems/<code>`.
- `title`: a short summary.
- `status`: the HTTP status.
- `detail`: a human-readable explanation, when there is one (ERR-09).
- `code`: the stable error code (ERR-02).
- `trace_id`: the request's trace ID.
- `errors`: present only for validation failures. A list of `{field, code, message}`, where `field` is a JSON Pointer to the invalid input: into the body, or, for `request.invalid_query`, into the query's parameters, such as `/limit`.
- Why: one standard format that every client parses the same way (D8, D10). The exception is a request that only a broken client sends, and that only another HTTP server would answer otherwise (D215).
- Check: `compiler` (ERR-07), plus `clippy` (`disallowed_types` bans axum's `Path` and `RawPathParams`, whose rejections are plain text), plus `test`: a generated test sends a segment that percent-decodes to bytes that aren't UTF-8 in each path parameter of every documented route, and expects a problem.

### API-08 Request bodies are strict
JSON bodies reject unknown fields. Each route group has a body size limit, and an oversized body gets the 413 problem before it is buffered.
An upload's body isn't JSON: an upload route's group has no body limit of its own, since its handler refuses a caller before it reads the body, which it reads through `Upload` with the limit its module sets, such as a setting's, refusing an oversized body as a group's limit would (PLATFORM-074). <!-- part: files -->
A batch of telemetry is the body that the OpenTelemetry Protocol defines, as Protobuf or as JSON, and the API forwards it without parsing it, so it reads no field of a JSON batch that it could refuse: the group of a batch's route has no body limit of its own, and its handler reads the body through `Batch`, with the limit its module sets, refusing an oversized body as a group's limit would (PLATFORM-090). <!-- part: client_telemetry -->
- Why: a mistyped field fails loudly instead of being silently ignored (D8, D12).
- Check: `ci: check-layers` (every type in a module's routes that derives `Deserialize` carries `#[serde(deny_unknown_fields)]`), plus `test`.

### API-09 Query parameters are strict
Unknown query parameters are rejected with 400. Filters are explicit, named parameters. Booleans are `true` or `false`.
- Why: a misspelled filter must never return unfiltered data (D8).
- Check: `ci: check-layers` (the API-08 check covers query types), plus `test`.

### API-10 Input becomes domain types at the boundary
A module's routes convert each request's input into the domain types its service takes, and report in one response every value of one body, or of one query, that the conversion refuses (API-07): a body's request type converts through `TryFrom`, and a query's parameters, each read as text, through `Page` and `Search`, which read a list's paging and its `search` ([platform spec](../spec/platform.md#lists)), and each filter's domain type. A body whose values are checked against the API's state converts in its handler instead, through functions that take that state and record what they refuse in one `FieldErrors`, as a sign-in through a provider checks its redirect address against the clients that the providers' registry lists (AUTH-078). A route that refuses values of one request in more than one step, such as a header's, a query's and a body's, answers with the refusal of the first of those steps: the steps come in the order that [architecture.md §6](../architecture.md#6-request-lifecycle) gives, such as an `Idempotency-Key` that isn't one before the body, and a handler's own reads in the order in which it makes them. A subcommand's arguments, which no route reads, are converted before its operation runs: by the function of the composition root that runs the subcommand, through each domain type's own reader, or by the constructor of the type that the operation takes. Service functions, and the values an email is rendered from, take domain types, never raw strings for values that have a domain type (D25).

Each length, pattern and closed set that the document gives a field of a JSON request body bounds what the body's conversion takes: the conversion refuses a value outside it with 400 at the field. API-17 says what the document gives.
- Why: a value can't skip validation, whatever its source, and a client that sends what the document's bounds refuse learns which field is wrong.
- Check: `test` (each route's invalid-input test asserts that every invalid field is reported; a generated test sends each parameter of each list a value that none takes, and expects `request.invalid_query` to name it alone; and another sends each field of each JSON request body each value that the comment of [its `outside_its_bounds`](../../crates/app/tests/integration/openapi.rs) names, and expects `request.invalid_fields` to name the field), plus `review`.

### API-11 Scalar formats
Timestamps are RFC 3339 in UTC with a `Z` suffix, at microsecond precision. A moment that a request gives, such as a list's filter, may hold a second's fraction of 1 to 6 digits, or none, since a client's own clock may write fewer (PLATFORM-088).
A signed URL's `expires` is the exception, the Unix time in seconds, which its signature covers, while the file that gives the URL gives the same time as RFC 3339 (PLATFORM-080, FILES-021). <!-- part: files -->
IDs are UUID strings, which the database generates (DATA-08), in the form of 36 characters with hyphens, which is the one form that a request's path, query, body or header gives one in, its letters in either case, with three exceptions: a trace ID is the 32 lowercase hexadecimal digits of W3C Trace Context (API-07, API-16), a device ID is the string its client chooses for its installation, which the auth spec bounds (AUTH-017), and a client ID is the one that an OAuth provider issued for a deployment's client (AUTH-081). Closed sets are `snake_case` strings. Three kinds of string that a schema lists aren't closed sets of this rule: a problem's codes, which ERR-02 shapes, the type of a frame of the stream, which API-19 shapes, and the action of an audit entry, which the audit spec shapes (AUDIT-009, AUDIT-014). A permission's name, such as `members.read`, which a workspace's `permissions`, the `permission` of an `access.denied` entry of the audit log and an operation's `x-permission` give, is no closed set either, and no schema lists the names: [the access spec](../spec/access.md#roles-and-permissions) lists those of the template, and a module adds its own (D230).
- Why: one representation per kind of value.
- Check: `ci: check-openapi` (the formats of the properties and the parameters that the comment of [its `schemas.rs`](../../crates/xtask/src/openapi/schemas.rs) names: a timestamp by a property's name, an ID by a property's or a parameter's), `clippy` (the `uuid` crate's `parse_str` and `try_parse` are banned, so a request's UUID is read by `platform::types::read_id`), plus `review` (the shared serializers in `platform::types` are used, and a parameter that gives a moment is a `lists::Moment`, whose schema holds the format).

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
A resource a client can edit returns an `ETag` built from its version (PLATFORM-071). An update carrying `If-Match` with a stale ETag fails with 412. An update without `If-Match` applies as sent. A part of an editable resource that has no representation of its own, and that a `PUT` or a `DELETE` writes at a path of its own, is an update of the resource: such a write takes the resource's `If-Match`, and a `PUT` of it answers the resource, with the resource's new `ETag` and a `Content-Location` that names the resource, whose representation the body is (RFC 9110, §8.7).
A user's avatar and a workspace's picture are such parts (ACCOUNTS-008, WORKSPACES-019). <!-- part: files -->
- Why: concurrent edits can't silently overwrite each other (D24).
- Check: `test`.

### API-14 Every route declares its access level in its type
Every route except the health probes (OPS-05) and the API reference that development serves (PLATFORM-052) is registered through one of these builders:
- `public`: no authentication.
- `user`: requires a `Principal`, whose account has proved its email address while email is on (AUTH-025), and has a second factor while the deployment requires one (AUTH-070).
- `verifying`: as `user`, for a route that proving the caller's email address, or correcting it, needs, which a caller whose account must still prove it may call (AUTH-025).
- `enrolling`: as `user`, for a route that setting up a second factor needs, which a caller whose account has none may call while the deployment requires one (AUTH-070).
- `workspace::<P>`: requires `Authorized<P>` for the workspace in the path, where `P` is a permission.
- `superadmin`: requires `Superadmin`, the proof that the caller is one of the deployment's superadmins, with a second factor (SUPERADMINS-004, SUPERADMINS-005). <!-- part: superadmins -->
- `workspace_upload::<P>` and `user_upload`: as `workspace::<P>` and `user`, for a route whose body is a file's bytes, an avatar's or a workspace's picture's, which its handler reads through `Upload` in a group of its own: once `Authorize::check` has authorized a member, on a workspace's, and once `Principal` has authenticated the caller, on a user's (PLATFORM-073, PLATFORM-074). <!-- part: files -->
- `user_download`: as `user`, for a route that serves bytes that never change, such as an export's archive, or a range of them (PLATFORM-081).
- `user_batch`: as `user`, for a route whose body is a batch of telemetry, which its handler reads through `Batch`, in a group of its own (PLATFORM-090). <!-- part: client_telemetry -->
- `workspace_signed_download::<P>`, `user_signed_download` and `public_download`: as `workspace::<P>`, `user` and `public`, for a route that serves a file's bytes, an avatar's or a workspace's picture's, or a range of them (PLATFORM-081). On the first two, a signed URL may also prove the `Principal` (PLATFORM-080), and the last serves under `/v1/public/`. <!-- part: files -->

A caller who isn't a member of the workspace gets 404. A member without permission `P` gets 403. Each workspace builder gives its routes the access level `workspace`, each user builder the access level `user`, and each public builder the access level `public`.
A caller who is no superadmin gets 403 from a superadmin's route, whose access level is `superadmin`. <!-- part: superadmins -->
- Why: no route can forget authentication or authorization, and non-members learn nothing (D2, D3).
- Check: `clippy` (`disallowed_methods` bans the routing methods of axum and utoipa-axum outside `platform::http`), plus `test`: a generated test calls every route that isn't `public` without a credential and expects 401, and TEST-08 covers the `workspace` routes.

### API-15 Rate limits are visible
A response to a request that a rate limit counted, other than a 5xx, carries the `RateLimit` and `RateLimit-Policy` headers of the IETF's draft-ietf-httpapi-ratelimit-headers-11, which isn't an RFC yet. Each holds one item, for the bucket that decided the response, named by the bucket's kind: `RateLimit-Policy: "ip";q=600;w=60` gives its quota and window in seconds, and `RateLimit: "ip";r=598;t=41` the requests left and the seconds until the window ends. A 429 also carries `Retry-After`.
A budget, which counts bytes, says so with the draft's quota unit, as `RateLimit-Policy: "<name>";q=1048576;qu="content-bytes";w=60`, and its `RateLimit` gives the bytes left (PLATFORM-089).
- Why: clients can back off correctly (D11).
- Check: `test` (PLATFORM-056, the headers of a bucket and a 429's `Retry-After`, and PLATFORM-089, a budget's).

### API-16 Every response carries its trace ID
Every response except the health probes (OBS-15) and the HTTP server's own refusal of a request that never reaches the router (PLATFORM-099), including streams, carries its trace context in `server-timing: trace;desc=<traceparent value>`, the form the W3C Trace Context editors' draft defines. Error bodies also carry `trace_id` (API-07).
- Why: any response leads back to its logs, and to its trace where the collector kept it (D10, OBS-08).
- Check: `test` (PLATFORM-041).

### API-17 The OpenAPI document is generated and committed
`openapi.json` is generated from code and committed, and a test fails if the document the binary writes differs from it. Every route under `/v1` documents its requests, responses, error codes and access level, and a response header the document lists is one the API always sends with that response. Each operation's `operationId` is its own, and each schema is one type's, so no operation documents another's request or response. The stream of server-sent events documents every frame it sends, in its event catalog (REALTIME-008), and its action catalog documents every action that an audit entry holds (AUDIT-012, AUDIT-014). Every field and every parameter typed with a domain type, a header's among them, such as `Idempotency-Key`, carries that type's limits in the schema (D25), and every string of a JSON request body has a `maxLength`, or an `enum` whose values bound it. A header's schema takes no `null`, which no header can be.
An upload's body, a binary string, states its limit in its description, with the setting that gives it, if one does. <!-- part: files -->
A batch of telemetry is documented as the OpenTelemetry Protocol defines it and no further: as a binary string, or, as JSON, an object whose members the document doesn't list, with its limit in its description. <!-- part: client_telemetry -->
- Why: the contract is reviewed in pull requests, and clients are built against it (D8).
- Check: `clippy`, `test`, plus `ci: check-openapi`, plus `review`, each for what its item below says.
  - `clippy`: only the builders of API-14 register routes, each at a path its documentation names.
  - `test`: every documented path serves, of the methods OpenAPI can document, exactly those it documents, the committed document is the one the code writes, and every request an integration test sends through the test support's typed calls, every response the in-process API gives it, and every frame of a stream it reads are checked against the document of the routes that answered it; a test of malformed input sends its body raw, unchecked.
  - `ci: check-openapi`: each operation's access level and response descriptions, each `operationId` its own, every reference in the document resolves, every string of a request body has a `maxLength` or an `enum` and every header's schema takes no `null`, as the comments of [its `lengths.rs`](../../crates/xtask/src/openapi/lengths.rs) and [its `headers.rs`](../../crates/xtask/src/openapi/headers.rs) say what it reads, and the event catalog and the action catalog, as the realtime spec and the audit spec say the check reads them. It refuses the forms of the document that it doesn't read, which the comment of [its `forms.rs`](../../crates/xtask/src/openapi/forms.rs) lists. Of the schema names that it reads in the code, which the comment of [its `names.rs`](../../crates/xtask/src/openapi/names.rs) lists with the `ToSchema` that it refuses since it can't read their names, no two are the same, none is `Frame` or `AuditEntry`, the two catalogs, a component that one of them refers to, or one that the document marks as a frame type's or an action's. Each of those names whose `ToSchema` a file of `app` writes is a component's, or, for a generic type, an instance's, while each component is one of those names, a catalog's, or an instance of a generic type, as that comment says.
  - `review`: fields and parameters of domain types carry their limits, and that a module declares its events and its audit actions in the form the check reads, which the two specs give.

### API-18 `/v1` only grows
Within `/v1`, changes are additive: new routes, new optional request fields, new response fields, a new alternative of a response that lists its alternatives, as a new event of the stream and a new action of an audit log are, a new permission's name, and a request that is taken where it was refused, such as one without a field that was required and is optional now (D151). A client passes over an event or an action that it doesn't know (D214), and a permission's name that it doesn't know (D230). Removing or changing anything else that exists requires `/v2`, a new value of a response's closed set among them, such as a role.
- Why: a client built against `/v1` keeps working, and a fork's new module still brings its events and its actions (D211, D214).
- Check: `ci: check-openapi` (a breaking-change diff against the last final release, which reads of the two documents what the comment of [`breaking`](../../crates/xtask/src/openapi/breaking/mod.rs) says), plus `review` (what the documents don't hold, such as what a route does with a request that it takes).

### API-19 Event names and payloads
Server-sent events (SSE) are named `<resource>.<past-tense verb>`, for example `member.removed`, and so are the frames that the stream sends of its own, such as `stream.closed` (REALTIME-008). The verb may name what changed before it, as `member.role_changed` does. A payload carries IDs and non-sensitive fields only, and is a type that its event declares, whose schema the event catalog documents (REALTIME-008). Clients fetch details over REST.
- Why: events never leak data that the REST permission checks would withhold (D7), and a client knows each payload's shape from the document (D8).
- Check: `compiler` (an event is published only with the payload type it declares), `ci: check-openapi` (each frame type's shape, `<resource>.<verb>` in `snake_case`), `review` (the verb's tense, and what a payload holds), plus `test` (TEST-08 covers the REST side, and REALTIME-008 the payloads' schemas).

### API-20 Lists of named resources take `search`
A list whose items have a name or an email takes `search`, which keeps the items that contain it in one of the fields the route's documentation names, ignoring case, as the platform spec defines it (PLATFORM-047). It works on every page of the list, with the list's order and cursors (API-06).
- Why: a client finds an item among many in one way on every list, and a fork's lists take the same parameter as the template's.
- Check: `ci: check-openapi` (a list whose items have a property named `name` or `email`, or ending in `_name` or `_email`, takes `search`), plus `test` (each list's search has a test that pages through matches), plus `review` (a list of named resources whose names the check doesn't recognize).
