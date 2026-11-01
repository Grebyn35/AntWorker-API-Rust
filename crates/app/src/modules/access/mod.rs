//! What a workspace member may do ([spec](../../../../../docs/spec/access.md)).

mod error;
mod model;
mod permissions;
mod policy;
mod repo;
mod service;

pub(crate) use error::AccessError;
pub(crate) use model::{MemberFilter, Role};
pub(crate) use permissions::{MembersRead, MembersRemove, matrix};
pub(crate) use policy::Policy;
pub(crate) use service::{Removal, add_owner, joined, member_ids, members, memberships, remove};
