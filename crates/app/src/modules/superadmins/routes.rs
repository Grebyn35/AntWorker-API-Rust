//! The route that tells a caller whether they are a superadmin (the superadmins spec's Routes). A
//! superadmin's routes on the deployment's workspaces are `workspaces`'.

use axum::extract::State;
use platform::{authn::Principal, db::Database, errors::Problem, http::Routes, types::Client};
use utoipa_axum::routes;

use super::{error::SuperadminsError, model::SuperadminStanding, service};

/// The module's routes, which take the database as their state.
pub(crate) fn routes() -> Routes<Database> {
    Routes::new().user(routes!(standing), &[])
}

/// Tells whether the caller is one of the deployment's superadmins, so a client offers a
/// superadmin's pages to them alone.
#[utoipa::path(
    get,
    path = "/v1/users/me/superadmin",
    tag = "superadmins",
    responses(
        (status = 200, description = "Whether the caller is a superadmin.", body = SuperadminStanding),
    )
)]
async fn standing(
    State(database): State<Database>,
    client: Client,
    principal: Principal,
) -> Result<axum::Json<SuperadminStanding>, Problem<SuperadminsError>> {
    let standing = service::standing(&database, &client, &principal).await?;
    Ok(axum::Json(standing))
}
