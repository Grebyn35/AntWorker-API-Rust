//! A workspace's files and a user's own, public files, their downloads, and users' avatars
//! ([spec](../../../../../docs/spec/files.md)).

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

pub(crate) use error::FilesError;
pub(crate) use model::Settings;
pub(crate) use personal_data::Contract;
pub(crate) use routes::routes;
pub(crate) use service::Files;
pub(crate) use service::avatar::{
    AVATAR_MAX_BYTES, NewAvatar, avatar_urls, delete_avatar, record_avatar, upload_avatar,
};
