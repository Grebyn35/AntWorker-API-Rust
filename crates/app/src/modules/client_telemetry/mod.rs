//! The telemetry of each user's clients: whether the user consents to their clients' sending it,
//! and the proxy through which a consenting user's client sends it to the collector
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
pub(crate) use model::Settings;
pub(crate) use personal_data::Contract;
pub(crate) use routes::routes;
pub(crate) use service::ClientTelemetry;
