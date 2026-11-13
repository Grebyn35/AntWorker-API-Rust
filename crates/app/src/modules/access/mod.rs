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
pub(crate) use permissions::{
    MembersRead, MembersRemove, MembersUpdate, WorkspaceDelete, WorkspaceLeave, WorkspaceRead,
    WorkspaceTransfer, WorkspaceUpdate,
};
pub(crate) use personal_data::Contract;
pub(crate) use service::{
    Policy, Removal, RoleChange, Transfer, add_owner, change_role, dissolve, end_membership,
    erasable, held, joined, leave, member, member_ids, members, remove, sole_workspaces, transfer,
    workspace_ids,
};
// part: invitations
pub(crate) use service::{add_member, hold_owner, is_member};
// end part: invitations
// part: files
pub(crate) use service::workspace_ids_of;
// end part: files
