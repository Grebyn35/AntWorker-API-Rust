//! The reads of public files, by their ID and their visibility alone, which DATA-16 exempts, and
//! `check-tenancy` lets this file alone make: whoever holds a public file's ID may download it
//! (FILES-023, the files spec's "Visibility"). Each reads what its download serves, and never
//! the name, which a public download withholds (SEC-11).

use platform::{db::UnitOfWork, errors::Internal};

/// What a public file's download serves: its media type, its size, and the key of its object.
pub(in crate::modules::files) struct Public {
    pub(in crate::modules::files) id: uuid::Uuid,
    pub(in crate::modules::files) media_type: String,
    pub(in crate::modules::files) size_bytes: u64,
    pub(in crate::modules::files) object_key: String,
}

/// The public workspace file `id`, if there is one: none for a private one.
pub(in crate::modules::files) async fn workspace_file(
    uow: &mut UnitOfWork,
    id: uuid::Uuid,
) -> Result<Option<Public>, Internal> {
    let row = sqlx::query!(
        "SELECT media_type, size_bytes, object_key FROM workspace_files
        WHERE id = $1 AND visibility = 'public'",
        id,
    )
    .fetch_optional(uow.executor())
    .await?;
    row.map(|row| public(id, row.media_type, row.size_bytes, row.object_key))
        .transpose()
}

/// The public user file `id`, if there is one: none for a private one.
pub(in crate::modules::files) async fn user_file(
    uow: &mut UnitOfWork,
    id: uuid::Uuid,
) -> Result<Option<Public>, Internal> {
    let row = sqlx::query!(
        "SELECT media_type, size_bytes, object_key FROM user_files
        WHERE id = $1 AND visibility = 'public'",
        id,
    )
    .fetch_optional(uow.executor())
    .await?;
    row.map(|row| public(id, row.media_type, row.size_bytes, row.object_key))
        .transpose()
}

fn public(
    id: uuid::Uuid,
    media_type: String,
    size_bytes: i64,
    object_key: String,
) -> Result<Public, Internal> {
    Ok(Public {
        id,
        media_type,
        size_bytes: u64::try_from(size_bytes).map_err(Internal::new)?,
        object_key,
    })
}
