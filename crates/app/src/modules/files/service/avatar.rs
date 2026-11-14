//! Users' avatars, which `accounts` sets and removes (the files spec's "Avatars", D120): an
//! avatar's upload, with the checks of any upload; its row, which `accounts` records in its own
//! unit of work; and the links of users' avatars.

use bytes::Bytes;
use platform::{db::UnitOfWork, types::UserId};

use super::{Files, Image, upload_image};
use crate::modules::files::{error::FilesError, repo::avatar as repo};

/// The most bytes an avatar holds, fixed, beside `FILE_MAX_BYTES`, since every member list
/// downloads each member's avatar, with which the route of `accounts` reads its body
/// (ACCOUNTS-009, D121).
pub(crate) const AVATAR_MAX_BYTES: usize = 1_048_576;

/// The prefix of the keys of avatars in the bucket.
pub(in crate::modules::files) const AVATAR_PREFIX: &str = "avatars/";

/// An avatar whose bytes are checked and scanned, and whose object is written, which
/// [`record_avatar`] makes a user's.
pub(crate) struct NewAvatar(Image);

/// Finds the kind of an avatar's `bytes`, which its route read, among the image kinds, from the
/// bytes alone; has the scanner scan them; writes their object outside any transaction; then has
/// `record` make it the caller's avatar, in a unit of work of its caller's, which calls
/// [`record_avatar`] (ACCOUNTS-009, DATA-21). An object whose row never commits is left to the
/// deletion of unreferenced objects (FILES-011), and the upload counts once in `files.uploads`,
/// by how it ended (FILES-013).
pub(crate) async fn upload_avatar<T, E, R>(
    files: &Files,
    bytes: Bytes,
    record: impl FnOnce(NewAvatar) -> R,
) -> Result<T, E>
where
    R: Future<Output = Result<T, E>>,
    E: From<FilesError>,
{
    upload_image(files, AVATAR_PREFIX, bytes, |image| {
        record(NewAvatar(image))
    })
    .await
}

/// Makes `avatar` the avatar of `user`, in place of any they had, whose row it deletes, and gives
/// its ID. Its caller has locked the user's row in `uow` already, so two changes of one user's
/// avatar take turns (the files spec's "Avatars").
pub(crate) async fn record_avatar(
    uow: &mut UnitOfWork,
    user: UserId,
    avatar: NewAvatar,
) -> Result<uuid::Uuid, FilesError> {
    let NewAvatar(Image { kind, object }) = avatar;
    repo::delete(uow, user).await?;
    Ok(repo::insert(
        uow,
        user,
        kind.media_type(),
        object.size_bytes,
        object.crc32,
        &object.key,
    )
    .await?)
}

/// Deletes the avatar of `user`, and gives its ID, if they had one. Its object stays until the
/// deletion of unreferenced objects deletes it (FILES-011).
pub(crate) async fn delete_avatar(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Option<uuid::Uuid>, FilesError> {
    Ok(repo::delete(uow, user).await?)
}

/// The path of the public download of each avatar among the users `users` names, for those that
/// have one, which came from a query filtered by tenant, or are the caller's own (FILES-024,
/// DATA-16).
pub(crate) async fn avatar_urls(
    uow: &mut UnitOfWork,
    users: &[UserId],
) -> Result<Vec<(UserId, String)>, FilesError> {
    Ok(repo::ids(uow, users)
        .await?
        .into_iter()
        .map(|(user, id)| (user, format!("/v1/public/avatars/{id}")))
        .collect())
}
