//! The workspace itself, and the members endpoints ([spec](../../../../../docs/spec/workspaces.md)).

mod audit;
mod error;
mod events;
mod model;
mod repo;
mod routes;
mod service;

pub(crate) use error::WorkspacesError; // part: invitations
pub(crate) use events::MEMBERSHIP_CREATED; // part: invitations
pub(crate) use model::WorkspaceName; // part: invitations
pub(crate) use routes::routes;
pub(crate) use service::{matching, names}; // part: invitations
