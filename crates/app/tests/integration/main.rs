//! Integration tests: behaviors through the API's real code, against real Postgres (TEST-04).
//! They form one test binary, so the test support compiles once and is shared.
//!
//! `cfg(test)` always holds here; it marks the helpers as test code, where clippy allows
//! `unwrap` and `expect` (CODE-03).

#[cfg(test)]
mod access;
#[cfg(test)]
mod accounts;
#[cfg(test)]
mod audit;
#[cfg(test)]
mod auth;
#[cfg(test)]
mod invitations;
#[cfg(test)]
mod mail;
#[cfg(test)]
mod openapi;
#[cfg(test)]
mod platform;
#[cfg(test)]
mod support;
#[cfg(test)]
mod system;
#[cfg(test)]
mod workspaces;
