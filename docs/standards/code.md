# Code rules (`CODE`)

## Toolchain and formatting

### CODE-01 One toolchain, edition 2024
Every crate uses edition 2024. The toolchain version is pinned in `rust-toolchain.toml`, and CI, the image build and local builds all use it.
- Why: every machine builds the same code the same way (D21).
- Check: `compiler`, plus `ci: check-toolchain`.

### CODE-02 Formatted by `rustfmt`
Code uses `rustfmt` with the repository's `rustfmt.toml`.
- Why: formatting is never a review topic.
- Check: `ci: fmt-check`.

## Lints

### CODE-03 Clippy warnings are errors
CI runs clippy with `-D warnings` and the lint set in `[workspace.lints]`. That set denies:
- `unwrap_used`, `expect_used`, `panic`, `todo`, `unimplemented`, `dbg_macro`
- `print_stdout`, `print_stderr`
- `cast_possible_truncation`, `cast_possible_wrap`, `cast_sign_loss`
- `let_underscore_must_use`, `let_underscore_future`, `unused_result_ok`
- `allow_attributes`, `allow_attributes_without_reason`, so every exception to a lint is an `#[expect]` that states its reason, and fails once nothing triggers the lint

`clippy.toml` allows `unwrap` and `expect` in tests only. Printing is allowed in three places: the API binary's startup line before logging exists (OPS-03) and the output of the checker in `xtask`, each through an `#[expect]` that states its reason, and the instructions a build script prints to Cargo, which clippy's print lints don't cover.
- Why: no panics in production code, no lossy numeric casts, no silently dropped results (D17).
- Check: `clippy`, `ci: check-instrument` (an `#[expect]` of a print lint only in the binary's `main.rs` and in `xtask`), plus `review` (what `main.rs` prints).

### CODE-04 No `unsafe`
`unsafe_code` is set to `forbid` for the whole workspace.
- Why: nothing in a web API needs it, and forbidding it removes a whole class of bugs.
- Check: `compiler`.

### CODE-05 No blocking calls in async code
Code the API runs never blocks its async runtime:
- Blocking file I/O is banned through `disallowed-methods` in `clippy.toml`.
- CPU-heavy work, such as password hashing, runs on a blocking thread, through the `platform` helper that moves it there and adds its span (OBS-06).
- A dependency that blocks inside an async call is allowed only where the spec of the area that uses it names the call and when it runs.

Tests and the test support may block, because each test runs on an async runtime of its own.
- Why: one blocked worker thread stalls every request scheduled on it.
- Check: `clippy` (`disallowed_methods`), plus `review` for CPU-heavy work and for dependencies.

### CODE-06 Time and randomness come from `platform`
Code in this workspace outside `platform::clock` doesn't read a clock (`Utc::now`, `SystemTime::now`, `Instant::now`, or the `elapsed` of a time it holds) or wait on one: `std::thread::sleep`, `tokio::time::sleep`, and the timed waits of threads, condition variables and channels. Every wait written in this workspace goes through the clock's `sleep`. Timeouts inside dependencies, such as the request timeout layer or an HTTP client timeout, are configured durations, not waits this rule covers. Code outside `platform` doesn't create its own random number generators.
- Why: tests control time, and every secret comes from the operating system's secure random source (D23, SEC-03).
- Check: `clippy` (`disallowed_methods` bans the clock reads and waits of the dependencies; a dependency that brings a clock or a random number generator brings its bans with it).

## Size

### CODE-07 Files stay under 500 lines
A source file over 500 lines, tests excluded, is split into a directory with the same name (ARCH-03).
- Why: a file a reader can hold in mind at once.
- Check: `ci: check-layout`.

### CODE-08 Functions stay short
Clippy's `too_many_lines` threshold is 80 and its `too_many_arguments` threshold is 6. A function over either threshold is split, or its arguments are grouped into a type.
- Why: a long function hides more than one responsibility.
- Check: `clippy`.

## Naming

### CODE-09 Rust naming conventions
Names follow the Rust API Guidelines: `UpperCamelCase` for types and traits, `snake_case` for functions, modules and fields, and `SCREAMING_SNAKE_CASE` for constants. Conversions use `as_`, `to_` and `into_` as those guidelines define.
- Why: the conventions every Rust developer already reads fluently.
- Check: `compiler` (built-in naming lints), plus `review`.

### CODE-10 One name per concept
A concept has the same name in code, SQL, the API and the docs: a workspace is a workspace everywhere, a member is a member. The terms are defined in the glossary in [docs/architecture.md](../architecture.md).
- Why: two names for one thing make readers think there are two things.
- Check: `review`.

## Dependencies

### CODE-11 Dependencies are declared once and kept minimal
Versions live in `[workspace.dependencies]`, and each crate inherits them. Default features are turned off unless needed. Each crate turns on the features it uses of each dependency, instead of relying on another dependency to turn them on. Git and path dependencies outside the workspace are not allowed.
- Why: one version of each crate, and a smaller build and attack surface.
- Check: `ci: deny` (`cargo-deny` bans), plus `review`.

### CODE-12 Dependency licenses are permissive
`cargo-deny` allows only permissive licenses: MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, ISC, Zlib, Unicode-3.0. It checks every crate that a build for one of the targets `just deny` names compiles: the platforms a developer builds on, and the image's. Any other license needs a rule change.
- Why: the template's own license is undecided, and a copyleft dependency would close off options (owner question Q1).
- Check: `ci: deny`.

### CODE-13 A new dependency states its purpose
A pull request that adds a dependency says what it is for and why the standard library or an existing dependency doesn't cover it.
- Why: every dependency is code you maintain but didn't write.
- Check: `review`.

## Shape

### CODE-14 No dead code, no commented-out code
Unused items are deleted, not kept behind `#[allow(dead_code)]`. Code that has been commented out is deleted.
- Why: dead code costs reading time and implies a requirement that doesn't exist.
- Check: `compiler` (`dead_code` denied; `app` exposes only its composition entry point publicly, so every module item is crate-visible and covered), `ci: check-layout` (no `allow(dead_code)`), plus `review` for unused public items in `platform` and for commented-out code.

### CODE-15 Public items are documented
Every public item in `platform`, and every item a module re-exports from `mod.rs`, has a doc comment (DOC-05).
- Why: a module's API is read far more often than it is written.
- Check: `clippy` (`missing_docs` on `platform`), plus `review` for module APIs.
