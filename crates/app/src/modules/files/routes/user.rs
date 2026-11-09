//! Uploading, listing, reading, downloading and deleting the caller's own files (the files spec's
//! Routes).

use axum::{
    extract::{RawPathParams, State},
    http::{HeaderMap, StatusCode},
};
use platform::{
    authn::Principal,
    errors::Problem,
    http::{Download, FileBytes, Query, ResourceResponse, Routes, Upload, path_id},
    idempotency::IdempotencyKey,
    lists::List,
    ranges::Wanted,
    storage::ObjectBytes,
    types::Client,
};
use utoipa_axum::routes;

use super::{
    DOWNLOAD_ERRORS, DownloadResponses, ListQuery, UPLOAD_ERRORS, UploadQuery, name, page,
    response, visibility,
};
use crate::modules::files::{
    error::FilesError,
    model::UserFile,
    service::{Files, Sent, user as service},
};

/// A user's routes of their own files.
pub(super) fn routes() -> Routes<Files> {
    Routes::new()
        .user_upload(routes!(upload_own_file), &UPLOAD_ERRORS)
        .user(routes!(list_own_files), &[])
        .user(routes!(read_own_file), &[&FilesError::NotFound])
        .user_signed_download(routes!(download_own_file), &DOWNLOAD_ERRORS)
        .user(routes!(delete_own_file), &[&FilesError::NotFound])
}

/// Uploads a file to the caller's own files: its bytes as the body, and its name in
/// `Content-Disposition`.
#[utoipa::path(
    post,
    path = "/v1/users/me/files",
    tag = "files",
    request_body(content = FileBytes, content_type = "application/octet-stream",
        description = "The file's bytes, at most `FILE_MAX_BYTES` (FILES-004). Its kind comes from its name's extension and its bytes alone (FILES-003)."),
    params(
        UploadQuery,
        ("Content-Disposition" = String, Header,
            description = "The file's name, as `filename*`, encoded as RFC 8187 says, or `filename`, such as `attachment; filename*=UTF-8''report.pdf` (FILES-002)."),
        ("Idempotency-Key" = inline(Option<IdempotencyKey>), Header,
            description = "A key of the caller's own, as a quoted string, so a retry stores nothing twice (API-12)."),
    ),
    responses(
        (status = 201, description = "The new file.", body = UserFile,
            headers(
                ("location" = String, description = "The new file: `/v1/users/me/files/{id}`."),
            )),
    )
)]
async fn upload_own_file(
    State(files): State<Files>,
    principal: Principal,
    client: Client,
    headers: HeaderMap,
    query: Query<UploadQuery>,
    upload: Upload,
) -> Result<ResourceResponse<UserFile>, Problem<FilesError>> {
    let limit = files.settings.max_bytes;
    let visibility = visibility(&upload, limit, &query)?;
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
    let file = service::upload(&files, &principal, &client, sent).await?;
    let location = format!("/v1/users/me/files/{}", file.id);
    Ok(ResourceResponse::created(file, location))
}

/// Lists the caller's own files, the latest uploaded first.
#[utoipa::path(
    get,
    path = "/v1/users/me/files",
    tag = "files",
    params(ListQuery),
    responses(
        (status = 200, description = "A page of the caller's own files.", body = List<UserFile>),
    )
)]
async fn list_own_files(
    State(files): State<Files>,
    principal: Principal,
    client: Client,
    query: Query<ListQuery>,
) -> Result<axum::Json<List<UserFile>>, Problem<FilesError>> {
    let (page, search) = page(&query)?;
    let list = service::list(&files, &client, &principal, search.as_ref(), &page).await?;
    Ok(axum::Json(list))
}

/// Reads a file of the caller's own.
#[utoipa::path(
    get,
    path = "/v1/users/me/files/{file_id}",
    tag = "files",
    params(
        ("file_id" = uuid::Uuid, Path, description = "The file."),
    ),
    responses(
        (status = 200, description = "The file.", body = UserFile),
    )
)]
async fn read_own_file(
    State(files): State<Files>,
    principal: Principal,
    client: Client,
    params: RawPathParams,
) -> Result<ResourceResponse<UserFile>, Problem<FilesError>> {
    let id = path_id(&params, "file_id").ok_or(FilesError::NotFound)?;
    let file = service::read(&files, &client, &principal, id).await?;
    Ok(ResourceResponse::ok(file))
}

/// Downloads a file of the caller's own: its bytes, or a range of them, served so that a browser
/// can't run them. A signed URL, which the file's `content_url` gives, authenticates it in place
/// of a credential.
#[utoipa::path(
    get,
    path = "/v1/users/me/files/{file_id}/content",
    tag = "files",
    params(
        ("file_id" = uuid::Uuid, Path, description = "The file."),
    ),
    responses(DownloadResponses)
)]
async fn download_own_file(
    State(files): State<Files>,
    principal: Principal,
    client: Client,
    params: RawPathParams,
    wanted: Wanted,
) -> Result<Download<ObjectBytes>, Problem<FilesError>> {
    let id = path_id(&params, "file_id").ok_or(FilesError::NotFound)?;
    let downloaded = service::download(&files, &client, &principal, id, &wanted).await?;
    Ok(response(downloaded))
}

/// Deletes a file of the caller's own.
#[utoipa::path(
    delete,
    path = "/v1/users/me/files/{file_id}",
    tag = "files",
    params(
        ("file_id" = uuid::Uuid, Path, description = "The file."),
    ),
    responses(
        (status = 204, description = "The file was deleted."),
    )
)]
async fn delete_own_file(
    State(files): State<Files>,
    principal: Principal,
    client: Client,
    params: RawPathParams,
) -> Result<StatusCode, Problem<FilesError>> {
    let id = path_id(&params, "file_id").ok_or(FilesError::NotFound)?;
    service::delete(&files, &client, &principal, id).await?;
    Ok(StatusCode::NO_CONTENT)
}
