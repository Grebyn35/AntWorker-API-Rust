//! Who a user is, and how they appear to others ([spec](../../../../../docs/spec/accounts.md)).

mod error;
mod model;
mod repo;
mod routes;
mod service;

pub(crate) use error::AccountsError;
pub(crate) use model::{DisplayName, Profile};
pub(crate) use routes::routes;
pub(crate) use service::{create, find_by_email, profile};
