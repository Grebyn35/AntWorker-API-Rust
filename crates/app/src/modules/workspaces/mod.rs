//! The workspace itself, and the members endpoints ([spec](../../../../../docs/spec/workspaces.md)).

mod audit;
mod error;
mod events;
mod model;
mod repo;
mod routes;
mod service;

pub(crate) use routes::routes;
