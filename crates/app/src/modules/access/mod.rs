//! What a workspace member may do ([spec](../../../../../docs/spec/access.md)).

mod error;
mod model;
mod permissions;
mod repo;
mod service;

pub(crate) use error::AccessError;
pub(crate) use model::{MemberFilter, Membership, Role};
// part: invitations
pub(crate) use permissions::InvitationsCreate;
// end part: invitations
// part: files
pub(crate) use permissions::{FilesDelete, FilesRead, FilesUpload};
// end part: files
pub(crate) use permissions::{MembersRead, MembersRemove, WorkspaceRead, WorkspaceUpdate, matrix};
pub(crate) use service::{
    Policy, Removal, add_owner, joined, member_ids, members, remove, workspace_ids,
};
// part: invitations
pub(crate) use service::{add_member, is_member};
// end part: invitations
