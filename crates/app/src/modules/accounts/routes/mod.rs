//! The caller's own record, and renaming oneself (the accounts spec's Routes).
// part: files
//! Setting and removing an avatar are in `avatar.rs`.
// end part: files

// part: files
mod avatar;
// end part: files

use axum::extract::State;
use platform::{
    authn::Principal,
    db::Database,
    errors::{Problem, RequestError},
    http::{FieldErrors, IfMatch, MergePatch, ResourceResponse, Routes},
    types::Client,
};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::routes;

use super::{
    error::AccountsError,
    events::{MEMBER_UPDATED, USER_UPDATED},
    model::{DisplayName, Profile, Rename},
    service,
};

// part: files
pub(crate) use avatar::avatar_routes;
// end part: files

/// The module's routes, which take the database as their state, and the events they publish.
pub(crate) fn routes() -> Routes<Database> {
    Routes::new()
        .events(&[&USER_UPDATED, &MEMBER_UPDATED])
        .user(routes!(me, rename_me), &[])
}

/// Reads the caller's own record.
#[utoipa::path(
    get,
    path = "/v1/users/me",
    tag = "accounts",
    responses(
        (status = 200, description = "The caller's own record.", body = Profile,
            headers(("etag" = String, description = "The record's version, which a rename's `If-Match` names (API-13)."))),
    )
)]
async fn me(
    State(database): State<Database>,
    client: Client,
    principal: Principal,
) -> Result<ResourceResponse<Profile>, Problem<AccountsError>> {
    let current = service::me(&database, &client, &principal).await?;
    Ok(ResourceResponse::ok(current.profile).tagged(current.version))
}

/// What renaming oneself takes: a JSON Merge Patch of the caller's record (ACCOUNTS-007).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct UserRenameRequest {
    /// The caller's new name (ACCOUNTS-003).
    #[schema(value_type = DisplayName)]
    name: String,
}

impl TryFrom<UserRenameRequest> for Rename {
    type Error = RequestError;

    fn try_from(request: UserRenameRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let name = errors.check("/name", DisplayName::parse(&request.name));
        errors.finish(name.map(|name| Self { name }))
    }
}

/// Renames the caller.
#[utoipa::path(
    patch,
    path = "/v1/users/me",
    tag = "accounts",
    request_body(content = UserRenameRequest, content_type = "application/merge-patch+json"),
    params(
        ("If-Match" = Option<String>, Header,
            description = "The `ETag` of the version the client renames, so that a change made since it read the record fails rather than being overwritten (API-13)."),
    ),
    responses(
        (status = 200, description = "The caller's record, renamed.", body = Profile,
            headers(("etag" = String, description = "The record's new version (API-13)."))),
    )
)]
async fn rename_me(
    State(database): State<Database>,
    client: Client,
    principal: Principal,
    if_match: IfMatch,
    request: MergePatch<UserRenameRequest>,
) -> Result<ResourceResponse<Profile>, Problem<AccountsError>> {
    let rename = Rename::try_from(request.0).map_err(AccountsError::from)?;
    let current =
        service::rename(&database, &client, &principal, rename, if_match.versions()).await?;
    Ok(ResourceResponse::ok(current.profile).tagged(current.version))
}
