//! `seed`, which fills a development database with demo accounts and workspaces (SYSTEM-003 to
//! SYSTEM-005, D28). It knows the API only through its routes, as any client does: it sends the
//! API's router each request in process, with a JSON body and the Bearer header of the account it
//! acts as, so every row it creates passes a client's checks and carries the audit entries and
//! events that the request records ([system spec](../../../../../docs/spec/system.md#seed)).

use std::net::{Ipv4Addr, SocketAddr};

use axum::{
    Router,
    body::{self, Body},
    extract::ConnectInfo,
    http::{Method, Request, StatusCode, header},
};
use platform::config::Environment;
use serde_json::{Value, json};
use tower::ServiceExt;

/// The password of every account the seed registers, which it prints, since a developer signs in
/// as each of them in turn.
pub(crate) const PASSWORD: &str = "orchard-lantern-42-violet";

/// The most bytes of an answer the seed reads, far more than any of its answers holds.
const ANSWER_LIMIT: usize = 1 << 20;

/// An account that the seed registered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Account {
    /// Its email address, which a developer signs in with.
    pub(crate) email: &'static str,
    /// Its name.
    pub(crate) name: &'static str,
    access_token: String,
}

/// Why the seed stopped, or didn't start.
#[derive(Debug, thiserror::Error)]
pub(crate) enum SeedError {
    /// `ENVIRONMENT` isn't `development` (SYSTEM-002).
    #[error(
        "seed runs only when ENVIRONMENT is development, since every account it registers has the \
         password it prints"
    )]
    NotDevelopment,
    /// An account it registers exists already, so the database holds its data (SYSTEM-004).
    #[error(
        "{0} is registered already, so this database holds the seed's data: it seeds a database \
         once, and again only after the database is recreated"
    )]
    Seeded(&'static str),
    /// The API refused one of its requests.
    #[error("{method} {path} answered {status}: {body}")]
    Refused {
        /// The request's method.
        method: Method,
        /// The request's path.
        path: String,
        /// The answer's status.
        status: StatusCode,
        /// The answer's body.
        body: String,
    },
    /// A request couldn't be built or answered, or an answer wasn't the JSON it should be.
    #[error("{0}")]
    Exchange(String),
}

/// What the seed created: the accounts, each with the password [`PASSWORD`].
#[derive(Debug)]
pub(crate) struct Seeded {
    /// The accounts, in the order it registered them.
    pub(crate) accounts: Vec<Account>,
}

/// Refuses unless `environment` is development, so no deployment holds the seed's accounts, whose
/// password it prints (SYSTEM-002).
pub(crate) fn allowed(environment: Environment) -> Result<(), SeedError> {
    match environment {
        Environment::Development => Ok(()),
        Environment::Test | Environment::Production => Err(SeedError::NotDevelopment),
    }
}

/// Fills the database behind `router` with the demo data (SYSTEM-003, SYSTEM-005), and stops at
/// the first account that exists already (SYSTEM-004).
pub(crate) async fn seed(router: Router) -> Result<Seeded, SeedError> {
    let api = Api { router };
    let olivia = api.register("owner@example.com", "Olivia Owner").await?;
    let adam = api.register("admin@example.com", "Adam Admin").await?;
    let mia = api.register("member@example.com", "Mia Member").await?;
    // part: invitations
    let ian = api.register("invitee@example.com", "Ian Invitee").await?;
    // end part: invitations
    let acme = api.create_workspace(&olivia, "Acme").await?;
    api.create_workspace(&mia, "Side Project").await?;
    // part: invitations
    for (invitee, role) in [(&adam, "admin"), (&mia, "member")] {
        let invitation = api.invite(&olivia, &acme, invitee, role).await?;
        api.send(
            Method::POST,
            &format!("/v1/users/me/invitations/{invitation}/accept"),
            Some(invitee),
            None,
        )
        .await?;
    }
    api.invite(&olivia, &acme, &ian, "member").await?;
    // end part: invitations
    Ok(Seeded {
        accounts: vec![
            olivia, adam, mia, ian, // part: invitations
        ],
    })
}

/// The API's router, which the seed sends its requests to.
struct Api {
    router: Router,
}

impl Api {
    /// Registers `email` as `name`, with the seed's password (AUTH-001).
    async fn register(
        &self,
        email: &'static str,
        name: &'static str,
    ) -> Result<Account, SeedError> {
        let body = json!({ "email": email, "password": PASSWORD, "name": name });
        let answer = match self.send(Method::POST, "/v1/users", None, Some(body)).await {
            Err(SeedError::Refused {
                status: StatusCode::CONFLICT,
                ..
            }) => return Err(SeedError::Seeded(email)),
            answer => answer?,
        };
        Ok(Account {
            email,
            name,
            access_token: text(&answer, "access_token")?,
        })
    }

    /// Creates the workspace `name`, which `owner` owns, and returns its ID (WORKSPACES-001).
    async fn create_workspace(&self, owner: &Account, name: &str) -> Result<String, SeedError> {
        let answer = self
            .send(
                Method::POST,
                "/v1/workspaces",
                Some(owner),
                Some(json!({ "name": name })),
            )
            .await?;
        text(&answer, "id")
    }

    // part: invitations
    /// Has `inviter` invite `invitee` to `workspace` at `role`, and returns the invitation's ID
    /// (INVITATIONS-001).
    async fn invite(
        &self,
        inviter: &Account,
        workspace: &str,
        invitee: &Account,
        role: &str,
    ) -> Result<String, SeedError> {
        let answer = self
            .send(
                Method::POST,
                &format!("/v1/workspaces/{workspace}/invitations"),
                Some(inviter),
                Some(json!({ "email": invitee.email, "role": role })),
            )
            .await?;
        text(&answer, "id")
    }
    // end part: invitations

    /// Sends `method` to `path`, as `account` when given, with `body` as JSON when given, as a
    /// client that sets its own headers does from 127.0.0.1, and returns the answer's JSON, or
    /// why the API refused it.
    async fn send(
        &self,
        method: Method,
        path: &str,
        account: Option<&Account>,
        body: Option<Value>,
    ) -> Result<Value, SeedError> {
        let mut request = Request::builder()
            .method(method.clone())
            .uri(path)
            .header(header::USER_AGENT, "seed");
        if let Some(account) = account {
            request = request.header(
                header::AUTHORIZATION,
                format!("Bearer {}", account.access_token),
            );
        }
        let body = match body {
            Some(body) => {
                request = request.header(header::CONTENT_TYPE, "application/json");
                Body::from(body.to_string())
            }
            None => Body::empty(),
        };
        let mut request = request
            .body(body)
            .map_err(|error| SeedError::Exchange(error.to_string()))?;
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))));
        let answer = self
            .router
            .clone()
            .oneshot(request)
            .await
            .map_err(|error| SeedError::Exchange(error.to_string()))?;
        let status = answer.status();
        let bytes = body::to_bytes(answer.into_body(), ANSWER_LIMIT)
            .await
            .map_err(|error| SeedError::Exchange(error.to_string()))?;
        if !status.is_success() {
            return Err(SeedError::Refused {
                method,
                path: path.to_owned(),
                status,
                body: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|error| SeedError::Exchange(error.to_string()))
    }
}

/// The string field `field` of `answer`.
fn text(answer: &Value, field: &str) -> Result<String, SeedError> {
    answer[field]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| SeedError::Exchange(format!("the answer has no {field}: {answer}")))
}
