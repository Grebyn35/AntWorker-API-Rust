//! GitHub, through OAuth 2.0 (the auth spec's "Signing in through a provider").

use futures_util::future::BoxFuture;
use platform::{
    errors,
    outbound::{Configured, Request, Response},
    types::Secret,
};
use serde::Deserialize;

use super::{Client, Entry, Identity, Provider, ProviderError};
use crate::modules::PRODUCT;

/// GitHub in the registry, with one client, an OAuth app, whose callback URLs list a loopback
/// redirect and an `https` one alike (AUTH-083).
pub(super) const ENTRY: Entry = Entry {
    id: "github",
    clients: &["GITHUB_"],
    build: |clients| Box::new(GitHub { clients }),
};

const AUTHORIZATION_ENDPOINT: &str = "https://github.com/login/oauth/authorize";
const TOKEN_ENDPOINT: &str = "https://github.com/login/oauth/access_token";
const API: &str = "https://api.github.com";

/// The version of GitHub's REST API that the requests name.
const API_VERSION: &str = "2022-11-28";

/// GitHub, with the deployment's client.
struct GitHub {
    clients: Vec<Client>,
}

/// The token endpoint's answer: a token, or, with a success status too, a refusal.
#[derive(Deserialize)]
struct Token {
    access_token: Option<String>,
    error: Option<String>,
}

/// The user, of `GET /user`.
#[derive(Deserialize)]
struct User {
    id: u64,
    login: String,
    name: Option<String>,
}

/// An address of the user's, of `GET /user/emails`.
#[derive(Deserialize)]
struct Email {
    email: String,
    primary: bool,
}

impl Provider for GitHub {
    fn id(&self) -> &'static str {
        ENTRY.id
    }

    fn name(&self) -> &'static str {
        "GitHub"
    }

    fn authorization_endpoint(&self) -> &'static str {
        AUTHORIZATION_ENDPOINT
    }

    fn scopes(&self) -> &'static [&'static str] {
        &["user:email"]
    }

    fn clients(&self) -> &[Client] {
        &self.clients
    }

    fn token_endpoint(&self) -> &'static str {
        TOKEN_ENDPOINT
    }

    /// GitHub answers a refusal with a success status: one of the API's own client is no user's
    /// to fix, so it is an unavailable provider, and any other is of the code, its verifier or
    /// its redirect URI (AUTH-083).
    fn token(&self, answer: &Response) -> Result<Secret<String>, ProviderError> {
        match answer.json::<Token>() {
            Some(Token {
                access_token: Some(token),
                error: None,
            }) => Ok(Secret::new(token)),
            Some(Token {
                error: Some(error), ..
            }) if error == "incorrect_client_credentials" => Err(ProviderError::Unavailable(
                "the token endpoint refused the API's client".to_owned(),
            )),
            Some(Token { error: Some(_), .. }) => Err(ProviderError::Rejected),
            _ => Err(ProviderError::Unavailable(
                "the token endpoint's answer holds no token".to_owned(),
            )),
        }
    }

    /// Reads the identity, then deletes the token, whatever the outcome (D139).
    fn identify<'a>(
        &'a self,
        outbound: &'a Configured,
        client: &'a Client,
        token: &'a Secret<String>,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>> {
        Box::pin(async move {
            let identity = read(outbound, token).await;
            delete(outbound, client, token).await;
            identity
        })
    }
}

/// A request of GitHub's REST API, with the headers it asks for, as the user `token` sends it.
fn api(request: Request, token: &Secret<String>) -> Request {
    request
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", API_VERSION)
        .header("user-agent", PRODUCT)
        .bearer(Secret::new(token.expose().clone()))
}

/// The identity of the user whose token `token` is: their ID, which GitHub never reassigns,
/// their name, or their login without one, and their primary address, when the token may read
/// their addresses. GitHub proves no address (AUTH-075, D140).
async fn read(outbound: &Configured, token: &Secret<String>) -> Result<Identity, ProviderError> {
    let unavailable =
        |error: platform::outbound::OutboundError| ProviderError::Unavailable(error.to_string());
    let user = outbound
        .send(api(Request::get(format!("{API}/user")), token))
        .await
        .map_err(unavailable)?;
    let user: User = readable(&user, "GET /user")?;
    let emails = outbound
        .send(api(Request::get(format!("{API}/user/emails")), token))
        .await
        .map_err(unavailable)?;
    // A token that may not read the addresses gives none, rather than failing.
    let email = if matches!(emails.status, 403 | 404) {
        None
    } else {
        readable::<Vec<Email>>(&emails, "GET /user/emails")?
            .into_iter()
            .find(|email| email.primary)
            .map(|email| email.email)
    };
    Ok(Identity {
        subject: user.id.to_string(),
        email,
        proved: false,
        name: user
            .name
            .filter(|name| !name.trim().is_empty())
            .or(Some(user.login)),
    })
}

/// `answer`'s body as `T`, when its status is a success and it reads as one.
fn readable<T: serde::de::DeserializeOwned>(
    answer: &Response,
    request: &str,
) -> Result<T, ProviderError> {
    answer
        .is_success()
        .then(|| answer.json())
        .flatten()
        .ok_or_else(|| ProviderError::Unavailable(format!("{request} answered {}", answer.status)))
}

/// Deletes `token` through GitHub's endpoint that deletes one token and keeps the user's grant,
/// since GitHub's tokens don't lapse, so no token outlives the request (AUTH-079, D139). A
/// failure refuses nothing: it is logged and counted (ERR-11).
async fn delete(outbound: &Configured, client: &Client, token: &Secret<String>) {
    let request = Request::delete(format!("{API}/applications/{}/token", client.id))
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", API_VERSION)
        .header("user-agent", PRODUCT)
        .basic(&client.id, &client.secret)
        .json(&serde_json::json!({ "access_token": token.expose() }));
    let failure = match outbound.send(request).await {
        Ok(answer) if answer.status == 204 => return,
        Ok(answer) => format!("DELETE of the token answered {}", answer.status),
        Err(error) => error.to_string(),
    };
    errors::ignore(&TokenKept(failure), "provider_token_not_deleted");
}

/// GitHub didn't delete a token that a sign-in used (AUTH-079).
#[derive(Debug, thiserror::Error)]
#[error("GitHub's token wasn't deleted: {0}")]
struct TokenKept(String);
