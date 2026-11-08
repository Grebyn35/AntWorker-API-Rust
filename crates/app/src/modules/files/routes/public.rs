//! The downloads of public files, which anyone may make without signing in (the files spec's
//! Routes, FILES-023).

use axum::extract::{RawPathParams, State};
use platform::{
    errors::Problem,
    http::{Download, DownloadRequest, Routes, path_id},
    storage::ObjectBytes,
    types::Client,
};
use utoipa_axum::routes;

use super::{DOWNLOAD_ERRORS, Downloaded, download};
use crate::modules::files::{
    error::FilesError,
    service::{
        Files,
        public::{self as service, Owner},
    },
};

/// The public downloads.
pub(super) fn routes() -> Routes<Files> {
    Routes::new()
        .public_download(routes!(download_public_workspace_file), &DOWNLOAD_ERRORS)
        .public_download(routes!(download_public_user_file), &DOWNLOAD_ERRORS)
}

/// Downloads a public file of a workspace, without signing in: its bytes, or a range of them,
/// without its name, served so that a browser can't run them, which shared caches may keep for 5
/// minutes.
#[utoipa::path(
    get,
    path = "/v1/public/workspace-files/{file_id}",
    tag = "files",
    params(
        ("file_id" = uuid::Uuid, Path, description = "The public file."),
    ),
    responses(Downloaded)
)]
async fn download_public_workspace_file(
    State(files): State<Files>,
    client: Client,
    params: RawPathParams,
    request: DownloadRequest,
) -> Result<Download<ObjectBytes>, Problem<FilesError>> {
    let id = path_id(&params, "file_id").ok_or(FilesError::NotFound)?;
    let file = service::download(&files, &client, Owner::Workspace, id).await?;
    Ok(download(&files, file, &request).await?)
}

/// Downloads a public file of a user's own, without signing in: its bytes, or a range of them,
/// without its name, served so that a browser can't run them, which shared caches may keep for 5
/// minutes.
#[utoipa::path(
    get,
    path = "/v1/public/user-files/{file_id}",
    tag = "files",
    params(
        ("file_id" = uuid::Uuid, Path, description = "The public file."),
    ),
    responses(Downloaded)
)]
async fn download_public_user_file(
    State(files): State<Files>,
    client: Client,
    params: RawPathParams,
    request: DownloadRequest,
) -> Result<Download<ObjectBytes>, Problem<FilesError>> {
    let id = path_id(&params, "file_id").ok_or(FilesError::NotFound)?;
    let file = service::download(&files, &client, Owner::User, id).await?;
    Ok(download(&files, file, &request).await?)
}
