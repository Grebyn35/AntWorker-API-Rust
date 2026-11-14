//! `seed`, which fills a development database with demo accounts and workspaces (the system
//! spec's Seed, D28). It changes the API's data through its routes, as any client does: it
//! sends the API's router each request in process, with a JSON body and the Bearer header of the
//! account it acts as, so every row it creates passes a client's checks and carries the audit
//! entries and events that the request records
//! ([system spec](../../../../../docs/spec/system.md#seed)). With email on, it also runs the jobs
//! that its registrations enqueued, and proves each account's address with the code that their
//! emails carry, which it reads from an inbox of its own, as a client reads the code from its
//! mailbox.
// part: workspace_setup
//! It sets one workspace up as the deployment's operator does, through the operation that the
//! composition root hands it, which no route offers (SYSTEM-009).
// end part: workspace_setup

use std::net::{Ipv4Addr, SocketAddr};

use axum::{
    Router,
    body::{self, Body},
    extract::ConnectInfo,
    http::{Method, Request, StatusCode, header},
};
use platform::{
    clock::SharedClock, config::Environment, db::Database, errors::Internal, jobs::Jobs,
    mail::Inbox, types::EmailAddress,
};
use serde_json::{Value, json};
use tower::ServiceExt;

use super::error::SystemError;

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
    /// Whether it must still prove its address, as registering answered (AUTH-026).
    email_verification_required: bool,
}

/// What the seed reads its accounts' codes with, while email is on: the API's jobs, which it runs
/// once its accounts have registered, on the API's database and clock, and the inbox that the
/// emails with their codes go to, rather than the mail server (the system spec's Seed).
pub(crate) struct Mailbox<'a> {
    /// The API's jobs, among them the one that sends a code (AUTH-030).
    pub(crate) jobs: &'a Jobs,
    /// The database the jobs run on.
    pub(crate) database: &'a Database,
    /// The clock the jobs run by.
    pub(crate) clock: &'a SharedClock,
    /// Where the emails with the codes go.
    pub(crate) inbox: &'a Inbox,
}

// part: workspace_setup
/// The operator's setup of a workspace, as the composition root hands it to the seed: given the
/// workspace's name and its owner's address, it sets the workspace up, and answers its ID
/// (SYSTEM-009).
pub(crate) type SetUp<'a> = &'a dyn Fn(&'static str, &'static str) -> SettingUp<'a>;

/// A setup that runs, which answers its workspace's ID.
pub(crate) type SettingUp<'a> =
    std::pin::Pin<Box<dyn Future<Output = Result<String, Internal>> + 'a>>;
// end part: workspace_setup

/// What the seed fills a database through: the API's router, which its requests go to, and what
/// it does beside them.
pub(crate) struct Seed<'a> {
    /// The API's router, as `serve` builds it.
    pub(crate) router: Router,
    /// What it reads its accounts' codes with.
    pub(crate) mailbox: Mailbox<'a>,
    // part: workspace_setup
    /// The operator's setup of a workspace.
    pub(crate) set_up: SetUp<'a>,
    // end part: workspace_setup
}

/// What the seed created: the accounts, each with the password [`PASSWORD`], and the workspaces.
#[derive(Debug)]
pub(crate) struct Seeded {
    /// The accounts, in the order it registered them.
    pub(crate) accounts: Vec<Account>,
    /// Each workspace's name and ID, which a developer names in the paths of its routes, in the
    /// order it created them.
    pub(crate) workspaces: Vec<(&'static str, String)>,
}

/// Refuses unless `environment` is development, so no deployment holds the seed's accounts, whose
/// password it prints (SYSTEM-002).
pub(crate) fn allowed(environment: Environment) -> Result<(), SystemError> {
    match environment {
        Environment::Development => Ok(()),
        Environment::Test | Environment::Production => Err(SystemError::NotDevelopment),
    }
}

/// Fills the database behind the router of `seed` with the demo data (SYSTEM-003), proving each
/// account's address with the code that its mailbox receives while email is on (SYSTEM-006), and
/// stops at the first account that exists already (SYSTEM-004).
pub(crate) async fn seed(seed: Seed<'_>) -> Result<Seeded, SystemError> {
    let api = Api {
        router: seed.router,
    };
    let olivia = api.register("owner@example.com", "Olivia Owner").await?;
    let adam = api.register("admin@example.com", "Adam Admin").await?;
    let mia = api.register("member@example.com", "Mia Member").await?;
    // part: invitations
    let ian = api.register("invitee@example.com", "Ian Invitee").await?;
    // end part: invitations
    let registered = vec![&olivia, &adam, &mia];
    // part: invitations
    let registered = [registered, vec![&ian]].concat();
    // end part: invitations
    api.verify(&seed.mailbox, &registered).await?;
    // The workspaces, Olivia's first: each part that creates workspaces adds its own.
    let mut workspaces = Vec::new();
    // part: workspace_creation
    // The workspaces that their owners create (SYSTEM-008).
    for (owner, name) in [(&olivia, "Acme"), (&mia, "Side Project")] {
        workspaces.push((name, api.create_workspace(owner, name).await?));
    }
    // end part: workspace_creation
    // part: workspace_setup
    // The workspace that the operator sets up for Olivia (SYSTEM-009).
    let labs = (seed.set_up)("Acme Labs", olivia.email).await?;
    workspaces.push(("Acme Labs", labs));
    // end part: workspace_setup
    // part: invitations
    // The members of the first workspace, and the invitation that waits (SYSTEM-005).
    let Some((_, shared)) = workspaces.first() else {
        return Err(
            Internal::new("the seed created no workspace to invite its accounts to").into(),
        );
    };
    for (invitee, role) in [(&adam, "admin"), (&mia, "member")] {
        let invitation = api.invite(&olivia, shared, invitee, role).await?;
        api.send(
            Method::POST,
            &format!("/v1/users/me/invitations/{invitation}/accept"),
            Some(invitee),
            None,
        )
        .await?;
    }
    api.invite(&olivia, shared, &ian, "member").await?;
    // end part: invitations
    let accounts = vec![olivia, adam, mia];
    // part: invitations
    let accounts = [accounts, vec![ian]].concat();
    // end part: invitations
    Ok(Seeded {
        accounts,
        workspaces,
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
    ) -> Result<Account, SystemError> {
        let body = json!({ "email": email, "password": PASSWORD, "name": name });
        let answer = match self.send(Method::POST, "/v1/users", None, Some(body)).await {
            Err(SystemError::Refused {
                status: StatusCode::CONFLICT,
                ..
            }) => return Err(SystemError::Seeded(email)),
            answer => answer?,
        };
        Ok(Account {
            email,
            name,
            access_token: text(&answer, "access_token")?,
            email_verification_required: answer["email_verification_required"].as_bool()
                == Some(true),
        })
    }

    /// Proves the address of each of `accounts` that must still prove it, with the code in the
    /// latest email that `mailbox` received for it, once the jobs that its registration enqueued
    /// have run (AUTH-030, AUTH-031).
    async fn verify(
        &self,
        mailbox: &Mailbox<'_>,
        accounts: &[&Account],
    ) -> Result<(), SystemError> {
        if !accounts
            .iter()
            .any(|account| account.email_verification_required)
        {
            return Ok(());
        }
        mailbox
            .jobs
            .run_pending(mailbox.database, mailbox.clock)
            .await;
        for account in accounts
            .iter()
            .filter(|account| account.email_verification_required)
        {
            let address = EmailAddress::parse(account.email)
                .map_err(|_| SystemError::Internal(Internal::new("a seed account's address")))?;
            let emails = mailbox.inbox.to(&address)?;
            let code = emails
                .last()
                .and_then(|email| code_in(&email.text))
                .ok_or_else(|| {
                    SystemError::Internal(Internal::new("no code reached a seed account's inbox"))
                })?;
            self.send(
                Method::POST,
                "/v1/users/me/email/verification/confirm",
                Some(account),
                Some(json!({ "code": code })),
            )
            .await?;
        }
        Ok(())
    }

    // part: workspace_creation
    /// Creates the workspace `name`, which `owner` owns, and returns its ID (WORKSPACES-001).
    async fn create_workspace(&self, owner: &Account, name: &str) -> Result<String, SystemError> {
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
    // end part: workspace_creation

    // part: invitations
    /// Has `inviter` invite `invitee` to `workspace` at `role`, and returns the invitation's ID
    /// (INVITATIONS-001).
    async fn invite(
        &self,
        inviter: &Account,
        workspace: &str,
        invitee: &Account,
        role: &str,
    ) -> Result<String, SystemError> {
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
    ) -> Result<Value, SystemError> {
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
            .map_err(|error| SystemError::Internal(Internal::new(error.to_string())))?;
        request
            .extensions_mut()
            .insert(ConnectInfo(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))));
        let answer = self
            .router
            .clone()
            .oneshot(request)
            .await
            .map_err(|error| SystemError::Internal(Internal::new(error.to_string())))?;
        let status = answer.status();
        let bytes = body::to_bytes(answer.into_body(), ANSWER_LIMIT)
            .await
            .map_err(|error| SystemError::Internal(Internal::new(error.to_string())))?;
        if !status.is_success() {
            return Err(SystemError::Refused {
                method,
                path: path.to_owned(),
                status,
                body: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        if bytes.is_empty() {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes)
            .map_err(|error| SystemError::Internal(Internal::new(error.to_string())))
    }
}

/// The string field `field` of `answer`.
fn text(answer: &Value, field: &str) -> Result<String, SystemError> {
    answer[field].as_str().map(str::to_owned).ok_or_else(|| {
        SystemError::Internal(Internal::new(format!(
            "the answer has no {field}: {answer}"
        )))
    })
}

/// The code in an email's text: its one word of 6 digits (AUTH-030).
fn code_in(text: &str) -> Option<&str> {
    text.split_whitespace()
        .find(|word| word.len() == 6 && word.bytes().all(|byte| byte.is_ascii_digit()))
}
