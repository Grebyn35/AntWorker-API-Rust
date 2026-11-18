//! How a session's tokens reach its client, in the body or as a browser's cookies, and how a
//! request shows the session that its browser holds (the auth spec's "Token delivery", AUTH-015,
//! AUTH-096).

use axum::{
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use platform::{
    authn::ACCESS_COOKIE,
    http::{Cookies, expired_cookie, session_cookie},
    types::{Secret, SessionId, Timestamp, UserId},
};
use serde::Serialize;
use utoipa::ToSchema;

use crate::modules::auth::{
    error::AuthError,
    model::{DeviceId, DeviceName, DeviceToken, Session, TokenDelivery, Tokens},
    service::Held,
};

/// The name of the cookie that holds a browser's refresh token, after `__Host-` (AUTH-015).
pub(super) const REFRESH_COOKIE: &str = "refresh_token";

/// Where a new session is: the session that the request's own tokens prove (AUTH-016).
const CURRENT_SESSION: &str = "/v1/users/me/sessions/current";

/// A session, and its tokens when they go in the body.
#[derive(Serialize, ToSchema)]
pub(crate) struct SessionResponse {
    /// The session's ID.
    id: SessionId,
    /// The user the session signs in.
    user_id: UserId,
    /// The device ID the client signed in with, if any.
    device_id: Option<DeviceId>,
    /// The name the client gave its device, if any (AUTH-042).
    device_name: Option<DeviceName>,
    /// The provider whose sign-in started the session, by its key, or null for a password
    /// (AUTH-073).
    provider: Option<String>,
    /// When the session began.
    created_at: Timestamp,
    /// When its access token expires.
    access_token_expires_at: Timestamp,
    /// When its refresh token expires, unless a refresh slides it.
    refresh_token_expires_at: Timestamp,
    /// When the session ends, whatever its refreshes.
    expires_at: Timestamp,
    /// When its user last proved who they are in it, their password or their account at a provider, and the second factor of an
    /// account that has one: when it began, re-authenticated, or changed the password (AUTH-021,
    /// AUTH-040, AUTH-065).
    authenticated_at: Timestamp,
    /// Whether its account must prove its email address, with the code sent to it, before the
    /// API serves it anything but proving it (AUTH-025, AUTH-026).
    email_verification_required: bool,
    /// Whether its account must set up an authenticator app before the API serves it anything
    /// but that and proving its address: true while the deployment requires a second factor and
    /// the account has none (AUTH-070).
    second_factor_setup_required: bool,
    /// The access token, unless the tokens went in cookies.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    access_token: Option<String>,
    /// The refresh token, unless the tokens went in cookies.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    refresh_token: Option<String>,
    /// The device token of a registration or a sign-in, unless the tokens went in cookies, which
    /// the client sends with its next sign-in (AUTH-019).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    device_token: Option<String>,
}

impl SessionResponse {
    /// The response of `session`, with `tokens` and `device`, if any, in it when they go in the
    /// body.
    fn new(session: Session, tokens: Option<&Tokens>, device: Option<&DeviceToken>) -> Self {
        Self {
            id: session.id,
            user_id: session.user_id,
            device_id: session.device_id,
            device_name: session.device_name,
            provider: session.provider,
            created_at: session.created_at,
            access_token_expires_at: session.access_token_expires_at,
            refresh_token_expires_at: session.refresh_token_expires_at,
            expires_at: session.expires_at,
            authenticated_at: session.authenticated_at,
            email_verification_required: session.email_verification_required,
            second_factor_setup_required: session.second_factor_setup_required,
            access_token: tokens.map(|tokens| tokens.access.expose().clone()),
            refresh_token: tokens.map(|tokens| tokens.refresh.expose().clone()),
            device_token: device.map(|device| device.token.expose().clone()),
        }
    }
}

/// The tokens of the session that the request's browser holds, as its session cookies carry
/// them, for a request that starts a session in its place (AUTH-096). A cookie that may not
/// authenticate its request, since the request has no listed `Origin`, counts as none
/// (PLATFORM-059).
pub(super) fn held(cookies: &Cookies) -> Held {
    let token = |name| {
        cookies
            .credential(name)
            .ok()
            .flatten()
            .map(|token| Secret::new(token.to_owned()))
    };
    Held {
        access: token(ACCESS_COOKIE),
        refresh: token(REFRESH_COOKIE),
    }
}

/// The 204 of a request that ended the session that authenticated it, which clears a browser's
/// session cookies (AUTH-013, AUTH-023).
pub(super) fn signed_out() -> Result<Response, AuthError> {
    let mut response = StatusCode::NO_CONTENT.into_response();
    for name in [ACCESS_COOKIE, REFRESH_COOKIE] {
        let cookie = expired_cookie(name)?;
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    Ok(response)
}

/// How the request's client takes its tokens: a request with an `Origin` comes from a browser,
/// which only ever gets cookies, wherever its refresh token came from (SEC-07, AUTH-015).
pub(super) fn delivery(headers: &HeaderMap) -> TokenDelivery {
    if headers.contains_key(header::ORIGIN) {
        TokenDelivery::Cookie
    } else {
        TokenDelivery::Body
    }
}

/// The 201 of a new session, at `/v1/users/me/sessions/current`, with its tokens and its device
/// token delivered as `delivery` says (AUTH-001, AUTH-004, AUTH-019).
pub(super) fn created(
    session: Session,
    tokens: &Tokens,
    device: &DeviceToken,
    delivery: TokenDelivery,
) -> Result<Response, AuthError> {
    let (cookies, body) = deliver(session, tokens, Some(device), delivery)?;
    let mut response = (StatusCode::CREATED, axum::Json(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(header::LOCATION, HeaderValue::from_static(CURRENT_SESSION));
    for cookie in cookies {
        headers.append(header::SET_COOKIE, cookie);
    }
    Ok(response)
}

/// The 200 of a session that got new tokens, by a refresh or by a proof of who its user is, with
/// the tokens delivered as `delivery` says (AUTH-010, AUTH-095).
pub(super) fn renewed(
    session: Session,
    tokens: &Tokens,
    delivery: TokenDelivery,
) -> Result<Response, AuthError> {
    let (cookies, body) = deliver(session, tokens, None, delivery)?;
    let mut response = (StatusCode::OK, axum::Json(body)).into_response();
    for cookie in cookies {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    Ok(response)
}

/// The session's response, with its tokens and `device`, if any, in it, or in the cookies
/// returned beside it for a browser, the device token's in its email's (AUTH-015).
fn deliver(
    session: Session,
    tokens: &Tokens,
    device: Option<&DeviceToken>,
    delivery: TokenDelivery,
) -> Result<(Vec<HeaderValue>, SessionResponse), AuthError> {
    match delivery {
        TokenDelivery::Body => Ok((
            Vec::new(),
            SessionResponse::new(session, Some(tokens), device),
        )),
        TokenDelivery::Cookie => {
            let mut cookies = vec![
                session_cookie(
                    ACCESS_COOKIE,
                    tokens.access.expose(),
                    session.access_token_expires_at,
                )?,
                session_cookie(
                    REFRESH_COOKIE,
                    tokens.refresh.expose(),
                    session.refresh_token_expires_at,
                )?,
            ];
            if let Some(device) = device {
                cookies.push(session_cookie(
                    &device.cookie,
                    device.token.expose(),
                    device.expires_at,
                )?);
            }
            Ok((cookies, SessionResponse::new(session, None, None)))
        }
    }
}
