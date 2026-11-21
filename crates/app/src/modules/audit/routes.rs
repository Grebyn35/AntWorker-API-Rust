//! Reading a workspace's log and one's own (the audit spec's Routes).

use axum::extract::State;
use platform::{
    authn::Principal,
    authz::Authorize,
    db::Database,
    errors::Problem,
    http::{FieldErrors, Query, Routes},
    lists::{self, Cursor, Limit, List, Moment, Page},
    types::{AnyId, Client, Timestamp, UserId, WorkspaceId},
};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::routes;

use super::{
    error::AuditError,
    model::{ActionName, EntryFilter, LoggedEntry, ResourceName},
    service,
};
use crate::modules::access::AuditRead;

/// The module's routes, which take the database.
pub(crate) fn routes() -> Routes<Database> {
    Routes::new()
        .workspace::<AuditRead>(routes!(workspace_entries), &[])
        .user(routes!(own_entries), &[])
}

platform::filter_values!(
    /// The actions that a filter of a log's list keeps (AUDIT-009).
    Actions(ActionName)
);
platform::filter_values!(
    /// The kinds of resource that a filter of a log's list keeps (AUDIT-009).
    ResourceTypes(ResourceName)
);
platform::filter_values!(
    /// The resources that a filter of a log's list keeps (AUDIT-009).
    ResourceIds(AnyId)
);
platform::filter_values!(
    /// The users, as actors or as targets, that a filter of a log's list keeps (AUDIT-009).
    UserIds(UserId)
);

/// The query of both lists of a log (AUDIT-009).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
pub(crate) struct EntriesQuery {
    /// How many entries a page holds.
    #[param(value_type = Option<Limit>, inline)]
    limit: Option<String>,
    /// The `next_cursor` of the page before.
    #[param(value_type = Option<Cursor>, inline)]
    cursor: Option<String>,
    /// Only the entries of one of these actions, such as `member.removed`, separated by commas.
    #[param(value_type = Option<Actions>, inline, style = Form, explode = false)]
    action: Option<String>,
    /// Only the entries that happened to one of these kinds of resource, such as `workspace`,
    /// separated by commas.
    #[param(value_type = Option<ResourceTypes>, inline, style = Form, explode = false)]
    resource_type: Option<String>,
    /// Only the entries that happened to one of these resources, separated by commas.
    #[param(value_type = Option<ResourceIds>, inline, style = Form, explode = false)]
    resource_id: Option<String>,
    /// Only the entries that one of these users did, separated by commas.
    #[param(value_type = Option<UserIds>, inline, style = Form, explode = false)]
    actor_user_id: Option<String>,
    /// Only the entries that were done to one of these users, separated by commas.
    #[param(value_type = Option<UserIds>, inline, style = Form, explode = false)]
    target_user_id: Option<String>,
    /// Only the entries recorded at this moment or after.
    #[param(value_type = Option<Moment>, inline)]
    from: Option<String>,
    /// Only the entries recorded before this moment.
    #[param(value_type = Option<Moment>, inline)]
    to: Option<String>,
}

/// The page and the filter that `query` names, or the 400 `request.invalid_query` that names
/// every parameter it refuses (PLATFORM-045, PLATFORM-088, AUDIT-009).
fn read(query: &Query<EntriesQuery>) -> Result<(Page, EntryFilter), AuditError> {
    let params = query.params();
    let mut errors = FieldErrors::new();
    let page = Page::read(
        query.route(),
        params.limit.as_deref(),
        params.cursor.as_deref(),
        &mut errors,
    );
    let user = |value: &str| {
        platform::types::read_id(value)
            .map(UserId::from_uuid)
            .ok_or_else(invalid_id)
    };
    let actions = lists::values(
        &mut errors,
        "action",
        params.action.as_deref(),
        ActionName::parse,
    );
    let resource_types = lists::values(
        &mut errors,
        "resource_type",
        params.resource_type.as_deref(),
        ResourceName::parse,
    );
    let resource_ids = lists::values(
        &mut errors,
        "resource_id",
        params.resource_id.as_deref(),
        |value| platform::types::read_id(value).ok_or_else(invalid_id),
    );
    let actors = lists::values(
        &mut errors,
        "actor_user_id",
        params.actor_user_id.as_deref(),
        user,
    );
    let targets = lists::values(
        &mut errors,
        "target_user_id",
        params.target_user_id.as_deref(),
        user,
    );
    let from = lists::parameter(&mut errors, "from", params.from.as_deref(), Timestamp::read);
    let to = lists::parameter(&mut errors, "to", params.to.as_deref(), Timestamp::read);
    // Each is `None` once refused, so the filter is whole only when none was.
    let filter = (|| {
        Some(EntryFilter {
            actions: actions?,
            resource_types: resource_types?,
            resource_ids: resource_ids?,
            actors: actors?,
            targets: targets?,
            from: from?,
            to: to?,
        })
    })();
    Ok(errors.finish_query(page.zip(filter))?)
}

/// The refusal of a filter's value that isn't a UUID (AUDIT-009).
fn invalid_id() -> platform::types::Invalid {
    platform::types::Invalid::new("invalid", "An ID is a UUID.")
}

/// Lists the entries of the workspace's log that the caller's role may read, the latest first.
#[utoipa::path(
    get,
    path = "/v1/workspaces/{workspace_id}/audit-entries",
    tag = "audit",
    params(
        ("workspace_id" = WorkspaceId, Path, description = "The workspace."),
        EntriesQuery,
    ),
    responses(
        (status = 200, description = "A page of the workspace's log.", body = List<LoggedEntry>),
    )
)]
async fn workspace_entries(
    authorize: Authorize<AuditRead>,
    query: Query<EntriesQuery>,
) -> Result<axum::Json<List<LoggedEntry>>, Problem<AuditError>> {
    let (page, filter) = read(&query)?;
    let (mut uow, authorized) = authorize.open().await.map_err(AuditError::from)?;
    let list = service::workspace_entries(&mut uow, &authorized, &filter, &page).await?;
    Ok(axum::Json(list))
}

/// Lists the entries of the caller's own log, and those of a workspace's log whose target is
/// them, the latest first.
#[utoipa::path(
    get,
    path = "/v1/users/me/audit-entries",
    tag = "audit",
    params(EntriesQuery),
    responses(
        (status = 200, description = "A page of the caller's entries.", body = List<LoggedEntry>),
    )
)]
async fn own_entries(
    State(database): State<Database>,
    client: Client,
    principal: Principal,
    query: Query<EntriesQuery>,
) -> Result<axum::Json<List<LoggedEntry>>, Problem<AuditError>> {
    let (page, filter) = read(&query)?;
    let list = service::own_entries(&database, &client, &principal, &filter, &page).await?;
    Ok(axum::Json(list))
}
