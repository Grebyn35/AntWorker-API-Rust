//! Uploading, listing, reading, downloading and deleting a workspace's files (the files spec's
//! Routes).

use axum::{
    extract::{RawPathParams, State},
    http::{HeaderMap, StatusCode},
};
use platform::{
    authz::Authorize,
    errors::Problem,
    http::{
        Download, FieldErrors, FileBytes, Query, ResourceResponse, Routes, Upload,
        disposition_name, path_id,
    },
    idempotency::IdempotencyKey,
    lists::{Cursor, Limit, List, Page, Search},
    storage::ObjectBytes,
    types::WorkspaceId,
};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::routes;

use super::{
    error::FilesError,
    events::{FILE_DELETED, FILE_UPLOADED},
    model::{File, FileName, Kind},
    service::{self, Files},
};
use crate::modules::access::{FilesDelete, FilesRead, FilesUpload};

/// The module's routes, and the events they publish.
pub(crate) fn routes() -> Routes<Files> {
    Routes::new()
        .events(&[&FILE_UPLOADED, &FILE_DELETED])
        .workspace_upload::<FilesUpload>(
            routes!(upload_file),
            &[
                &FilesError::InvalidName(None),
                &FilesError::Empty,
                &FilesError::UnsupportedType,
                &FilesError::MalwareDetected,
                &FilesError::ScannerUnavailable(None),
                &FilesError::StorageUnavailable(None),
            ],
        )
        .workspace::<FilesRead>(routes!(list_files), &[])
        .workspace::<FilesRead>(routes!(read_file), &[&FilesError::NotFound])
        .workspace::<FilesRead>(
            routes!(download_file),
            &[&FilesError::NotFound, &FilesError::StorageUnavailable(None)],
        )
        .workspace::<FilesDelete>(routes!(delete_file), &[&FilesError::NotFound])
}

/// Uploads a file to the workspace: its bytes as the body, and its name in `Content-Disposition`.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{workspace_id}/files",
    tag = "files",
    request_body(content = FileBytes, content_type = "application/octet-stream",
        description = "The file's bytes, at most `FILE_MAX_BYTES` (FILES-004). Its kind comes from its name's extension and its bytes alone (FILES-003)."),
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("Content-Disposition" = String, Header,
            description = "The file's name, as `filename*`, encoded as RFC 8187 says, or `filename`, such as `attachment; filename*=UTF-8''report.pdf` (FILES-002)."),
        ("Idempotency-Key" = inline(Option<IdempotencyKey>), Header,
            description = "A key of the caller's own, as a quoted string, so a retry stores nothing twice (API-12)."),
    ),
    responses(
        (status = 201, description = "The new file.", body = File,
            headers(
                ("location" = String, description = "The new file: `/v1/workspaces/{workspace_id}/files/{id}`."),
            )),
    )
)]
async fn upload_file(
    State(files): State<Files>,
    authorize: Authorize<FilesUpload>,
    headers: HeaderMap,
    upload: Upload,
) -> Result<ResourceResponse<File>, Problem<FilesError>> {
    let limit = files.settings.max_bytes;
    upload.refuse_declared(limit).map_err(FilesError::from)?;
    authorize.check().await.map_err(FilesError::from)?;
    let name = disposition_name(&headers).ok_or(FilesError::InvalidName(None))?;
    let name = FileName::parse(&name).map_err(|invalid| FilesError::InvalidName(Some(invalid)))?;
    // The turn the bytes hold lasts until `uploaded` is dropped, once the file is stored
    // (PLATFORM-074).
    let uploaded = upload
        .read(limit, &files.workers)
        .await
        .map_err(FilesError::from)?;
    let file = service::upload(
        &files,
        authorize,
        name,
        uploaded.bytes.clone(),
        uploaded.key.as_ref(),
    )
    .await?;
    let location = format!("/v1/workspaces/{}/files/{}", file.workspace_id, file.id);
    Ok(ResourceResponse::created(file, location))
}

/// The query of `GET /v1/workspaces/{workspace_id}/files` (FILES-007).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub(crate) struct ListQuery {
    /// How many files a page holds.
    #[param(value_type = Option<Limit>, inline)]
    limit: Option<String>,
    /// The `next_cursor` of the page before.
    #[param(value_type = Option<Cursor>, inline)]
    cursor: Option<String>,
    /// Only the files whose name contains this, ignoring case (API-20).
    #[param(value_type = Option<Search>, inline)]
    search: Option<String>,
}

/// Lists the workspace's files, the latest uploaded first.
#[utoipa::path(
    get,
    path = "/v1/workspaces/{workspace_id}/files",
    tag = "files",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ListQuery,
    ),
    responses(
        (status = 200, description = "A page of the workspace's files.", body = List<File>),
    )
)]
async fn list_files(
    authorize: Authorize<FilesRead>,
    query: Query<ListQuery>,
) -> Result<axum::Json<List<File>>, Problem<FilesError>> {
    let params = query.params();
    let mut errors = FieldErrors::new();
    let page = Page::read(
        query.route(),
        params.limit.as_deref(),
        params.cursor.as_deref(),
        &mut errors,
    );
    let search = Search::read(params.search.as_deref(), &mut errors);
    let (page, search) = errors
        .finish_query(page.zip(search))
        .map_err(FilesError::from)?;
    let (mut uow, authorized) = authorize.open().await.map_err(FilesError::from)?;
    let list = service::list(&mut uow, &authorized, search.as_ref(), &page).await?;
    Ok(axum::Json(list))
}

/// Reads a file of the workspace.
#[utoipa::path(
    get,
    path = "/v1/workspaces/{workspace_id}/files/{file_id}",
    tag = "files",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("file_id" = uuid::Uuid, Path, description = "The file."),
    ),
    responses(
        (status = 200, description = "The file.", body = File),
    )
)]
async fn read_file(
    authorize: Authorize<FilesRead>,
    params: RawPathParams,
) -> Result<ResourceResponse<File>, Problem<FilesError>> {
    let id = path_id(&params, "file_id");
    let (mut uow, authorized) = authorize.open().await.map_err(FilesError::from)?;
    let id = id.ok_or(FilesError::NotFound)?;
    let file = service::read(&mut uow, &authorized, id).await?;
    Ok(ResourceResponse::ok(file))
}

/// Downloads a file of the workspace: its bytes, served so that a browser can't run them.
#[utoipa::path(
    get,
    path = "/v1/workspaces/{workspace_id}/files/{file_id}/content",
    tag = "files",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("file_id" = uuid::Uuid, Path, description = "The file."),
    ),
    responses(
        (status = 200, description = "The file's bytes, as its media type.",
            content(
                (FileBytes = "image/png"),
                (FileBytes = "image/jpeg"),
                (FileBytes = "image/gif"),
                (FileBytes = "image/webp"),
                (FileBytes = "application/pdf"),
                (FileBytes = "application/vnd.openxmlformats-officedocument.wordprocessingml.document"),
                (FileBytes = "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"),
                (FileBytes = "application/vnd.openxmlformats-officedocument.presentationml.presentation"),
                (FileBytes = "text/plain"),
                (FileBytes = "text/csv"),
                (FileBytes = "text/markdown"),
            ),
            headers(
                ("content-disposition" = String, description = "`inline` for an image and `attachment` for every other kind, with the file's name (FILES-009)."),
                ("content-security-policy" = String, description = "`default-src 'none'; sandbox`, so the file runs nothing (SEC-11)."),
            )),
    )
)]
async fn download_file(
    State(files): State<Files>,
    authorize: Authorize<FilesRead>,
    params: RawPathParams,
) -> Result<Download<ObjectBytes>, Problem<FilesError>> {
    let id = path_id(&params, "file_id");
    let (uow, authorized) = authorize.open().await.map_err(FilesError::from)?;
    let id = id.ok_or(FilesError::NotFound)?;
    let (stored, object) = service::download(&files, uow, &authorized, id).await?;
    let inline = Kind::of_media_type(&stored.file.media_type).is_some_and(Kind::inline);
    Ok(Download {
        media_type: stored.file.media_type,
        length: object.size,
        inline,
        name: stored.file.name.as_str().to_owned(),
        bytes: object.bytes,
    })
}

/// Deletes a file of the workspace.
#[utoipa::path(
    delete,
    path = "/v1/workspaces/{workspace_id}/files/{file_id}",
    tag = "files",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("file_id" = uuid::Uuid, Path, description = "The file."),
    ),
    responses(
        (status = 204, description = "The file was deleted."),
    )
)]
async fn delete_file(
    authorize: Authorize<FilesDelete>,
    params: RawPathParams,
) -> Result<StatusCode, Problem<FilesError>> {
    let id = path_id(&params, "file_id");
    let (uow, authorized) = authorize.open().await.map_err(FilesError::from)?;
    let id = id.ok_or(FilesError::NotFound)?;
    service::delete(uow, &authorized, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use platform::telemetry::Service;

    use super::*;

    #[test]
    fn files_009_the_download_documents_the_media_type_of_every_kind() {
        let document = routes().openapi(Service {
            name: "api",
            version: "0.0.0",
        });
        let document = serde_json::to_value(&document).unwrap();
        let content = &document["paths"]["/v1/workspaces/{workspace_id}/files/{file_id}/content"]["get"]
            ["responses"]["200"]["content"];
        let mut documented: Vec<&str> = content
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        // A kind's stored type with its parameters, such as `text/plain; charset=utf-8`, is
        // documented as its essence, `text/plain`.
        let mut stored: Vec<&str> = Kind::every()
            .iter()
            .map(|kind| kind.media_type().split(';').next().unwrap_or_default())
            .collect();
        documented.sort_unstable();
        stored.sort_unstable();

        assert_eq!(documented, stored);
    }
}
