//! The telemetry of each user's clients: whether the user consents to their clients' sending it
//! ([spec](../../../../../docs/spec/client_telemetry.md)).

mod audit;
mod error;
mod events;
mod model;
mod personal_data;
mod repo;
mod routes;
mod service;

pub(crate) use audit::ACTIONS as AUDIT_ACTIONS;
pub(crate) use personal_data::Contract;
pub(crate) use routes::routes;
