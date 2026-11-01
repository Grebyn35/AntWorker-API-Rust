//! What a workspace member may do ([spec](../../../../../docs/spec/access.md)).

mod error;
mod model;
mod permissions;
mod repo;
mod service;

pub(crate) use error::AccessError;
pub(crate) use model::{MemberFilter, Membership, Role};
pub(crate) use permissions::{InvitationsCreate, MembersRead, MembersRemove, matrix};
pub(crate) use service::{
    Policy, Removal, add_member, add_owner, is_member, joined, member_ids, members, remove,
    workspace_ids,
};
