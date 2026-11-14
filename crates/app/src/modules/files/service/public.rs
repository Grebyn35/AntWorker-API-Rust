//! The public files, the avatars and the workspaces' pictures that anyone may download without
//! signing in, read by their ID alone, and named by it, never by a file's own name (FILES-023,
//! FILES-024, FILES-027, the files spec's "Visibility").

use platform::{ranges::Wanted, types::Client};

use super::{Downloaded, Files, Serving, serve};
use crate::modules::files::{
    error::FilesError,
    model::Kind,
    repo::public::{self as repo, Public},
};

/// The kind of public file a download names: a workspace's, a user's, an avatar or a workspace's
/// picture.
#[derive(Clone, Copy, Debug)]
pub(in crate::modules::files) enum Owner {
    /// A workspace's file.
    Workspace,
    /// A user's own file.
    User,
    /// A user's avatar (FILES-024).
    Avatar,
    /// A workspace's picture (FILES-027).
    Picture,
}

/// What a public download of the file `id` of `owner`'s kind serves, as `wanted` asks for it,
/// named by the file's ID and its kind's extension, never by its own name. An ID that names no
/// public file of that kind, a private one included, answers 404 (FILES-023, FILES-024,
/// FILES-027). The unit of work ends once the row is read, before the bucket is asked (DATA-21).
pub(in crate::modules::files) async fn download(
    files: &Files,
    client: &Client,
    owner: Owner,
    id: uuid::Uuid,
    wanted: &Wanted,
) -> Result<Downloaded, FilesError> {
    let mut uow = files.database.begin(client).await?;
    let found = match owner {
        Owner::Workspace => repo::workspace_file(&mut uow, id).await?,
        Owner::User => repo::user_file(&mut uow, id).await?,
        Owner::Avatar => repo::avatar(&mut uow, id).await?,
        Owner::Picture => repo::picture(&mut uow, id).await?,
    };
    drop(uow);
    let Public {
        id,
        media_type,
        size_bytes,
        object_key,
    } = found.ok_or(FilesError::NotFound)?;
    // The file's ID and its kind's extension, so every browser saves it with that extension,
    // while its own name reaches only those who may read the file (FILES-023, D115).
    let name = match Kind::of_media_type(&media_type) {
        Some(kind) => format!("{id}.{}", kind.extension()),
        None => id.to_string(),
    };
    let serving = Serving {
        id,
        media_type,
        size_bytes,
        object_key,
        name,
        public: true,
    };
    serve(files, serving, wanted).await
}
