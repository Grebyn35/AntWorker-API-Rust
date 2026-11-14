//! A user creating a workspace, which they then own (the workspaces spec's "Users creating
//! workspaces").

use axum::extract::State;
use platform::{
    authn::Principal,
    db::Database,
    errors::{Problem, RequestError},
    http::{FieldErrors, Idempotent, ResourceResponse, Routes},
    idempotency::IdempotencyKey,
    types::Client,
};
use serde::Deserialize;
use utoipa::ToSchema;
use utoipa_axum::routes;

use crate::modules::workspaces::{
    error::WorkspacesError,
    model::{NewWorkspace, Workspace, WorkspaceName},
    service::creation as service,
};

/// The route that creates a workspace, which takes the database as its state. It publishes the
/// event that [`super::routes`] declares.
pub(super) fn routes() -> Routes<Database> {
    Routes::new().user(routes!(create), &[])
}

/// What creating a workspace takes (WORKSPACES-001).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateRequest {
    /// The workspace's name (WORKSPACES-002).
    #[schema(value_type = WorkspaceName)]
    name: String,
}

impl TryFrom<CreateRequest> for NewWorkspace {
    type Error = RequestError;

    fn try_from(request: CreateRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let name = errors.check("/name", WorkspaceName::parse(&request.name));
        errors.finish(name.map(|name| Self { name }))
    }
}

/// Creates a workspace that the caller owns.
#[utoipa::path(
    post,
    path = "/v1/workspaces",
    tag = "workspaces",
    request_body = CreateRequest,
    params(
        ("Idempotency-Key" = inline(Option<IdempotencyKey>), Header,
            description = "A key of the caller's own, as a quoted string, so a retry creates nothing twice (API-12)."),
    ),
    responses(
        (status = 201, description = "The new workspace, which the caller owns.", body = Workspace,
            headers(
                ("location" = String, description = "The new workspace: `/v1/workspaces/{id}`."),
                ("etag" = String, description = "The workspace's version, which a rename's `If-Match` names (API-13)."),
            )),
    )
)]
async fn create(
    State(database): State<Database>,
    client: Client,
    principal: Principal,
    request: Idempotent<CreateRequest>,
) -> Result<ResourceResponse<Workspace>, Problem<WorkspacesError>> {
    let input = NewWorkspace::try_from(request.body).map_err(WorkspacesError::from)?;
    let created =
        service::create(&database, &client, &principal, request.key.as_ref(), input).await?;
    let location = format!("/v1/workspaces/{}", created.workspace.id);
    Ok(ResourceResponse::created(created.workspace, location).tagged(created.version))
}
