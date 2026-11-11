//! Who a caller is: sign-in methods and sessions ([spec](../../../../../docs/spec/auth.md)).

mod audit;
mod emails;
mod error;
mod events;
mod jobs;
mod metrics;
mod model;
mod personal_data;
mod repo;
mod routes;
mod service;

pub(crate) use jobs::register;
pub(crate) use model::Lifetimes;
pub(crate) use personal_data::Contract;
pub(crate) use routes::routes;
pub(crate) use service::{Auth, Keys, Settings, Verifier};
