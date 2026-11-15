//! Inviting someone to a workspace by email, what the workspace's members do with its
//! invitations, and the invitee seeing, accepting and declining an invitation
//! ([spec](../../../../../docs/spec/invitations.md)).

mod audit;
mod emails;
mod error;
mod events;
mod jobs;
mod model;
mod personal_data;
mod repo;
mod routes;
mod service;

pub(crate) use model::Settings;
pub(crate) use personal_data::Contract;
pub(crate) use routes::routes;
pub(crate) use service::Invitations;
