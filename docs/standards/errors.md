# Error rules (`ERR`)

How errors are defined, how they move through the code, and how they reach the client. The wire format is in [api.md](api.md) (API-07).

## Defining errors

### ERR-01 One error enum per module
Each module defines one error enum in `error.rs`, using `thiserror`, with variants named in domain terms (for example `LastOwner` or `InvitationExpired`). A large module may group variants into nested enums inside that file.
- Why: a module's failure modes can be read in one place (D17).
- Check: `ci: check-layout` (`error.rs` exists in every module that returns errors), plus `review`.

### ERR-02 Every variant has a stable code and a category
Every variant implements `platform::errors::ErrorCode`, which gives it:
- A code in the form `<namespace>.<snake_case_name>`. The namespace is the module name, for example `workspaces.last_owner`. For errors `platform` raises before any module runs (authentication, body limits, rate limits, load shedding, disabled features, internal errors), the namespace is `request`, for example `request.rate_limited`. Once released, a code never changes.
- One category.
- Why: clients act on codes, so codes are part of the contract.
- Check: `compiler` (the conversion to a response requires `ErrorCode`), plus `ci: check-openapi` (a changed code is a breaking diff).

### ERR-03 The category alone sets the HTTP status
`platform` defines the categories and their statuses. A module never sets a status directly.

| Category | Status | Use |
|---|---|---|
| `Validation` | 400 | Malformed input, or a field that fails validation |
| `Unauthenticated` | 401 | Missing, invalid or expired credentials |
| `Forbidden` | 403 | Authenticated, but not allowed, within a resource the caller can see |
| `NotFound` | 404 | The resource doesn't exist or the caller may not know it exists |
| `MethodNotAllowed` | 405 | The path exists but doesn't serve the method. The response carries `Allow` with the methods it does serve (RFC 9110, §15.5.6) |
| `Conflict` | 409 | Conflicts with current state: a duplicate, already a member, a request already in flight |
| `PreconditionFailed` | 412 | An `If-Match` mismatch (API-13) |
| `PayloadTooLarge` | 413 | A body over its limit |
| `BusinessRule` | 422 | Valid input that a rule refuses, such as removing the owner |
| `RateLimited` | 429 | A rate limit was hit |
| `Unavailable` | 503 | A disabled feature, a dependency that's down, load shedding, or a request past its timeout |
| `Internal` | 500 | Anything else |

- Why: the same kind of failure always produces the same status (D8, D17).
- Check: `compiler` (the category is an enum, and the mapping lives in one function).

## Moving errors

### ERR-04 Infrastructure errors stay in their adapter
The errors of the libraries that reach other systems, such as the database driver and the clients for object storage, email and HTTP, never appear in a service or route signature. The repository or adapter turns each one into either a domain outcome or a `platform::errors::Internal` value. That value keeps the cause for logs only.
- Why: a database error must never decide a response or reach a client (D17).
- Check: `ci: check-layers` (no infrastructure error types in `service.rs` or `routes.rs`), plus `compiler`.

### ERR-05 Constraint violations are mapped explicitly
A repository maps each unique, foreign-key or check constraint it relies on to a named domain outcome. A violation it doesn't map becomes `Internal`. Each mapped constraint has a test.
- Why: a duplicate is a 409 the client can act on, not a 500 (D24).
- Check: `test`, plus `review`.

### ERR-06 Errors cross module boundaries by conversion
When a service calls another module, it converts that module's error into its own, either with a `From` implementation in its `error.rs` or at the call site. No module returns another module's error type.
- Why: each module's error enum stays a complete list of its own failure modes.
- Check: `ci: check-deps` (error types of other modules don't appear in public signatures).

### ERR-07 One conversion produces every error response
Errors become HTTP responses in exactly one function in `platform::errors`, which produces the problem format in API-07. Handlers return `Result<_, Problem<E>>` where `E: ErrorCode`, and `?` converts an `E` into its `Problem<E>`.
- Why: one path means one format, everywhere (D17).
- Check: `compiler`.

## Content and logging

### ERR-08 Internal errors expose nothing
A 500 response has a fixed title and no detail. Its only identifying field is `trace_id`.
- Why: internals in a response help attackers and confuse clients (D8).
- Check: `test` (every `Internal` path asserts the fixed body).

### ERR-09 `detail` is for humans and never carries internals
`detail` holds a human-readable sentence. It never includes SQL, file paths, internal identifiers, stack traces or secrets. Clients act on `code`, never on the text.
- Why: the text can change or be translated, while the code is the contract.
- Check: `review`.

### ERR-10 An error is logged once, where it is handled
The HTTP boundary logs every 5xx at `ERROR`, with its cause chain and trace ID. 4xx responses appear in the request's completion log (OBS-03) and are not logged at `WARN` or `ERROR`. Code that returns an error never logs it.
- Why: one log entry per failure, at a level that means something (D17).
- Check: `review`.

### ERR-11 An ignored error is explicit
An error is deliberately ignored only through `platform::errors::ignore(error, reason)`, which logs it at `WARN` in a standard shape and counts it in the `platform.errors_ignored` metric, labelled by reason. `let _ =` on a fallible call is not allowed (CODE-03).
- Why: every ignored failure is visible in logs and metrics (D17).
- Check: `clippy` (`let_underscore_must_use`, and `unused_result_ok` for a `Result` dropped with `.ok()`), `test` (the line and the count), plus `review`.

### ERR-12 Panics become a 500 and are counted
A catch-panic layer turns any panic in a request into the ERR-08 response. In every subcommand that logs, each panic is logged at `ERROR` and counted in `platform.panics`, on whatever thread it happens. Panics are still not allowed in code (CODE-03); the layer is the last line of defense.
- Why: one bug must never take down the process or leak a stack trace.
- Check: `test`.

### ERR-13 Every error code is in the catalog
Every code appears in the generated OpenAPI document, with its category and meaning.
- Why: client developers can see every failure they need to handle.
- Check: `ci: check-openapi`.
