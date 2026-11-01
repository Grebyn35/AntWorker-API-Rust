//! Who did what ([spec](../../../../../docs/spec/audit.md)).

#[expect(
    clippy::module_inception,
    reason = "each module's audit actions live in its audit.rs (ARCH-03), audit's own among them"
)]
mod audit;
mod error;
mod model;
mod repo;
mod service;

pub(crate) use error::AuditError;
pub(crate) use model::{Action, Entry, Resource};
pub(crate) use service::{Sink, record};
