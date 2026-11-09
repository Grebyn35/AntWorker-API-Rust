//! Requesting an export of the caller's data, reading their exports, and downloading an export's
//! archive (the accounts spec's Routes, D31).

use axum::extract::{RawPathParams, State};
use platform::{
    authn::Principal,
    errors::{ErrorCode, Problem},
    http::{Download, FieldErrors, FileBytes, Query, ResourceResponse, Routes, path_id},
    lists::{Cursor, Limit, List, Page},
    ranges::Wanted,
    rate_limit::RateLimits,
    types::Client,
};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::routes;

use crate::modules::accounts::{
    error::AccountsError,
    events::{DATA_EXPORT_FAILED, DATA_EXPORT_READY},
    model::Export,
    service::exports::{ArchiveBytes, Exports},
};

/// The routes of exports, which take the exports' state, and the events their job publishes.
pub(crate) fn export_routes() -> Routes<Exports> {
    Routes::new()
        .events(&[&DATA_EXPORT_READY, &DATA_EXPORT_FAILED])
        .user(routes!(request_export, list_exports), &[])
        .user(routes!(read_export), &[&AccountsError::ExportNotFound])
        .user_download(routes!(download_export), &DOWNLOAD_ERRORS)
}

/// What downloading an export can answer besides the pipeline's own (ERR-13).
const DOWNLOAD_ERRORS: [&dyn ErrorCode; 2] = [
    &AccountsError::ExportNotFound,
    &AccountsError::ExportNotReady,
];

/// Requests an export of the caller's data, whose archive a job builds.
#[utoipa::path(
    post,
    path = "/v1/users/me/exports",
    tag = "accounts",
    responses(
        (status = 202, description = "The export, pending until its archive is built.", body = Export,
            headers(("location" = String, description = "The export: `/v1/users/me/exports/{id}`."))),
    )
)]
async fn request_export(
    State(exports): State<Exports>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
) -> Result<ResourceResponse<Export>, Problem<AccountsError>> {
    let export = exports.request(&limits, &client, &principal).await?;
    let location = format!("/v1/users/me/exports/{}", export.id);
    Ok(ResourceResponse::accepted(export, location))
}

/// The query of the list of exports (ACCOUNTS-022).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    /// How many exports a page holds.
    #[param(value_type = Option<Limit>, inline)]
    limit: Option<String>,
    /// The `next_cursor` of the page before.
    #[param(value_type = Option<Cursor>, inline)]
    cursor: Option<String>,
}

/// Lists the caller's exports that haven't expired, the latest first.
#[utoipa::path(
    get,
    path = "/v1/users/me/exports",
    tag = "accounts",
    params(ListQuery),
    responses(
        (status = 200, description = "A page of the caller's exports.", body = List<Export>),
    )
)]
async fn list_exports(
    State(exports): State<Exports>,
    client: Client,
    principal: Principal,
    query: Query<ListQuery>,
) -> Result<axum::Json<List<Export>>, Problem<AccountsError>> {
    let params = query.params();
    let mut errors = FieldErrors::new();
    let page = Page::read(
        query.route(),
        params.limit.as_deref(),
        params.cursor.as_deref(),
        &mut errors,
    );
    let page = errors.finish_query(page).map_err(AccountsError::from)?;
    Ok(axum::Json(exports.list(&client, &principal, &page).await?))
}

/// Reads one of the caller's exports.
#[utoipa::path(
    get,
    path = "/v1/users/me/exports/{export_id}",
    tag = "accounts",
    params(
        ("export_id" = uuid::Uuid, Path, description = "The export."),
    ),
    responses(
        (status = 200, description = "The export.", body = Export),
    )
)]
async fn read_export(
    State(exports): State<Exports>,
    client: Client,
    principal: Principal,
    params: RawPathParams,
) -> Result<ResourceResponse<Export>, Problem<AccountsError>> {
    let id = path_id(&params, "export_id").ok_or(AccountsError::ExportNotFound)?;
    Ok(ResourceResponse::ok(
        exports.read(&client, &principal, id).await?,
    ))
}

/// Downloads a ready export's archive, or a range of it.
#[utoipa::path(
    get,
    path = "/v1/users/me/exports/{export_id}/content",
    tag = "accounts",
    params(
        ("export_id" = uuid::Uuid, Path, description = "The export."),
    ),
    responses(
        (status = 200, description = "The export's archive, a ZIP file of the caller's data (ACCOUNTS-024).",
            content_type = "application/zip", body = FileBytes,
            headers(
                ("content-disposition" = String, description = "`attachment`, with the archive's name, `<product>-export-<date>.zip`."),
                ("content-security-policy" = String, description = "`default-src 'none'; sandbox` (PLATFORM-081)."),
            )),
    )
)]
async fn download_export(
    State(exports): State<Exports>,
    client: Client,
    principal: Principal,
    params: RawPathParams,
    wanted: Wanted,
) -> Result<Download<ArchiveBytes>, Problem<AccountsError>> {
    let id = path_id(&params, "export_id").ok_or(AccountsError::ExportNotFound)?;
    let download = exports.download(&client, &principal, id, &wanted).await?;
    Ok(Download {
        media_type: "application/zip".to_owned(),
        size: download.size,
        served: download.served,
        inline: false,
        name: download.name,
        tag: download.tag,
        public_for: None,
        bytes: download.bytes,
    })
}
