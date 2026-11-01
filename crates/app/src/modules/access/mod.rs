//! What a workspace member may do ([spec](../../../../../docs/spec/access.md)).

mod error;
mod model;
mod permissions;
mod repo;
mod service;

pub(crate) use error::AccessError;
pub(crate) use model::{MemberFilter, Membership, Role};
pub(crate) use permissions::{MembersRead, MembersRemove, matrix};
pub(crate) use service::{
    Policy, Removal, add_owner, joined, member_ids, members, remove, workspace_ids,
};
