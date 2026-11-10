//! Registration, sign-in, refresh and the current session (the auth spec's Routes). Proving the
//! account's email address is in `verification.rs`.

mod verification;

use std::sync::Arc;

use axum::{
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use platform::{
    authn::{ACCESS_COOKIE, Principal},
    errors::{Problem, RequestError},
    http::{Cookies, FieldErrors, Json, Routes, expired_cookie, session_cookie},
    rate_limit::{LimitError, RateLimits},
    types::{Client, EmailAddress, Secret, SessionId, Timestamp, UserId},
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::routes;

use super::{
    error::AuthError,
    model::{DEVICE_COOKIE, DeviceId, DeviceToken, Password, Session, TokenDelivery, Tokens},
    service::{Auth, Registration, SignIn},
};
use crate::modules::accounts::DisplayName;

/// The name of the cookie that holds a browser's refresh token, after `__Host-` (AUTH-015).
const REFRESH_COOKIE: &str = "refresh_token";

/// Where a new session is: the session that the request's own tokens prove (AUTH-016).
const CURRENT_SESSION: &str = "/v1/users/me/sessions/current";

/// The module's routes, which take the service as their state.
pub(crate) fn routes() -> Routes<Arc<Auth>> {
    verification::routes()
        .public(
            routes!(register),
            &[&AuthError::EmailInUse, &LimitError::RateLimited],
        )
        .public(routes!(sign_in), &[&AuthError::InvalidCredentials])
        .public(
            routes!(refresh),
            &[&AuthError::SessionExpired, &AuthError::InvalidRefreshToken],
        )
        .verifying(routes!(current, sign_out), &[])
        .user(
            routes!(reauthenticate),
            &[&AuthError::WrongPassword, &LimitError::RateLimited],
        )
        .user(
            routes!(delete_account),
            &[
                &AuthError::ReauthenticationRequired,
                &AuthError::OwnsSharedWorkspace,
                &AuthError::AccountChanged,
                &LimitError::RateLimited,
            ],
        )
}

/// What registering takes (AUTH-001).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RegistrationRequest {
    /// The user's email address.
    #[schema(value_type = EmailAddress)]
    email: String,
    /// The user's password (AUTH-003).
    #[schema(value_type = Password)]
    password: String,
    /// The user's name.
    #[schema(value_type = DisplayName)]
    name: String,
    /// An ID the client keeps for its installation (AUTH-008, AUTH-017).
    #[serde(default)]
    #[schema(value_type = Option<DeviceId>)]
    device_id: Option<String>,
}

impl TryFrom<RegistrationRequest> for Registration {
    type Error = RequestError;

    fn try_from(request: RegistrationRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let email = errors.check("/email", EmailAddress::parse(&request.email));
        let password = errors.check("/password", Password::parse(request.password));
        let name = errors.check("/name", DisplayName::parse(&request.name));
        let device_id = device_id(&mut errors, request.device_id.as_deref());
        let built = email.zip(password).zip(name).zip(device_id).map(
            |(((email, password), name), device_id)| Self {
                email,
                password,
                name,
                device_id,
            },
        );
        errors.finish(built)
    }
}

/// What signing in takes (AUTH-004).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SignInRequest {
    /// The account's email address.
    #[schema(value_type = EmailAddress)]
    email: String,
    /// The account's password.
    #[schema(min_length = 1, max_length = 128, format = Password)]
    password: String,
    /// An ID the client keeps for its installation (AUTH-008, AUTH-017).
    #[serde(default)]
    #[schema(value_type = Option<DeviceId>)]
    device_id: Option<String>,
    /// The device token that the account's last registration or sign-in on this device returned
    /// (AUTH-019). A browser leaves it out, and its cookie carries it. One that isn't valid for
    /// the email counts as none.
    #[serde(default)]
    #[schema(min_length = 1, max_length = 64)]
    device_token: Option<String>,
}

impl TryFrom<SignInRequest> for SignIn {
    type Error = RequestError;

    fn try_from(request: SignInRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let email = errors.check("/email", EmailAddress::parse(&request.email));
        let password = errors.check("/password", Password::presented(request.password));
        let device_id = device_id(&mut errors, request.device_id.as_deref());
        let device_token = request.device_token.map(Secret::new);
        let built = email
            .zip(password)
            .zip(device_id)
            .map(|((email, password), device_id)| Self {
                email,
                password,
                device_id,
                device_token,
                device_cookies: Vec::new(),
            });
        errors.finish(built)
    }
}

/// The device ID in `value`, if any: `Some(None)` without one, and `None` when it is invalid.
fn device_id(errors: &mut FieldErrors, value: Option<&str>) -> Option<Option<DeviceId>> {
    match value {
        None => Some(None),
        Some(value) => errors.check("/device_id", DeviceId::parse(value)).map(Some),
    }
}

/// What refreshing takes: the refresh token, unless a browser's cookie holds it (AUTH-010).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RefreshRequest {
    /// The session's current refresh token. A browser leaves it out, and its cookie carries it.
    #[serde(default)]
    #[schema(min_length = 1, max_length = 64)]
    refresh_token: Option<String>,
}

/// What re-authenticating takes (AUTH-021).
#[derive(Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReauthenticationRequest {
    /// The caller's password.
    #[schema(min_length = 1, max_length = 128, format = Password)]
    password: String,
}

impl TryFrom<ReauthenticationRequest> for Password {
    type Error = RequestError;

    fn try_from(request: ReauthenticationRequest) -> Result<Self, Self::Error> {
        let mut errors = FieldErrors::new();
        let password = errors.check("/password", Password::presented(request.password));
        errors.finish(password)
    }
}

/// A session, and its tokens when they go in the body.
#[derive(Serialize, ToSchema)]
pub(crate) struct SessionResponse {
    /// The session's ID.
    id: SessionId,
    /// The user the session signs in.
    user_id: UserId,
    /// The device ID the client signed in with, if any.
    device_id: Option<DeviceId>,
    /// When the session began.
    created_at: Timestamp,
    /// When its access token expires.
    access_token_expires_at: Timestamp,
    /// When its refresh token expires, unless a refresh slides it.
    refresh_token_expires_at: Timestamp,
    /// When the session ends, whatever its refreshes.
    expires_at: Timestamp,
    /// When its user last proved their password in it: when it began, or re-authenticated
    /// (AUTH-021).
    authenticated_at: Timestamp,
    /// Whether its account must prove its email address, with the code sent to it, before the
    /// API serves it anything but proving it (AUTH-025, AUTH-026).
    email_verification_required: bool,
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
            created_at: session.created_at,
            access_token_expires_at: session.access_token_expires_at,
            refresh_token_expires_at: session.refresh_token_expires_at,
            expires_at: session.expires_at,
            authenticated_at: session.authenticated_at,
            email_verification_required: session.email_verification_required,
            access_token: tokens.map(|tokens| tokens.access.expose().clone()),
            refresh_token: tokens.map(|tokens| tokens.refresh.expose().clone()),
            device_token: device.map(|device| device.token.expose().clone()),
        }
    }
}

/// Registers a user, and signs them in.
#[utoipa::path(
    post,
    path = "/v1/users",
    tag = "auth",
    request_body = RegistrationRequest,
    responses(
        (status = 201, description = "The user's first session.", body = SessionResponse,
            headers(("location" = String, description = "The new session: `/v1/users/me/sessions/current`."))),
    )
)]
async fn register(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    headers: HeaderMap,
    Json(request): Json<RegistrationRequest>,
) -> Result<Response, Problem<AuthError>> {
    let input = Registration::try_from(request).map_err(AuthError::from)?;
    let (session, tokens, device) = auth.register(&limits, &client, input).await?;
    Ok(created(session, &tokens, &device, delivery(&headers))?)
}

/// Signs in with an email and a password.
#[utoipa::path(
    post,
    path = "/v1/sessions",
    tag = "auth",
    request_body = SignInRequest,
    responses(
        (status = 201, description = "The new session.", body = SessionResponse,
            headers(("location" = String, description = "The new session: `/v1/users/me/sessions/current`."))),
    )
)]
async fn sign_in(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    cookies: Cookies,
    headers: HeaderMap,
    Json(request): Json<SignInRequest>,
) -> Result<Response, Problem<AuthError>> {
    let mut input = SignIn::try_from(request).map_err(AuthError::from)?;
    input.device_cookies = cookies
        .starting_with(DEVICE_COOKIE)
        .into_iter()
        .map(|(name, token)| (name.to_owned(), Secret::new(token.to_owned())))
        .collect();
    let (session, tokens, device) = auth.sign_in(&limits, &client, input).await?;
    Ok(created(session, &tokens, &device, delivery(&headers))?)
}

/// Gives the session a new pair of tokens.
#[utoipa::path(
    post,
    path = "/v1/sessions/refresh",
    tag = "auth",
    request_body = RefreshRequest,
    responses(
        (status = 200, description = "The session, with its new tokens.", body = SessionResponse),
    )
)]
async fn refresh(
    State(auth): State<Arc<Auth>>,
    client: Client,
    cookies: Cookies,
    headers: HeaderMap,
    Json(request): Json<RefreshRequest>,
) -> Result<Response, Problem<AuthError>> {
    let token = match request.refresh_token {
        Some(token) => token,
        None => match cookies
            .credential(REFRESH_COOKIE)
            .map_err(AuthError::from)?
        {
            Some(token) => token.to_owned(),
            None => return Err(AuthError::InvalidRefreshToken.into()),
        },
    };
    let (session, tokens) = auth.refresh(&client, &token).await?;
    let (cookies, body) = deliver(session, &tokens, None, delivery(&headers))?;
    let mut response = (StatusCode::OK, axum::Json(body)).into_response();
    for cookie in cookies {
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    Ok(response)
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
    let mut response = StatusCode::NO_CONTENT.into_response();
    for name in [ACCESS_COOKIE, REFRESH_COOKIE] {
        let cookie = expired_cookie(name).map_err(AuthError::from)?;
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    Ok(response)
}

/// Re-authenticates: proves the caller's password again in the session that authenticated the
/// request, for an action that needs it, such as deleting the account.
#[utoipa::path(
    post,
    path = "/v1/users/me/sessions/current/reauthenticate",
    tag = "auth",
    request_body = ReauthenticationRequest,
    responses(
        (status = 200, description = "The session, without its tokens.", body = Session),
    )
)]
async fn reauthenticate(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
    Json(request): Json<ReauthenticationRequest>,
) -> Result<axum::Json<Session>, Problem<AuthError>> {
    let password = Password::try_from(request).map_err(AuthError::from)?;
    let session = auth
        .reauthenticate(&limits, &client, &principal, password)
        .await?;
    Ok(axum::Json(session))
}

/// Deletes the caller's account, and everything it holds, once the session that authenticated
/// the request has proved their password within the last 5 minutes.
#[utoipa::path(
    delete,
    path = "/v1/users/me",
    tag = "auth",
    responses(
        (status = 204, description = "The account is deleted, and every session of it ended."),
    )
)]
async fn delete_account(
    State(auth): State<Arc<Auth>>,
    limits: RateLimits,
    client: Client,
    principal: Principal,
) -> Result<Response, Problem<AuthError>> {
    auth.delete_account(&limits, &client, &principal).await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    for name in [ACCESS_COOKIE, REFRESH_COOKIE] {
        let cookie = expired_cookie(name).map_err(AuthError::from)?;
        response.headers_mut().append(header::SET_COOKIE, cookie);
    }
    Ok(response)
}

/// How the request's client takes its tokens: a request with an `Origin` comes from a browser,
/// which only ever gets cookies, wherever its refresh token came from (SEC-07, AUTH-015).
fn delivery(headers: &HeaderMap) -> TokenDelivery {
    if headers.contains_key(header::ORIGIN) {
        TokenDelivery::Cookie
    } else {
        TokenDelivery::Body
    }
}

/// The 201 of a new session, at `/v1/users/me/sessions/current`, with its tokens and its device
/// token delivered as `delivery` says (AUTH-001, AUTH-004, AUTH-019).
fn created(
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
