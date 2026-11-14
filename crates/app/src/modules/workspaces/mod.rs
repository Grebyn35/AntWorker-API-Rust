//! The workspace itself, and the members endpoints ([spec](../../../../../docs/spec/workspaces.md)).

mod audit;
mod error;
mod events;
mod model;
mod personal_data;
mod repo;
mod routes;
mod service;

// part: invitations
pub(crate) use error::WorkspacesError;
pub(crate) use events::{MEMBERSHIP_CREATED, MembershipCreated};
pub(crate) use model::WorkspaceName;
// end part: invitations
pub(crate) use personal_data::Contract;
pub(crate) use routes::routes;
// part: files
pub(crate) use routes::picture_routes;
pub(crate) use service::picture::Pictures;
// end part: files
// part: invitations
pub(crate) use service::{matching, names};
// end part: invitations
