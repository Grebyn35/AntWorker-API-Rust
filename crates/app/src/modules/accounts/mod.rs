//! Who a user is, and how they appear to others ([spec](../../../../../docs/spec/accounts.md)).

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

pub(crate) use error::AccountsError;
pub(crate) use model::{Address, DisplayName, Export, PublicProfile};
pub(crate) use routes::{export_routes, routes};
// part: files
pub(crate) use routes::avatar_routes;
pub(crate) use service::avatar::Avatars;
// end part: files
// part: invitations
pub(crate) use service::profile;
// end part: invitations
pub(crate) use personal_data::Contract;
pub(crate) use service::deletion::{Deletion, Outcome as DeletionOutcome};
pub(crate) use service::exports::Exports;
pub(crate) use service::{
    Creation, EmailChange, address, address_for_update, change_email, create, find_by_email,
    matching, profiles, verify_email,
};
