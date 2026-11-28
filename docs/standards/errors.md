# Error rules (`ERR`)

How errors are defined, how they move through the code, and how they reach the client. The wire format is in [api.md](api.md) (API-07).

## Defining errors

### ERR-01 One error enum per module
Each module defines one error enum in `error.rs`, using `thiserror`, with variants named in domain terms (for example `SessionExpired` or `MemberNotFound`). A large module may group variants into nested enums inside that file. Beside it, the file may hold an enum of what stops a run of a subcommand of the module's, which no route answers, and which the run's internal error holds as its cause.
`files` holds one so, `RestoreFailure`, of what stops a run of `restore-files` (FILES-029). <!-- part: files -->
- Why: a module's failure modes can be read in one place (D17).
- Check: `ci: check-layout` (every module has its `error.rs`), plus `review`.

### ERR-02 Every error a route can answer has a stable code and a category
Every variant of an error that a route can answer implements `platform::errors::ErrorCode`, which gives it:
- A code in the form `<namespace>.<snake_case_name>`. The namespace is the module name, for example `workspaces.member_not_found`. For errors `platform` raises itself, before any module runs (authentication, body limits, rate limits, load shedding, internal errors) or in a mechanism a module calls (authorization, idempotency keys), for a feature the deployment turned off, which the module whose route serves it answers (OPS-04), for an update whose precondition fails, which every editable resource answers alike (API-13, D87), and for a sensitive action whose caller's session hasn't just proved who its user is, which every such action answers alike (AUTH-023, D148), the namespace is `request`, for example `request.rate_limited`. Once in a final release, a code never changes.
- One category.

An error that only a subcommand reports, such as `seed`'s, implements no `ErrorCode`: no client receives it, so it has no code to keep stable, and the subcommand writes its message (OPS-03).
- Why: clients act on codes, so codes are part of the contract.
- Check: `compiler` (the conversion to a response requires `ErrorCode`), plus `ci: check-openapi` (its breaking-change diff against the last final release, by an operation's codes and their statuses, as the comment of [`breaking`](../../crates/xtask/src/openapi/breaking/mod.rs) says what it takes for a changed code), plus `review` (an error that only a subcommand reports implements no `ErrorCode`).

### ERR-03 The category alone sets the HTTP status
`platform` defines the categories and their statuses. A module never sets an error's status directly.

| Category | Status | Use |
|---|---|---|
| `Validation` | 400 | Malformed input, or a field that fails validation |
| `Unauthenticated` | 401 | Missing, invalid or expired credentials |
| `Forbidden` | 403 | Not allowed: an authenticated caller's action within a resource the caller can see, or a change from an origin the API doesn't list (PLATFORM-059) |
| `NotFound` | 404 | The resource doesn't exist or the caller may not know it exists |
| `MethodNotAllowed` | 405 | The path exists but doesn't serve the method. The response carries `Allow` with the methods it does serve (RFC 9110, §15.5.6) |
| `Conflict` | 409 | Conflicts with current state: a duplicate, already a member, a request already in flight |
| `PreconditionFailed` | 412 | An `If-Match` mismatch (API-13) |
| `PayloadTooLarge` | 413 | A body over its limit |
| `BusinessRule` | 422 | Valid input that a rule refuses, such as removing the owner |
| `RateLimited` | 429 | A rate limit was hit |
| `Unavailable` | 503 | A disabled feature, a dependency that's down, load shedding, a request past its timeout, or, from the reference deployment's proxy, no replica that answers (OPS-05) |
| `Internal` | 500 | Anything else |

- Why: the same kind of failure always produces the same status (D8, D17).
- Check: `compiler` (the category is an enum, and the mapping lives in one function), plus `ci: check-openapi` (an operation whose access level is `public` lists no code for a 403 but `request.origin_not_allowed`).

## Moving errors

### ERR-04 Infrastructure errors stay in their adapter
The errors of the libraries that reach other systems, such as the database driver and the clients for email and HTTP, never appear in a service or route signature. The repository or adapter turns each one into either a domain outcome or a `platform::errors::Internal` value. That value keeps the cause for logs only.
- Why: a database error must never decide a response or reach a client (D17).
- Check: `ci: check-layers` (no signature in a module's service or routes names an infrastructure error type), plus `compiler`.

### ERR-05 Constraint violations are mapped explicitly
A repository maps each unique, foreign-key or check constraint it relies on to a named domain outcome. A violation it doesn't map becomes `Internal`. Each mapped constraint has a test. A statement whose `ON CONFLICT` says what a conflict does raises no violation to map. One kind of violation the pipeline answers for every route, in the internal error's place: a foreign key's refusal of a row in a request whose caller's session lasts no more, as once a deletion took their account and the row that the request's insert names, answers 401 (PLATFORM-097).
- Why: a duplicate is a 409 the client can act on, not a 500 (D24).
- Check: `test` (PLATFORM-097, whose tests also show a foreign key's refusal that nothing maps staying `Internal`; this rule's own tests, a check constraint's refusal that no repository maps staying `Internal`; and each mapped constraint has a test, such as ACCOUNTS-004's and AUDIT-004's), plus `review` (which constraints a repository relies on).

### ERR-06 Errors cross module boundaries by conversion
When a service calls another module, it converts that module's error into its own, either with a `From` implementation in its `error.rs` or at the call site. No module returns another module's error type.
- Why: each module's error enum stays a complete list of its own failure modes.
- Check: `ci: check-deps` (error types of other modules don't appear in public signatures).

### ERR-07 One conversion produces every error response
Errors become HTTP responses in exactly one function in `platform::errors`, which produces the problem format in API-07. A module's handler that can fail returns `Result<_, Problem<E>>` where `E: ErrorCode`, and `?` converts an `E` into its `Problem<E>`.
- Why: one path means one format, everywhere (D17).
- Check: `compiler`.

## Content and logging

### ERR-08 Internal errors expose nothing
A 500 response has a fixed title and no detail. Its only identifying field is `trace_id`.
- Why: internals in a response help attackers and confuse clients (D8).
- Check: `test` (`platform`'s unit tests assert the fixed body of an error of the `Internal` category, and the integration tests' support asserts it of each 500 that it reads from the API in process).

### ERR-09 `detail` is for humans and never carries internals
`detail` holds a human-readable sentence, which holds on every route that answers its code, for every caller that the route answers it to: it names no field or step that only some of those routes or callers have. It never includes SQL, file paths, internal identifiers, stack traces or secrets. Clients act on `code`, never on the text.
- Why: the text can change or be translated, while the code is the contract.
- Check: `review`.

### ERR-10 An error is logged once, where it is handled
The HTTP boundary logs every 5xx at `ERROR`, with its cause chain and trace ID, but a health probe's (OPS-05), a panic's, whose one `ERROR` line the panic hook writes (ERR-12), and the 503 of a feature that the deployment turned off, which is no failure (OPS-04). 4xx responses appear in the request's completion log (OBS-03) and are not logged at `WARN` or `ERROR`. Code that returns an error never logs it. An error that a download's bytes meet once its status is sent can become no response: it ends the body short, and is an ignored error of ERR-11 (PLATFORM-100).
- Why: one log entry per failure, at a level that means something (D17).
- Check: `review`.

### ERR-11 An ignored error is explicit
An error is deliberately ignored only through `platform::errors::ignore(error, reason)`, which logs it at `WARN` in a standard shape and counts it in the `platform.errors_ignored` metric, labelled by reason. `let _ =` on a fallible call is not allowed (CODE-03). An error that the code reads as a value, where its doc says what value, is no ignored error, such as a cookie that may not authenticate its request, which counts as none (AUTH-096), or the body of a probe's answer that isn't its JSON, which names no status. So is the `PoisonError` of a lock that a panic poisoned, whose value `platform::errors::Held` takes as the panic left it, where the lock's doc says why that value is whole.
- Why: every ignored failure is visible in logs and metrics (D17).
- Check: `clippy` (`let_underscore_must_use`, `unused_result_ok` for a `Result` dropped with `.ok()`, and the bans of `PoisonError`'s `into_inner`, `get_ref` and `get_mut`, of which `Held` calls `into_inner` under an `#[expect]`), `test` (the line and the count, and what `Held` takes), plus `review` (a `Result` dropped another way, such as one that holds nothing and ends in `unwrap_or_default`, which no lint tells from a value's default, and the doc of each lock that `Held` takes).

### ERR-12 Panics become a 500 and are counted
A catch-panic layer turns a panic in what it wraps, the layers that follow it in [architecture §6](../architecture.md#6-request-lifecycle)'s order and the route they pass the request to, into the ERR-08 response. A panic while the server reads a response's body comes once its status has gone out, and becomes no response: the connection ends, cutting the body short, and the request's completion line and span, where it has them, keep that status. In every subcommand that logs, each panic is logged at `ERROR` and counted in `platform.panics`, on whatever thread it happens. Panics are still not allowed in code (CODE-03); the layer is the last line of defense.
- Why: one bug must never take down the process or leak a stack trace.
- Check: `test`.

### ERR-13 Every error code is in the catalog
Every code appears in the generated OpenAPI document, with its category and meaning. A response lists each of its codes once: the document gives every operation the codes that the pipeline answers there, `request.rate_limited` among them, so a route's builder takes only the errors that the route answers besides those.
- Why: client developers can see every failure they need to handle.
- Check: `ci: check-openapi` (every code that the `code` method of an `ErrorCode` of `platform` or `app` returns, outside tests, is among the codes of the document's problem responses, an operation's or a shared one's, read in the forms that the comment of [`codes.rs`](../../crates/xtask/src/openapi/codes.rs) lists, which fails on any other, and none of those responses lists a code twice), plus `test` (every problem an integration test receives is one the document lists for its route, API-17).
