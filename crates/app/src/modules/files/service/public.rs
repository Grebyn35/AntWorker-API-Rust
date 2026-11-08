//! The public files that anyone may download without signing in, read by a public file's ID
//! alone, without its name (FILES-023, the files spec's "Visibility").

use platform::types::Client;

use super::{Files, Serving};
use crate::modules::files::{
    error::FilesError,
    repo::public::{self as repo, Public},
};

/// The kind of public file a download names: a workspace's or a user's.
#[derive(Clone, Copy, Debug)]
pub(in crate::modules::files) enum Owner {
    /// A workspace's file.
    Workspace,
    /// A user's own file.
    User,
}

/// The public file `id` of `owner`'s kind, as its public download serves it, with no name. An ID
/// that names no public file of that kind, a private one included, answers 404 (FILES-023). The
/// unit of work ends once the row is read, before the bucket is asked (DATA-21).
pub(in crate::modules::files) async fn download(
    files: &Files,
    client: &Client,
    owner: Owner,
    id: uuid::Uuid,
) -> Result<Serving, FilesError> {
    let mut uow = files.database.begin(client).await?;
    let found = match owner {
        Owner::Workspace => repo::workspace_file(&mut uow, id).await?,
        Owner::User => repo::user_file(&mut uow, id).await?,
    };
    drop(uow);
    let Public {
        id,
        media_type,
        size_bytes,
        object_key,
    } = found.ok_or(FilesError::NotFound)?;
    Ok(Serving {
        id,
        media_type,
        size_bytes,
        object_key,
        name: None,
    })
}
