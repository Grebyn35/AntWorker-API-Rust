//! Reading the current session, signing out, listing the caller's sessions, and ending one of
//! them or every other (AUTH-013, AUTH-016, AUTH-043 to AUTH-045).

use std::sync::Arc;

use axum::{
    extract::{RawPathParams, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use platform::{
    authn::Principal,
    errors::{Problem, RequestError},
    http::{FieldErrors, Query, Routes, path_id},
    lists::{Cursor, Limit, List, Page, Search},
    types::{Client, SessionId},
};
use serde::Deserialize;
use utoipa::IntoParams;
use utoipa_axum::routes;

use super::signed_out;
use crate::modules::auth::{
    error::AuthError,
    model::{Session, SessionItem},
    service::{Auth, Ended},
};

/// The routes of the caller's sessions, which take the service as their state.
pub(super) fn routes() -> Routes<Arc<Auth>> {
    Routes::new()
        .verifying(routes!(current, sign_out), &[])
        .user(routes!(list_sessions), &[])
        .user(
            routes!(end_session),
            &[
                &RequestError::ReauthenticationRequired,
                &AuthError::SessionNotFound,
            ],
        )
        .user(
            routes!(end_other_sessions),
            &[&RequestError::ReauthenticationRequired],
        )
}

/// Reads the session that authenticated the request.
#[utoipa::path(
    get,
    path = "/v1/users/me/sessions/current",
    tag = "auth",
    responses(
        (status = 200, description = "The session, without its tokens.", body = Session),
    )
)]
async fn current(
    State(auth): State<Arc<Auth>>,
    client: Client,
    principal: Principal,
) -> Result<axum::Json<Session>, Problem<AuthError>> {
    Ok(axum::Json(auth.current(&client, &principal).await?))
}

/// Signs out: ends the session that authenticated the request.
#[utoipa::path(
    delete,
    path = "/v1/users/me/sessions/current",
    tag = "auth",
    responses(
        (status = 204, description = "The session ended."),
    )
)]
async fn sign_out(
    State(auth): State<Arc<Auth>>,
    client: Client,
    principal: Principal,
) -> Result<Response, Problem<AuthError>> {
    auth.sign_out(&client, &principal).await?;
    Ok(signed_out()?)
}

/// The query of the list of sessions (AUTH-043).
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(deny_unknown_fields)]
struct ListQuery {
    /// How many sessions a page holds.
    #[param(value_type = Option<Limit>, inline)]
    limit: Option<String>,
    /// The `next_cursor` of the page before.
    #[param(value_type = Option<Cursor>, inline)]
    cursor: Option<String>,
    /// Keeps the sessions whose device's name contains it, ignoring case.
    #[param(value_type = Option<Search>, inline)]
    search: Option<String>,
}

/// Lists the caller's sessions that last, the latest started first.
#[utoipa::path(
    get,
    path = "/v1/users/me/sessions",
    tag = "auth",
    params(ListQuery),
    responses(
        (status = 200, description = "A page of the caller's sessions.", body = List<SessionItem>),
    )
)]
async fn list_sessions(
    State(auth): State<Arc<Auth>>,
    client: Client,
    principal: Principal,
    query: Query<ListQuery>,
) -> Result<axum::Json<List<SessionItem>>, Problem<AuthError>> {
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
        .map_err(AuthError::from)?;
    let list = auth
        .sessions(&client, &principal, search.as_ref(), &page)
        .await?;
    Ok(axum::Json(list))
}

/// Ends one of the caller's sessions: their own, which signs them out, or, once the session that
/// authenticated the request has proved who they are within the last 5 minutes, another.
#[utoipa::path(
    delete,
    path = "/v1/users/me/sessions/{session_id}",
    tag = "auth",
    params(
        ("session_id" = uuid::Uuid, Path, description = "The session."),
    ),
    responses(
        (status = 204, description = "The session ended."),
    )
)]
async fn end_session(
    State(auth): State<Arc<Auth>>,
    client: Client,
    principal: Principal,
    params: RawPathParams,
) -> Result<Response, Problem<AuthError>> {
    let id = path_id(&params, "session_id").map(SessionId::from_uuid);
    match auth.end_session(&client, &principal, id).await? {
        Ended::Own => Ok(signed_out()?),
        Ended::Other => Ok(StatusCode::NO_CONTENT.into_response()),
    }
}

/// Ends every session of the caller's but the one that authenticated the request, once it has
/// proved who they are within the last 5 minutes.
#[utoipa::path(
    delete,
    path = "/v1/users/me/sessions/others",
    tag = "auth",
    responses(
        (status = 204, description = "Every other session ended."),
    )
)]
async fn end_other_sessions(
    State(auth): State<Arc<Auth>>,
    client: Client,
    principal: Principal,
) -> Result<StatusCode, Problem<AuthError>> {
    auth.end_other_sessions(&client, &principal).await?;
    Ok(StatusCode::NO_CONTENT)
}
