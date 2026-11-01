//! The workspace itself, and the members endpoints ([spec](../../../../../docs/spec/workspaces.md)).

mod audit;
mod error;
mod events;
mod model;
mod repo;
mod routes;
mod service;

pub(crate) use error::WorkspacesError;
pub(crate) use events::MEMBERSHIP_CREATED;
pub(crate) use model::WorkspaceName;
pub(crate) use routes::routes;
pub(crate) use service::{matching, names};
