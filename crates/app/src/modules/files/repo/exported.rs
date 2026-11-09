//! The SQL of what a user's export holds of files: their own files, their avatar and the files of
//! each workspace whose only member is them, a page at a time, and the rows of `exported_files`
//! that hold their bytes (FILES-026). Every statement filters by the user whose export the job
//! builds, or by a workspace that came from their own memberships (DATA-16).

use platform::{
    db::UnitOfWork,
    errors::Internal,
    personal_data::PAGE,
    types::{UserId, WorkspaceId},
};

use crate::modules::files::model::ExportedFile;

/// A file's row, or an avatar's, as an export reads it.
pub(in crate::modules::files) struct Row {
    pub(in crate::modules::files) id: uuid::Uuid,
    pub(in crate::modules::files) name: String,
    pub(in crate::modules::files) media_type: String,
    pub(in crate::modules::files) size_bytes: i64,
    pub(in crate::modules::files) crc32: i64,
    pub(in crate::modules::files) visibility: String,
    pub(in crate::modules::files) object_key: String,
    pub(in crate::modules::files) uploader_id: Option<uuid::Uuid>,
    pub(in crate::modules::files) created_at: time::OffsetDateTime,
}

/// A page of `user`'s own files, the latest uploaded first, after `after`, a file's upload time
/// and ID, through the index on the user and that time.
pub(in crate::modules::files) async fn own(
    uow: &mut UnitOfWork,
    user: UserId,
    after: Option<(time::OffsetDateTime, uuid::Uuid)>,
) -> Result<Vec<Row>, Internal> {
    Ok(sqlx::query_as!(
        Row,
        r#"SELECT id, name, media_type, size_bytes, crc32, visibility::text AS "visibility!",
            object_key, NULL::uuid AS uploader_id, created_at
        FROM user_files
        WHERE user_id = $1 AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3))
        ORDER BY created_at DESC, id DESC
        LIMIT $4"#,
        user.uuid(),
        after.map(|after| after.0),
        after.map(|after| after.1),
        PAGE,
    )
    .fetch_all(uow.executor())
    .await?)
}

/// `user`'s avatar, if they have one, as a row with no name, public.
pub(in crate::modules::files) async fn avatar(
    uow: &mut UnitOfWork,
    user: UserId,
) -> Result<Option<Row>, Internal> {
    Ok(sqlx::query_as!(
        Row,
        r#"SELECT id, '' AS "name!", media_type, size_bytes, crc32, 'public' AS "visibility!",
            object_key, NULL::uuid AS uploader_id, created_at
        FROM avatars
        WHERE user_id = $1"#,
        user.uuid(),
    )
    .fetch_optional(uow.executor())
    .await?)
}

/// A page of the files of `workspace`, one whose only member is the user, which came from their
/// own memberships, the latest uploaded first, after `after`, through the index on the workspace
/// and the upload's time.
pub(in crate::modules::files) async fn of_workspace(
    uow: &mut UnitOfWork,
    workspace: WorkspaceId,
    after: Option<(time::OffsetDateTime, uuid::Uuid)>,
) -> Result<Vec<Row>, Internal> {
    Ok(sqlx::query_as!(
        Row,
        r#"SELECT id, name, media_type, size_bytes, crc32, visibility::text AS "visibility!",
            object_key, uploader_id, created_at
        FROM workspace_files
        WHERE workspace_id = $1 AND ($2::timestamptz IS NULL OR (created_at, id) < ($2, $3))
        ORDER BY created_at DESC, id DESC
        LIMIT $4"#,
        workspace.uuid(),
        after.map(|after| after.0),
        after.map(|after| after.1),
        PAGE,
    )
    .fetch_all(uow.executor())
    .await?)
}

/// Inserts a row of `exported_files` for each of `files`, the files of `user`'s export `export`
/// from `first` on, at their positions, so their objects stay while it lasts (FILES-011).
pub(in crate::modules::files) async fn hold(
    uow: &mut UnitOfWork,
    user: UserId,
    export: uuid::Uuid,
    first: u32,
    files: &[ExportedFile],
) -> Result<(), Internal> {
    let mut positions = Vec::with_capacity(files.len());
    let mut paths = Vec::with_capacity(files.len());
    let mut keys = Vec::with_capacity(files.len());
    let mut sizes = Vec::with_capacity(files.len());
    let mut crcs = Vec::with_capacity(files.len());
    for (offset, file) in files.iter().enumerate() {
        let offset = u32::try_from(offset).map_err(Internal::new)?;
        positions.push(i32::try_from(first + offset).map_err(Internal::new)?);
        paths.push(file.path.clone());
        keys.push(file.object_key.clone());
        sizes.push(i64::try_from(file.size_bytes).map_err(Internal::new)?);
        crcs.push(i64::from(file.crc32));
    }
    sqlx::query!(
        "INSERT INTO exported_files (export_id, user_id, position, path, object_key, size_bytes, crc32)
        SELECT $1, $2, held.position, held.path, held.object_key, held.size_bytes, held.crc32
        FROM unnest($3::integer[], $4::text[], $5::text[], $6::bigint[], $7::bigint[])
            AS held (position, path, object_key, size_bytes, crc32)",
        export,
        user.uuid(),
        &positions,
        &paths,
        &keys,
        &sizes,
        &crcs,
    )
    .execute(uow.executor())
    .await?;
    Ok(())
}

/// The files whose bytes `user`'s export `export` holds, in their order.
pub(in crate::modules::files) async fn held(
    uow: &mut UnitOfWork,
    user: UserId,
    export: uuid::Uuid,
) -> Result<Vec<ExportedFile>, Internal> {
    let rows = sqlx::query!(
        "SELECT path, object_key, size_bytes, crc32 FROM exported_files
        WHERE export_id = $1 AND user_id = $2
        ORDER BY position",
        export,
        user.uuid(),
    )
    .fetch_all(uow.executor())
    .await?;
    rows.into_iter()
        .map(|row| {
            Ok(ExportedFile {
                path: row.path,
                object_key: row.object_key,
                size_bytes: u64::try_from(row.size_bytes).map_err(Internal::new)?,
                crc32: u32::try_from(row.crc32).map_err(Internal::new)?,
            })
        })
        .collect()
}
