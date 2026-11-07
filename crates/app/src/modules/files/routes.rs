//! Creating, listing, reading, renaming and deleting a workspace's files (the files
//! spec's Routes).

use axum::{extract::RawPathParams, http::StatusCode};
use platform::{
    authz::Authorize,
    errors::{Problem, RequestError},
    http::{
        FieldErrors, Idempotent, IfMatch, MergePatch, Query, ResourceResponse, Routes, path_id,
    },
    idempotency::IdempotencyKey,
    lists::{Cursor, Limit, List, Page, Search},
    types::WorkspaceId,
};
use serde::Deserialize;
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::routes;

use super::{
    error::FilesError,
    events::{FILE_CREATED, FILE_DELETED, FILE_RENAMED},
    model::{File, FileName, NewFile, Rename},
    service,
};
use crate::modules::access::{FilesCreate, FilesDelete, FilesRead, FilesUpdate};

/// The module's routes, and the events they publish.
pub(crate) fn routes() -> Routes {
    Routes::new()
        .events(&[&FILE_CREATED, &FILE_RENAMED, &FILE_DELETED])
        .workspace::<FilesCreate>(routes!(create_file), &[])
        .workspace::<FilesRead>(routes!(list_files), &[])
        .workspace::<FilesRead>(routes!(read_file), &[&FilesError::NotFound])
        .workspace::<FilesUpdate>(routes!(rename_file), &[&FilesError::NotFound])
        .workspace::<FilesDelete>(routes!(delete_file), &[&FilesError::NotFound])
}

/// What creating a file takes (FILES-001).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateFileRequest {
    /// The file's name (FILES-002).
    #[schema(value_type = FileName)]
    name: String,
}

impl TryFrom<CreateFileRequest> for NewFile {
    type Error = RequestError;

    fn try_from(request: CreateFileRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let name = errors.check("/name", FileName::parse(&request.name));
        errors.finish(name.map(|name| Self { name }))
    }
}

/// Creates a file in the workspace.
#[utoipa::path(
    post,
    path = "/v1/workspaces/{workspace_id}/files",
    tag = "files",
    request_body = CreateFileRequest,
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("Idempotency-Key" = inline(Option<IdempotencyKey>), Header,
            description = "A key of the caller's own, as a quoted string, so a retry creates nothing twice (API-12)."),
    ),
    responses(
        (status = 201, description = "The new file.", body = File,
            headers(
                ("location" = String, description = "The new file: `/v1/workspaces/{workspace_id}/files/{id}`."),
                ("etag" = String, description = "The file's version, which a rename's `If-Match` names (API-13)."),
            )),
    )
)]
async fn create_file(
    authorize: Authorize<FilesCreate>,
    request: Idempotent<CreateFileRequest>,
) -> Result<ResourceResponse<File>, Problem<FilesError>> {
    let input = NewFile::try_from(request.body).map_err(FilesError::from)?;
    let (uow, authorized) = authorize.open().await.map_err(FilesError::from)?;
    let created = service::create(uow, &authorized, request.key.as_ref(), input).await?;
    let location = format!(
        "/v1/workspaces/{}/files/{}",
        authorized.workspace_id(),
        created.file.id
    );
    Ok(ResourceResponse::created(created.file, location).tagged(created.version))
}

/// The query of `GET /v1/workspaces/{workspace_id}/files` (FILES-003).
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

/// Lists the workspace's files, the latest created first.
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
        (status = 200, description = "The file.", body = File,
            headers(("etag" = String, description = "The file's version, which a rename's `If-Match` names (API-13)."))),
    )
)]
async fn read_file(
    authorize: Authorize<FilesRead>,
    params: RawPathParams,
) -> Result<ResourceResponse<File>, Problem<FilesError>> {
    let id = path_id(&params, "file_id");
    let (mut uow, authorized) = authorize.open().await.map_err(FilesError::from)?;
    let id = id.ok_or(FilesError::NotFound)?;
    let current = service::read(&mut uow, &authorized, id).await?;
    Ok(ResourceResponse::ok(current.file).tagged(current.version))
}

/// What renaming a file takes: a JSON Merge Patch of it (FILES-005).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RenameFileRequest {
    /// The file's new name (FILES-002).
    #[schema(value_type = FileName)]
    name: String,
}

impl TryFrom<RenameFileRequest> for Rename {
    type Error = RequestError;

    fn try_from(request: RenameFileRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let name = errors.check("/name", FileName::parse(&request.name));
        errors.finish(name.map(|name| Self { name }))
    }
}

/// Renames a file of the workspace.
#[utoipa::path(
    patch,
    path = "/v1/workspaces/{workspace_id}/files/{file_id}",
    tag = "files",
    request_body(content = RenameFileRequest, content_type = "application/merge-patch+json"),
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        ("file_id" = uuid::Uuid, Path, description = "The file."),
        ("If-Match" = Option<String>, Header,
            description = "The `ETag` of the version the client renames, so that a rename made since it read the file fails rather than being overwritten (API-13)."),
    ),
    responses(
        (status = 200, description = "The file, renamed.", body = File,
            headers(("etag" = String, description = "The file's new version (API-13)."))),
    )
)]
async fn rename_file(
    authorize: Authorize<FilesUpdate>,
    params: RawPathParams,
    if_match: IfMatch,
    request: MergePatch<RenameFileRequest>,
) -> Result<ResourceResponse<File>, Problem<FilesError>> {
    let rename = Rename::try_from(request.0).map_err(FilesError::from)?;
    let id = path_id(&params, "file_id");
    let (uow, authorized) = authorize.open().await.map_err(FilesError::from)?;
    let id = id.ok_or(FilesError::NotFound)?;
    let current = service::rename(uow, &authorized, id, rename, if_match.versions()).await?;
    Ok(ResourceResponse::ok(current.file).tagged(current.version))
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
