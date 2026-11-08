//! The feature modules (architecture §5). Each is used only through its `mod.rs` (ARCH-04).

pub(crate) mod access;
pub(crate) mod accounts;
pub(crate) mod audit;
pub(crate) mod auth;
// part: files
pub(crate) mod files;
// end part: files
// part: invitations
pub(crate) mod invitations;
// end part: invitations
pub(crate) mod realtime;
pub(crate) mod system;
pub(crate) mod workspaces;
