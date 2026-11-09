//! The public files that anyone may download without signing in, read by a public file's ID
//! alone, and named by it, never by their own names (FILES-023, the files spec's "Visibility").

use platform::{storage::Wanted, types::Client};

use super::{Downloaded, Files, Serving, serve};
use crate::modules::files::{
    error::FilesError,
    model::Kind,
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

/// What a public download of the file `id` of `owner`'s kind serves, as `wanted` asks for it,
/// named by the file's ID and its kind's extension, never by its own name. An ID that names no public file of that kind, a private one included, answers
/// 404 (FILES-023). The unit of work ends once the row is read, before the bucket is asked
/// (DATA-21).
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
