//! The caller's own record (ACCOUNTS-001).

use axum::{Json, extract::State};
use platform::{authn::Principal, db::Database, errors::Problem, http::Routes, types::Client};
use utoipa_axum::routes;

use super::{error::AccountsError, model::Profile, service};

/// The module's routes, which take the database as their state.
pub(crate) fn routes() -> Routes<Database> {
    Routes::new().user(routes!(me), &[])
}

/// Reads the caller's own record.
#[utoipa::path(
    get,
    path = "/v1/users/me",
    tag = "accounts",
    responses(
        (status = 200, description = "The caller's own record.", body = Profile),
    )
)]
async fn me(
    State(database): State<Database>,
    client: Client,
    principal: Principal,
) -> Result<Json<Profile>, Problem<AccountsError>> {
    Ok(Json(service::me(&database, &client, &principal).await?))
}
