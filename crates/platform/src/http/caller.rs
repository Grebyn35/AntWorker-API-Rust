//! The pipeline's check of a caller that is gone: a request that a foreign key failed answers 401
//! once the session that proved its caller lasts no more (PLATFORM-097).

use axum::{
    extract::Request,
    middleware::Next,
    response::{IntoResponse, Response},
};

use crate::{
    authn::{Authenticator, Principal, Proved},
    authz::Authorizer,
    errors::{self, ErrorReport, Internal, Problem, RequestError},
};

/// Answers 401 `request.unauthenticated` in place of an internal error that a foreign key
/// caused, once the session that proved the request's caller lasts no more: a deletion of their
/// account committed while the request ran, and took the row that an insert of the request's
/// names, so the request answers as one sent after the deletion does (PLATFORM-097). Any other
/// internal error stays, and so does this one while the session lasts, or when the check of it
/// fails, which is written and counted (ERR-11).
pub(super) async fn caller_gone(mut request: Request, next: Next) -> Response {
    let proved = Proved::default();
    request.extensions_mut().insert(proved.clone());
    let authenticator = request.extensions().get::<Authenticator>().cloned();
    let authorizer = request.extensions().get::<Authorizer>().cloned();
    let response = next.run(request).await;
    let foreign_key = response
        .extensions()
        .get::<ErrorReport>()
        .is_some_and(|report| report.foreign_key);
    let (true, Some(caller), Some(authenticator), Some(authorizer)) =
        (foreign_key, proved.caller(), authenticator, authorizer)
    else {
        return response;
    };
    match session_lasts(&authenticator, &authorizer, &caller).await {
        Ok(true) => response,
        Ok(false) => Problem(RequestError::Unauthenticated).into_response(),
        Err(error) => {
            errors::ignore(&error, "session_recheck_failed");
            response
        }
    }
}

/// Whether the session that proved `caller` still lasts, read in a unit of work of its own
/// that writes nothing.
async fn session_lasts(
    authenticator: &Authenticator,
    authorizer: &Authorizer,
    caller: &Principal,
) -> Result<bool, Internal> {
    let mut uow = authorizer.database.unattributed().await?;
    let lasting = authenticator
        .verifier
        .lasting(&mut uow, std::slice::from_ref(caller))
        .await?;
    Ok(lasting.contains(&caller.session_id()))
}
