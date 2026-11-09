//! Uploading, listing, reading, downloading and deleting a workspace's files (the files spec's
//! Routes).

use axum::{
    extract::{RawPathParams, State},
    http::{HeaderMap, StatusCode},
};
use platform::{
    authz::Authorize,
    errors::Problem,
    http::{Download, FileBytes, Query, ResourceResponse, Routes, Upload, path_id},
    idempotency::IdempotencyKey,
    lists::List,
    ranges::Wanted,
    storage::ObjectBytes,
    types::WorkspaceId,
};
use utoipa_axum::routes;

use super::{
    DOWNLOAD_ERRORS, DownloadResponses, ListQuery, UPLOAD_ERRORS, UploadQuery, name, page,
    response, visibility,
};
use crate::modules::{
    access::{FilesDelete, FilesRead, FilesUpload},
    files::{
        error::FilesError,
        model::File,
        service::{Files, Sent, workspace as service},
    },
};

/// A workspace's routes.
pub(super) fn routes() -> Routes<Files> {
    Routes::new()
        .workspace_upload::<FilesUpload>(routes!(upload_file), &UPLOAD_ERRORS)
        .workspace::<FilesRead>(routes!(list_files), &[])
        .workspace::<FilesRead>(routes!(read_file), &[&FilesError::NotFound])
        .workspace_signed_download::<FilesRead>(routes!(download_file), &DOWNLOAD_ERRORS)
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
        UploadQuery,
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
    query: Query<UploadQuery>,
    upload: Upload,
) -> Result<ResourceResponse<File>, Problem<FilesError>> {
    let limit = files.settings.max_bytes;
    let visibility = visibility(&upload, limit, &query)?;
    authorize.check().await.map_err(FilesError::from)?;
    let name = name(&headers)?;
    // The turn the bytes hold lasts until `uploaded` is dropped, once the file is stored
    // (PLATFORM-074).
    let uploaded = upload
        .read(limit, &files.workers)
        .await
        .map_err(FilesError::from)?;
    let sent = Sent {
        name,
        visibility,
        bytes: uploaded.bytes.clone(),
        key: uploaded.key.as_ref(),
    };
    let file = service::upload(&files, authorize, sent).await?;
    let location = format!("/v1/workspaces/{}/files/{}", file.workspace_id, file.id);
    Ok(ResourceResponse::created(file, location))
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
    State(files): State<Files>,
    authorize: Authorize<FilesRead>,
    query: Query<ListQuery>,
) -> Result<axum::Json<List<File>>, Problem<FilesError>> {
    let (page, search) = page(&query)?;
    let (mut uow, authorized) = authorize.open().await.map_err(FilesError::from)?;
    let list = service::list(&files, &mut uow, &authorized, search.as_ref(), &page).await?;
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
    State(files): State<Files>,
    authorize: Authorize<FilesRead>,
    params: RawPathParams,
) -> Result<ResourceResponse<File>, Problem<FilesError>> {
    let id = path_id(&params, "file_id");
    let (mut uow, authorized) = authorize.open().await.map_err(FilesError::from)?;
    let id = id.ok_or(FilesError::NotFound)?;
    let file = service::read(&files, &mut uow, &authorized, id).await?;
    Ok(ResourceResponse::ok(file))
}

/// Downloads a file of the workspace: its bytes, or a range of them, served so that a browser
/// can't run them. A signed URL, which the file's `content_url` gives, authenticates it in place
/// of a credential.
#[utoipa::path(
    get,
    path = "/v1/workspaces/{workspace_id}/files/{file_id}/content",
    tag = "files",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("file_id" = uuid::Uuid, Path, description = "The file."),
    ),
    responses(DownloadResponses)
)]
async fn download_file(
    State(files): State<Files>,
    authorize: Authorize<FilesRead>,
    params: RawPathParams,
    wanted: Wanted,
) -> Result<Download<ObjectBytes>, Problem<FilesError>> {
    let id = path_id(&params, "file_id");
    let (uow, authorized) = authorize.open().await.map_err(FilesError::from)?;
    let id = id.ok_or(FilesError::NotFound)?;
    let downloaded = service::download(&files, uow, &authorized, id, &wanted).await?;
    Ok(response(downloaded))
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
