//! Who did what ([spec](../../../../../docs/spec/audit.md)).

mod error;
mod model;
mod repo;
mod service;

pub(crate) use error::AuditError;
pub(crate) use model::{Action, Entry, Resource};
pub(crate) use service::record;
