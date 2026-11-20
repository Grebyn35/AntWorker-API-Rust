//! A workspace's files and a user's own, public files, their downloads, users' avatars and
//! workspaces' pictures ([spec](../../../../../docs/spec/files.md)).

mod audit;
mod error;
mod events;
mod jobs;
mod metrics;
mod model;
mod personal_data;
mod repo;
mod routes;
mod service;

pub(crate) use audit::ACTIONS as AUDIT_ACTIONS;
pub(crate) use error::FilesError;
pub(crate) use model::{ExportedFile, LARGEST_UPLOAD_BYTES, Settings};
pub(crate) use personal_data::Contract;
pub(crate) use routes::routes;
pub(crate) use service::Files;
pub(crate) use service::avatar::{
    AVATAR_MAX_BYTES, NewAvatar, avatar_urls, delete_avatar, record_avatar, upload_avatar,
};
pub(crate) use service::exported::{exported, exported_bytes};
pub(crate) use service::picture::{
    NewPicture, PICTURE_MAX_BYTES, delete_picture, picture_urls, record_picture, upload_picture,
};
pub(crate) use service::restoring::{Restored, Stopped, restore};
