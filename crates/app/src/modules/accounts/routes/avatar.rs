//! Setting and removing the caller's avatar, whose image `files` keeps (the accounts spec's
//! Routes, D120).

use axum::{extract::State, http::StatusCode};
use platform::{
    authn::Principal,
    errors::{ErrorCode, Problem},
    http::{FileBytes, ResourceResponse, Routes, Upload},
    types::Client,
};
use utoipa_axum::routes;

use crate::modules::{
    accounts::{
        error::AccountsError,
        model::Profile,
        service::avatar::{self as service, Avatars},
    },
    files::AVATAR_MAX_BYTES,
};

/// The avatar's routes, which take the database and `files` as their state. They publish the
/// events that [`super::routes`] declares.
pub(crate) fn avatar_routes() -> Routes<Avatars> {
    Routes::new()
        .user_upload(routes!(set_avatar), &SET_ERRORS)
        .user(routes!(remove_avatar), &[])
}

/// What setting an avatar can answer besides the pipeline's own (ERR-13).
const SET_ERRORS: [&dyn ErrorCode; 3] = [
    &AccountsError::AvatarNotImage,
    &AccountsError::AvatarMalwareDetected,
    &AccountsError::AvatarUnavailable(None),
];

/// Sets the caller's avatar: the image's bytes as the body, in place of any avatar they had.
#[utoipa::path(
    put,
    path = "/v1/users/me/avatar",
    tag = "accounts",
    request_body(content = FileBytes, content_type = "application/octet-stream",
        description = "The image's bytes: a PNG, JPEG, GIF or WebP image of at most 1 MiB, whose kind comes from its bytes alone (ACCOUNTS-009)."),
    responses(
        (status = 200, description = "The caller's record, with its new avatar.", body = Profile,
            headers(("etag" = String, description = "The record's new version (API-13)."))),
    )
)]
async fn set_avatar(
    State(avatars): State<Avatars>,
    principal: Principal,
    client: Client,
    upload: Upload,
) -> Result<ResourceResponse<Profile>, Problem<AccountsError>> {
    // The turn the bytes hold lasts until `uploaded` is dropped, once the avatar is stored
    // (PLATFORM-074).
    let uploaded = upload
        .read(AVATAR_MAX_BYTES, &avatars.workers)
        .await
        .map_err(AccountsError::from)?;
    let bytes = uploaded.bytes.clone();
    let current = service::set(&avatars, &client, &principal, bytes).await?;
    drop(uploaded);
    Ok(ResourceResponse::ok(current.profile).tagged(current.version))
}

/// Removes the caller's avatar, if they have one.
#[utoipa::path(
    delete,
    path = "/v1/users/me/avatar",
    tag = "accounts",
    responses(
        (status = 204, description = "The caller has no avatar."),
    )
)]
async fn remove_avatar(
    State(avatars): State<Avatars>,
    client: Client,
    principal: Principal,
) -> Result<StatusCode, Problem<AccountsError>> {
    service::remove(&avatars, &client, &principal).await?;
    Ok(StatusCode::NO_CONTENT)
}
