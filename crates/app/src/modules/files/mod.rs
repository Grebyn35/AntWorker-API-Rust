//! A workspace's files ([spec](../../../../../docs/spec/files.md)).

mod audit;
mod error;
mod events;
mod model;
mod repo;
mod routes;
mod service;

pub(crate) use routes::routes;
