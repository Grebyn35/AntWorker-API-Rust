# Code rules (`CODE`)

## Toolchain and formatting

### CODE-01 One toolchain, edition 2024
Every crate uses edition 2024. The toolchain version is pinned in `rust-toolchain.toml`, and CI, the image build and local builds all use it.
- Why: every machine builds the same code the same way (D21).
- Check: `compiler`, plus `ci: check-pins`.

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

`clippy.toml` allows `unwrap` and `expect` in tests only. Printing is allowed in four places: the API binary's `main.rs`, for its startup line before logging exists (OPS-03) and for the accounts that `seed` reports once it finishes (SYSTEM-003); the checks and the other tasks in `xtask`, for their output; each of these through an `#[expect]` that states its reason; a build script, for the instructions it prints to Cargo, which clippy's print lints don't cover; and the watch of the tests' telemetry in `platform::telemetry::testing`, which writes the lines of a test that runs on to stderr through `std::io`, which the print lints don't see either (TEST-06).
- Why: no panics in production code, no lossy numeric casts, no silently dropped results (D17).
- Check: `clippy`, `ci: check-instrument` (an `#[expect]` of a print lint only in the binary's `main.rs` and in `xtask`), plus `review` (what `main.rs` prints).

### CODE-04 No `unsafe`
`unsafe_code` is set to `forbid` for the whole workspace.
- Why: nothing in a web API needs it, and forbidding it removes a whole class of bugs.
- Check: `compiler`.

### CODE-05 No blocking calls in async code
Code the API runs never blocks its async runtime:
- Blocking file I/O is banned through `disallowed-methods` in `clippy.toml`.
- CPU-heavy work, such as password hashing, runs on a blocking thread, through the `platform` helper that moves it there, adds its span (OBS-06) and gives it its turn (OPS-07). No other code moves work to a blocking thread itself; file I/O goes through `tokio::fs`, which runs each call on tokio's own blocking threads.
- A dependency that blocks inside an async call is allowed only where the spec of the area that uses it names the call and when it runs.

Tests and the test support may block, because each test runs on an async runtime of its own.
- Why: one blocked worker thread stalls every request scheduled on it.
- Check: `clippy` (`disallowed_methods`), plus `review` for CPU-heavy work and for dependencies.

### CODE-06 Time and randomness come from `platform`
Code in this workspace outside `platform::clock` doesn't read a clock (`Utc::now`, `SystemTime::now`, `Instant::now`, or the `elapsed` of a time it holds) or wait on one: `std::thread::sleep`, `tokio::time::sleep`, and the timed waits of threads, condition variables and channels, but for one read of `SystemTime::now`, by `just new-module`, which names the migration it writes after the moment it writes it (ARCH-03). Every wait written in this workspace goes through the clock's `sleep`, but for the watch of the tests' telemetry, which waits on the real time, on a thread of its own, for a test that may hold its runtime's thread forever (TEST-06). Only the binary's `main.rs` hands the system clock, through `platform::clock`, to what it builds: every other piece of code, the tests included, waits on the clock it's given. Timeouts inside dependencies, such as the request timeout layer or an HTTP client timeout, are configured durations, not waits this rule covers. Code outside `platform` doesn't create its own random number generators.
- Why: tests control time, and every secret comes from the operating system's secure random source (D23, SEC-03).
- Check: `clippy` (`disallowed_methods` bans the clock reads and waits of the dependencies, and `disallowed_types` the system clock outside `main.rs`; a dependency that brings a clock or a random number generator brings its bans with it).

### CODE-07 Shell passes shellcheck
Every recipe of the justfile and the files it imports that runs on Linux and macOS, and every shell script under `deploy/`, passes shellcheck with no finding, a script together with the files it sources. A finding is disabled only by a directive on the line before the command it concerns, with a comment that states the reason. A script under `deploy/` writes a value with `printf '%s\n'`, never with `echo`, and no function of it, or of a file it sources, assigns a variable that its top level sets.
- Why: in shell, a quoting slip runs a different command than the one written, and a script that deploys or restores runs where a mistake costs the most (D21). A server's `sh`, such as Ubuntu's dash, reads a backslash in `echo`'s arguments as an escape, and `\c` ends its output, while the tests' server, on Alpine, writes it as it is. Every variable of `sh` is global, so a function that assigns one the top level sets changes what the rest of the script reads.
- Check: `ci: check-shell`.

## Size

### CODE-08 Files stay under 500 lines
A file of code holds at most 500 lines. A Rust file of a crate's `src/` counts its lines before its test module, and one over the limit is split into a directory of modules with the same name (ARCH-03); a test file, under a crate's `tests/`, isn't counted. A shell script under `deploy/` counts all its lines, and one over the limit is split into files that it sources, by task, as `deploy.sh` sources `deploy/tasks/`. The justfile and each file it imports count all their lines: the justfile holds the settings, the variables and the tasks that run the others, and imports the rest from `just/`, where `just/<group>.just` holds the tasks of one group that `just --list` shows, with the private tasks they run.
- Why: a file a reader can hold in mind at once, whatever its language.
- Check: `ci: check-layout`.

### CODE-09 Functions stay short
Clippy's `too_many_lines` threshold is 80 and its `too_many_arguments` threshold is 6. A function over either threshold is split, or its arguments are grouped into a type.
- Why: a long function hides more than one responsibility.
- Check: `clippy`.

## Naming

### CODE-10 Rust naming conventions
Names follow the Rust API Guidelines: `UpperCamelCase` for types and traits, `snake_case` for functions, modules and fields, and `SCREAMING_SNAKE_CASE` for constants. Conversions use `as_`, `to_` and `into_` as those guidelines define.
- Why: the conventions every Rust developer already reads fluently.
- Check: `compiler` (built-in naming lints), plus `review`.

### CODE-11 One name per concept
A concept has the same name in code, SQL, the API and the docs: a workspace is a workspace everywhere, a member is a member. The terms are defined in the glossary in [docs/architecture.md](../architecture.md). The standard columns of DATA-02 keep their names in every table, since they describe the row; the API names a resource's field for what it means to the resource, such as a membership's `created_at`, which is when the member joined, as `joined_at`, and the module's model maps one to the other.
- Why: two names for one thing make readers think there are two things. A standard column named for each table's meaning would break the shared trigger and the schema checks that read the names.
- Check: `review`.

## Dependencies

### CODE-12 Dependencies are declared once and kept minimal
Versions live in `[workspace.dependencies]`, and each crate inherits them. Default features are turned off unless needed. Each crate turns on the features it uses of each dependency, instead of relying on another dependency to turn them on. A crate declares only the dependencies its code names: a normal one in its `src/` or build script, a development one there or in its tests, and a build one in its build script. Git and path dependencies outside the workspace are not allowed.
- Why: one version of each crate, and a smaller build and attack surface. A dependency that only a removed optional part used goes with it (ARCH-07).
- Check: `ci: deny` (`cargo-deny` bans), `ci: check-deps` (each dependency is named in the code its kind builds), plus `review`.

### CODE-13 Dependency licenses are permissive
`cargo-deny` allows only permissive licenses: MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause, 0BSD, ISC, Zlib, Unicode-3.0. It checks every crate that a build for one of the targets `just deny` names compiles: the platforms a developer builds on, and the image's. Any other license needs a rule change.
- Why: the template's own license is undecided, and a copyleft dependency would close off options (owner question Q1).
- Check: `ci: deny`.

### CODE-14 A new dependency states its purpose
The commit that adds a dependency says, in its message, what it is for and why the standard library or an existing dependency doesn't cover it, as does a pull request that brings it.
- Why: every dependency is code you maintain but didn't write.
- Check: `review`.

## Shape

### CODE-15 No dead code, no commented-out code
Unused items are deleted, not kept behind an exception to the `dead_code` lint. Code that has been commented out is deleted. An item of `platform`'s public API is used when code outside the tests calls it, or when the platform spec's Interface names it, since that section is the contract modules are built against, such as `JobContext`'s attempt, which a job's handler may read. An item that only the tests use is compiled only for them, with `#[cfg(test)]` or, for the tests of other crates, the `test-support` feature (TEST-05); any other item that only tests call is dead. A test target that includes another's support module through `#[path]`, as the end-to-end tests include the integration tests', excepts `dead_code` on that module's declaration alone, with its reason, since the target the module belongs to uses all of it, and the lint checks it there.
- Why: dead code costs reading time and implies a requirement that doesn't exist.
- Check: `clippy` (`-D warnings` refuses the `dead_code` lint, which a build only warns of, so code in progress still builds, in a run over the targets the image builds, without the tests' features, as well as one over every target; `app` exposes only its composition entry point publicly, so every module item is crate-visible and covered), `ci: check-layout` (no exception to `dead_code` in the code or the tests of `platform` and `app` but on a module that `#[path]` names, and no public item of `platform`, outside what only the tests compile, that no code outside the tests names and the Interface doesn't), plus `review` for an unused public item whose name another item shares, which the check can't tell apart, for the public fields, variants and trait methods it doesn't read, and for commented-out code.

### CODE-16 Public items are documented
Every public item in `platform`, and every item a module re-exports from `mod.rs`, has a doc comment (DOC-02).
- Why: a module's API is read far more often than it is written.
- Check: `clippy` (`missing_docs` on `platform`), plus `review` for module APIs.
