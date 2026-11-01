//! Who did what ([spec](../../../../../docs/spec/audit.md)).

mod model;
mod repo;
mod service;

pub(crate) use model::Entry;
pub(crate) use service::record;
