//! End-to-end tests: guarantees of the built image, which run it as an operator would (TEST-04).
//! `just e2e` pushes the image in target/image to the registry that Compose runs, and the tests
//! run it from there by digest, on the platform `E2E_PLATFORM` names, against the Postgres that
//! `just services` starts, or another server of it. Only the `e2e` feature compiles them, because
//! they need a built image.
//!
//! `cfg(test)` always holds here; it marks the helpers as test code, where clippy allows
//! `unwrap` and `expect` (CODE-03).

#[cfg(test)]
mod backups;
#[cfg(test)]
mod deploy;
#[cfg(test)]
mod deployment;
#[cfg(test)]
mod docker;
#[cfg(test)]
mod image;
#[cfg(test)]
mod images;
#[cfg(test)]
mod server;
#[cfg(test)]
mod setup;
#[cfg(test)]
mod stack;
#[cfg(test)]
#[expect(
    dead_code,
    unused_imports,
    reason = "the end-to-end tests use the support's databases and requests, and not its \
              in-process API"
)]
#[path = "../integration/support/mod.rs"]
mod support;
#[cfg(test)]
mod traces;
