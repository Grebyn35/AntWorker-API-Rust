//! A workspace's files and a user's own, public files, and their downloads
//! ([spec](../../../../../docs/spec/files.md)).

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
