//! Setting and removing a workspace's picture, whose image `files` keeps (the workspaces spec's
//! Routes, D149).

use axum::{extract::State, http::StatusCode};
use platform::{
    authz::Authorize,
    errors::{ErrorCode, Problem},
    http::{FileBytes, IfMatch, ResourceResponse, Routes, Upload},
    types::WorkspaceId,
};
use utoipa_axum::routes;

use crate::modules::{
    access::WorkspaceUpdate,
    files::PICTURE_MAX_BYTES,
    workspaces::{
        error::WorkspacesError,
        model::Workspace,
        service::picture::{self as service, Pictures},
    },
};

/// The picture's routes, which take `files` as their state. They publish the event that
/// [`super::routes`] declares.
pub(crate) fn picture_routes() -> Routes<Pictures> {
    Routes::new()
        .workspace_upload::<WorkspaceUpdate>(routes!(set_picture), &SET_ERRORS)
        .workspace::<WorkspaceUpdate>(routes!(remove_picture), &[])
}

/// What setting a picture can answer besides the pipeline's own (ERR-13).
const SET_ERRORS: [&dyn ErrorCode; 3] = [
    &WorkspacesError::PictureNotImage,
    &WorkspacesError::PictureMalwareDetected,
    &WorkspacesError::PictureUnavailable(None),
];

/// Sets the workspace's picture: the image's bytes as the body, in place of any picture it had.
#[utoipa::path(
    put,
    path = "/v1/workspaces/{workspace_id}/picture",
    tag = "workspaces",
    request_body(content = FileBytes, content_type = "application/octet-stream",
        description = "The image's bytes: a PNG, JPEG, GIF or WebP image of at most 1 MiB, whose kind comes from its bytes alone (WORKSPACES-020)."),
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("If-Match" = Option<String>, Header,
            description = "The `ETag` of the version of the workspace that the client changes, so that a change made since it read the workspace fails rather than being overwritten (API-13)."),
    ),
    responses(
        (status = 200, description = "The workspace, with its new picture.", body = Workspace,
            headers(("etag" = String, description = "The workspace's new version (API-13)."))),
    )
)]
async fn set_picture(
    State(pictures): State<Pictures>,
    authorize: Authorize<WorkspaceUpdate>,
    if_match: IfMatch,
    upload: Upload,
) -> Result<ResourceResponse<Workspace>, Problem<WorkspacesError>> {
    upload
        .refuse_declared(PICTURE_MAX_BYTES)
        .map_err(WorkspacesError::from)?;
    authorize.check().await.map_err(WorkspacesError::from)?;
    // The turn the bytes hold lasts until `uploaded` is dropped, once the picture is stored
    // (PLATFORM-074).
    let uploaded = upload
        .read(PICTURE_MAX_BYTES, &pictures.workers)
        .await
        .map_err(WorkspacesError::from)?;
    let bytes = uploaded.bytes.clone();
    let current = service::set(&pictures, authorize, bytes, if_match.versions()).await?;
    drop(uploaded);
    Ok(ResourceResponse::ok(current.workspace).tagged(current.version))
}

/// Removes the workspace's picture, if it has one.
#[utoipa::path(
    delete,
    path = "/v1/workspaces/{workspace_id}/picture",
    tag = "workspaces",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("If-Match" = Option<String>, Header,
            description = "The `ETag` of the version of the workspace that the client changes, so that a change made since it read the workspace fails rather than being overwritten (API-13)."),
    ),
    responses(
        (status = 204, description = "The workspace has no picture."),
    )
)]
async fn remove_picture(
    authorize: Authorize<WorkspaceUpdate>,
    if_match: IfMatch,
) -> Result<StatusCode, Problem<WorkspacesError>> {
    let (uow, authorized) = authorize.open().await.map_err(WorkspacesError::from)?;
    service::remove(uow, &authorized, if_match.versions()).await?;
    Ok(StatusCode::NO_CONTENT)
}
