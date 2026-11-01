//! Who a caller is: sign-in methods and sessions ([spec](../../../../../docs/spec/auth.md)).

mod audit;
mod error;
mod metrics;
mod model;
mod repo;
mod routes;
mod service;

pub(crate) use model::Lifetimes;
pub(crate) use routes::routes;
pub(crate) use service::{Auth, Settings, Verifier};
