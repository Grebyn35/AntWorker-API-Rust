//! Who did what ([spec](../../../../../docs/spec/audit.md)).

#[expect(
    clippy::module_inception,
    reason = "each module's audit actions live in its audit.rs (ARCH-03), audit's own among them"
)]
mod audit;
mod error;
mod model;
mod personal_data;
mod repo;
mod routes;
mod service;

pub(crate) use model::{Action, Entry, Resource};
pub(crate) use personal_data::Contract;
pub(crate) use routes::routes;
pub(crate) use service::{Sink, record};
