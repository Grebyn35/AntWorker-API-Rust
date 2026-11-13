//! What a workspace member may do ([spec](../../../../../docs/spec/access.md)).

mod error;
mod model;
mod permissions;
mod personal_data;
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
#[cfg(feature = "test-support")]
pub(crate) use permissions::matrix;
pub(crate) use permissions::{MembersRead, MembersRemove, WorkspaceRead, WorkspaceUpdate};
pub(crate) use personal_data::Contract;
pub(crate) use service::{
    Policy, Removal, add_owner, erasable, held, joined, leave, member_ids, members, remove,
    sole_workspaces, workspace_ids,
};
// part: invitations
pub(crate) use service::{add_member, hold_owner, is_member};
// end part: invitations
// part: files
pub(crate) use service::workspace_ids_of;
// end part: files
