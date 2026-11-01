//! Who a user is, and how they appear to others ([spec](../../../../../docs/spec/accounts.md)).

mod error;
mod model;
mod repo;
mod routes;
mod service;

pub(crate) use error::AccountsError;
pub(crate) use model::DisplayName;
pub(crate) use routes::routes;
pub(crate) use service::{Creation, create, find_by_email, matching, profiles};
