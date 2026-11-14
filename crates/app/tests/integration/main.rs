//! Integration tests: behaviors through the API's real code, against real Postgres (TEST-03).
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
// part: files
#[cfg(test)]
mod files;
// end part: files
// part: invitations
#[cfg(test)]
mod invitations;
// end part: invitations
#[cfg(test)]
mod openapi;
#[cfg(test)]
mod platform;
#[cfg(test)]
mod realtime;
#[cfg(test)]
mod support;
#[cfg(test)]
mod system;
// part: workspace_creation
#[cfg(test)]
mod workspace_creation;
// end part: workspace_creation
// part: workspace_setup
#[cfg(test)]
mod workspace_setup;
// end part: workspace_setup
#[cfg(test)]
mod workspaces;
