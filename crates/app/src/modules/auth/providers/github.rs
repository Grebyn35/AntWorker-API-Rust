//! GitHub, through OAuth 2.0 (the auth spec's "Signing in through a provider").

use futures_util::future::BoxFuture;
use platform::{
    outbound::{Configured, Request, Response},
    types::Secret,
};
use serde::Deserialize;

use super::{Client, Declared, Entry, Identity, Provider, ProviderError};
use crate::modules::PRODUCT;

/// GitHub in the registry, with one client, an OAuth app, whose callback URLs list a loopback
/// redirect and an `https` one alike (AUTH-083).
pub(super) const ENTRY: Entry = Entry {
    key: "github",
    clients: &["GITHUB_"],
    settings: &[],
    build: |_| Ok(Box::new(GitHub)),
};

const API: &str = "https://api.github.com";

/// `GET /user` of GitHub's REST API, which gives the user of a token.
const USER: &str = "https://api.github.com/user";

/// The version of GitHub's REST API that the requests name.
const API_VERSION: &str = "2022-11-28";

/// GitHub, which follows OAuth 2.0 and not OpenID Connect, so it reads the identity its own
/// way, and differs in its token endpoint's refusals and in its tokens, which never lapse.
struct GitHub;

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
    fn declared(&self) -> Declared<'_> {
        Declared {
            name: "GitHub",
            authorization_endpoint: "https://github.com/login/oauth/authorize",
            token_endpoint: "https://github.com/login/oauth/access_token",
            identity_endpoint: USER,
            scopes: &["user:email"],
        }
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

    fn identify<'a>(
        &'a self,
        outbound: &'a Configured,
        token: &'a Secret<String>,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>> {
        Box::pin(read(outbound, token))
    }

    /// Deletes the token, which never lapses (D139).
    fn discard<'a>(
        &'a self,
        outbound: &'a Configured,
        client: &'a Client,
        token: &'a Secret<String>,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(delete(outbound, client, token))
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
        .send(api(Request::get(USER), token))
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
/// since GitHub's tokens don't lapse, so no token outlives the request (AUTH-079, D139), or says
/// why it couldn't.
async fn delete(
    outbound: &Configured,
    client: &Client,
    token: &Secret<String>,
) -> Result<(), String> {
    let request = Request::delete(format!("{API}/applications/{}/token", client.id))
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", API_VERSION)
        .header("user-agent", PRODUCT)
        .basic(&client.id, &client.secret)
        .json(&serde_json::json!({ "access_token": token.expose() }));
    match outbound.send(request).await {
        Ok(answer) if answer.status == 204 => Ok(()),
        Ok(answer) => Err(format!("DELETE of the token answered {}", answer.status)),
        Err(error) => Err(error.to_string()),
    }
}
