//! Inviting someone to a workspace by email, and the invitee seeing and accepting the
//! invitation ([spec](../../../../../docs/spec/invitations.md)).

mod audit;
mod error;
mod events;
mod expiry;
mod model;
mod repo;
mod routes;
mod service;

pub(crate) use model::Settings;
pub(crate) use routes::routes;
pub(crate) use service::Invitations;
