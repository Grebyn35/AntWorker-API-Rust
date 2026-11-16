//! The deployment's superadmins: who is one, which an operator's subcommand alone decides and
//! lists, and which ends with the user's account too, and what authorizes a superadmin's routes
//! ([spec](../../../../../docs/spec/superadmins.md)).

mod audit;
mod error;
mod model;
mod personal_data;
mod repo;
mod routes;
mod service;

pub(crate) use audit::ACTIONS as AUDIT_ACTIONS;
pub(crate) use model::{Change, Listed, Refusal};
pub(crate) use personal_data::Contract;
pub(crate) use routes::routes;
pub(crate) use service::{Policy, change, listed};
