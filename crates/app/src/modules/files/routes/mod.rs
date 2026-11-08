//! The routes of `files` (the files spec's Routes): a workspace's files, in `workspace.rs`, a
//! user's own, in `user.rs`, and the public downloads, in `public.rs`, with what they share: the
//! query of an upload and of a list, and the documented responses of a download.

mod public;
mod user;
mod workspace;

use std::{collections::BTreeMap, time::Duration};

use axum::http::HeaderMap;
use platform::{
    errors::ErrorCode,
    http::{
        Download, DownloadRequest, FieldErrors, FileBytes, Query, Routes, Served, Upload,
        disposition_name,
    },
    lists::{Cursor, Limit, Page, Search, parameter},
    storage::ObjectBytes,
};
use serde::Deserialize;
use utoipa::{
    IntoParams, PartialSchema,
    openapi::{
        RefOr, Response,
        content::Content,
        header::HeaderBuilder,
        response::ResponseBuilder,
        schema::{ObjectBuilder, Type},
    },
};

use super::{
    error::FilesError,
    events::{FILE_DELETED, FILE_UPLOADED, USER_FILE_DELETED, USER_FILE_UPLOADED},
    model::{FileName, Kind, Visibility},
    service::{self, Files, Serving},
};

/// The module's routes, and the events they publish.
pub(crate) fn routes() -> Routes<Files> {
    Routes::new()
        .events(&[
            &FILE_UPLOADED,
            &FILE_DELETED,
            &USER_FILE_UPLOADED,
            &USER_FILE_DELETED,
        ])
        .merge(workspace::routes())
        .merge(user::routes())
        .merge(public::routes())
}

/// What an upload can answer besides the pipeline's own (ERR-13).
const UPLOAD_ERRORS: [&dyn ErrorCode; 6] = [
    &FilesError::InvalidName(None),
    &FilesError::Empty,
    &FilesError::UnsupportedType,
    &FilesError::MalwareDetected,
    &FilesError::ScannerUnavailable(None),
    &FilesError::StorageUnavailable(None),
];

/// What a download can answer besides the pipeline's own (ERR-13).
const DOWNLOAD_ERRORS: [&dyn ErrorCode; 2] =
    [&FilesError::NotFound, &FilesError::StorageUnavailable(None)];

/// The query of an upload (FILES-020).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub(super) struct UploadQuery {
    /// Who may download the file: `private`, those who may read it, unless it is `public`,
    /// anyone, without signing in (FILES-020).
    #[param(value_type = Option<Visibility>, inline)]
    visibility: Option<String>,
}

/// What an upload's request gives before its body is read: its visibility, from the query, and
/// its name, from `Content-Disposition`, once `upload` has refused a body that declares more than
/// `limit` (FILES-002, FILES-004, FILES-020). A workspace's upload authorizes its caller between
/// the two, so it takes them one at a time.
pub(super) fn visibility(
    upload: &Upload,
    limit: usize,
    query: &Query<UploadQuery>,
) -> Result<Visibility, FilesError> {
    upload.refuse_declared(limit).map_err(FilesError::from)?;
    let mut errors = FieldErrors::new();
    let visibility = parameter(
        &mut errors,
        "visibility",
        query.params().visibility.as_deref(),
        Visibility::parse,
    );
    let visibility = errors.finish_query(visibility).map_err(FilesError::from)?;
    Ok(visibility.unwrap_or_default())
}

/// The upload's name, from `Content-Disposition` (FILES-002).
pub(super) fn name(headers: &HeaderMap) -> Result<FileName, FilesError> {
    let name = disposition_name(headers).ok_or(FilesError::InvalidName(None))?;
    FileName::parse(&name).map_err(|invalid| FilesError::InvalidName(Some(invalid)))
}

/// The query of a list of files (FILES-007, FILES-017).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub(super) struct ListQuery {
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

/// A list's page and search, from its query (API-10).
pub(super) fn page(query: &Query<ListQuery>) -> Result<(Page, Option<Search>), FilesError> {
    let params = query.params();
    let mut errors = FieldErrors::new();
    let page = Page::read(
        query.route(),
        params.limit.as_deref(),
        params.cursor.as_deref(),
        &mut errors,
    );
    let search = Search::read(params.search.as_deref(), &mut errors);
    errors
        .finish_query(page.zip(search))
        .map_err(FilesError::from)
}

/// How long a shared cache may keep a public file's download (FILES-023).
const PUBLIC_FOR: Duration = Duration::from_secs(300);

/// What a download serves of `file`, as `request` asks for it: nothing when the client holds its
/// bytes, and otherwise its bytes, or the range of them that the request names, as the bucket
/// streams them. A file without a name is a public one's, which shared caches may keep
/// (FILES-009, FILES-023, PLATFORM-081).
pub(super) async fn download(
    files: &Files,
    file: Serving,
    request: &DownloadRequest,
) -> Result<Download<ObjectBytes>, FilesError> {
    let tag = file.id.to_string();
    let served = request.serve(file.size_bytes, &tag);
    let bytes = if served == Served::NotModified {
        None
    } else {
        Some(service::bytes(files, &file, served.offsets()).await?)
    };
    Ok(Download {
        inline: Kind::of_media_type(&file.media_type).is_some_and(Kind::inline),
        public_for: file.name.is_none().then_some(PUBLIC_FOR),
        media_type: file.media_type,
        size: file.size_bytes,
        served,
        name: file.name,
        tag,
        bytes,
    })
}

/// The 200 of every download: the file's bytes, as the media type of one of the accepted kinds,
/// which the table of kinds lists (FILES-009, FILES-023). Each download's builder adds what a
/// range and a revalidation answer (PLATFORM-081).
pub(super) struct Downloaded;

impl utoipa::IntoResponses for Downloaded {
    fn responses() -> BTreeMap<String, RefOr<Response>> {
        let mut response =
            ResponseBuilder::new().description("The file's bytes, as its media type.");
        let mut documented = Vec::new();
        for kind in Kind::every() {
            // A kind's stored type with its parameters, such as `text/plain; charset=utf-8`, is
            // documented as its essence, `text/plain`.
            let essence = kind.media_type().split(';').next().unwrap_or_default();
            if !documented.contains(&essence) {
                documented.push(essence);
                response = response.content(essence, Content::new(Some(FileBytes::schema())));
            }
        }
        let text = || ObjectBuilder::new().schema_type(Type::String);
        let response = response
            .header(
                "content-disposition",
                HeaderBuilder::new().schema(Some(text())).description(Some(
                    "`inline` for an image and `attachment` for every other kind, with the file's name, but on a public download (FILES-009, FILES-023).",
                )).build(),
            )
            .header(
                "content-security-policy",
                HeaderBuilder::new().schema(Some(text())).description(Some(
                    "`default-src 'none'; sandbox`, so the file runs nothing (SEC-11).",
                )).build(),
            )
            .build();
        BTreeMap::from([("200".to_owned(), RefOr::T(response))])
    }
}

#[cfg(test)]
mod tests {
    use platform::telemetry::Service;

    use super::*;

    #[test]
    fn files_009_every_download_documents_the_media_type_of_every_kind() {
        let document = routes().openapi(Service {
            name: "api",
            version: "0.0.0",
        });
        let document = serde_json::to_value(&document).unwrap();
        let mut stored: Vec<&str> = Kind::every()
            .iter()
            .map(|kind| kind.media_type().split(';').next().unwrap_or_default())
            .collect();
        stored.sort_unstable();
        for path in [
            "/v1/workspaces/{workspace_id}/files/{file_id}/content",
            "/v1/users/me/files/{file_id}/content",
            "/v1/public/workspace-files/{file_id}",
            "/v1/public/user-files/{file_id}",
        ] {
            for status in ["200", "206"] {
                let content = &document["paths"][path]["get"]["responses"][status]["content"];
                assert!(content.is_object(), "{path} documents no {status}");
                let mut documented: Vec<&str> = content
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                documented.sort_unstable();
                assert_eq!(documented, stored, "{path} {status}");
            }
        }
    }
}
