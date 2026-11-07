//! A workspace's files ([spec](../../../../../docs/spec/files.md)).

mod audit;
mod error;
mod events;
mod jobs;
mod metrics;
mod model;
mod repo;
mod routes;
mod service;

pub(crate) use model::Settings;
pub(crate) use routes::routes;
pub(crate) use service::Files;
