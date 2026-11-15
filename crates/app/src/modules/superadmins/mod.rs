//! The deployment's superadmins: who is one, which only an operator's subcommand changes, and
//! what authorizes a superadmin's routes ([spec](../../../../../docs/spec/superadmins.md)).

mod audit;
mod error;
mod model;
mod personal_data;
mod repo;
mod routes;
mod service;

pub(crate) use model::{Change, Refusal};
pub(crate) use personal_data::Contract;
pub(crate) use routes::routes;
pub(crate) use service::{Policy, change};
