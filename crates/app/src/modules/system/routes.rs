//! The capabilities endpoint (SYSTEM-001).

use axum::{Json, extract::State};
use platform::http::Routes;
use serde::Serialize;
use utoipa::ToSchema;
use utoipa_axum::routes;

/// The features this deployment offers.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub(crate) struct Capabilities {
    /// One member per feature a deployment can turn off, `true` while the feature is on, and one
    /// per optional part that changes what a client offers, `true` while the fork keeps it. A
    /// feature or a part this API doesn't have is absent.
    features: Features,
    /// The OAuth providers that a user signs in through, each of which the deployment
    /// configures, by their IDs, with what a client builds the authorization request from
    /// (AUTH-081).
    sign_in_providers: Vec<SignInProvider>,
}

/// An OAuth provider that a user signs in through, as the capabilities list it, with what a
/// client builds the authorization request from (AUTH-081).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub(crate) struct SignInProvider {
    /// Its key, which a sign-in's `provider` names.
    pub(crate) provider: &'static str,
    /// Its name, as a client shows it.
    pub(crate) name: &'static str,
    /// The URL of its authorization endpoint.
    pub(crate) authorization_endpoint: &'static str,
    /// The scopes that the authorization request asks for.
    pub(crate) scopes: Vec<&'static str>,
    /// The deployment's clients, each with the redirect URIs it lists.
    pub(crate) clients: Vec<SignInClient>,
}

/// A client of a provider, as the capabilities list it (AUTH-081).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub(crate) struct SignInClient {
    /// The client's ID, which the provider issued, and the authorization request names.
    pub(crate) client_id: String,
    /// The redirect URIs it lists, of which the authorization request names one.
    pub(crate) redirect_uris: Vec<String>,
}

impl Capabilities {
    /// The capabilities of a deployment with `features`, whose users sign in through
    /// `sign_in_providers`.
    pub(crate) fn new(features: Features, sign_in_providers: Vec<SignInProvider>) -> Self {
        Self {
            features,
            sign_in_providers,
        }
    }
}

/// Each feature a deployment can turn off, and whether it is on, and each optional part that the
/// fork keeps and that changes what a client offers (D118).
#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
pub(crate) struct Features {
    /// Whether the deployment sends email (PLATFORM-022). While it is off, a client hides what
    /// needs email, such as a way to reset a password.
    email: bool,
    // part: invitations
    /// The fork keeps `invitations`, so a workspace's members invite others.
    invitations: bool,
    // end part: invitations
    // part: files
    /// The fork keeps `files`, so users upload files, avatars and workspace pictures.
    files: bool,
    // end part: files
}

impl Features {
    /// The features of a deployment that sends email while `email` is on, with every optional
    /// part the fork keeps.
    pub(crate) fn new(email: bool) -> Self {
        Self {
            email,
            // part: invitations
            invitations: true,
            // end part: invitations
            // part: files
            files: true,
            // end part: files
        }
    }
}

/// The module's routes, which take the deployment's capabilities.
pub(crate) fn routes() -> Routes<Capabilities> {
    Routes::new().public(routes!(capabilities), &[])
}

/// Lists the features a deployment can turn off, and whether each one is on.
#[utoipa::path(
    get,
    path = "/v1/capabilities",
    tag = "system",
    responses(
        (status = 200, description = "The deployment's features.", body = Capabilities),
    )
)]
async fn capabilities(State(capabilities): State<Capabilities>) -> Json<Capabilities> {
    Json(capabilities)
}
