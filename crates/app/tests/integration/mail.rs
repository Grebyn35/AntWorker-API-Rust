//! Sending email from jobs (the mail spec).

use std::{net::SocketAddr, sync::Arc};

use platform::{clock::TestClock, lifecycle::Readiness};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

use crate::support::{
    Api, Options,
    api::{Response, application},
    mailbox,
    users::{self, SignedIn},
    workspaces,
};

/// An API whose emails go to Mailpit, with Ada owning the workspace Acme.
async fn acme(options: Options) -> (Api, SignedIn, String) {
    let api = Api::with(options).await;
    let ada = users::register(&api, "ada@example.com").await;
    let workspace = workspaces::arrange(api.database(), "Acme", &[(&ada.user_id, "owner")]).await;
    (api, ada, workspace)
}

/// Invites `email` to `workspace` as a member, as `user`.
async fn invite(api: &Api, user: &SignedIn, workspace: &str, email: &str) -> Response {
    api.post(
        &format!("/v1/workspaces/{workspace}/invitations"),
        &json!({ "email": email, "role": "member" }),
    )
    .bearer(&user.access)
    .send()
    .await
}

/// One column of the jobs table, as text.
async fn jobs(api: &Api, column: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT {column} FROM jobs ORDER BY created_at, id"
        ))
        .await
}

/// A mail server on a port of its own that refuses every recipient with a permanent reply,
/// which quotes the recipient's address, as many servers' replies do.
async fn refusing_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                write.write_all(b"220 refusing ESMTP\r\n").await.unwrap();
                while let Ok(Some(line)) = lines.next_line().await {
                    let upper = line.to_ascii_uppercase();
                    let reply = if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                        "250 refusing\r\n".to_owned()
                    } else if upper.starts_with("RCPT TO:") {
                        format!("550 5.1.1 {} no such user\r\n", &line[8..])
                    } else if upper.starts_with("QUIT") {
                        // The client may have closed the connection already.
                        write.write_all(b"221 bye\r\n").await.unwrap_or_default();
                        break;
                    } else {
                        "250 ok\r\n".to_owned()
                    };
                    write.write_all(reply.as_bytes()).await.unwrap();
                }
            });
        }
    });
    address
}

#[tokio::test]
async fn mail_001_a_request_only_enqueues_its_email_and_a_job_sends_it() {
    let (api, ada, workspace) = acme(Options::default()).await;
    let invitee = mailbox::address("invitee");

    let response = invite(&api, &ada, &workspace, &invitee).await;

    assert_eq!(response.status, 201);
    assert!(mailbox::to(&invitee).await.is_empty());
    assert_eq!(jobs(&api, "kind").await, ["mail.send"]);
    assert_eq!(api.run_jobs().await, 1);
    assert_eq!(mailbox::to(&invitee).await.len(), 1);
    assert!(jobs(&api, "kind").await.is_empty());
}

#[tokio::test]
async fn mail_001_a_failed_send_is_retried_with_backoff_then_dead_lettered() {
    // Nothing listens on the port the closed listener had.
    let closed = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let options = Options::default().set("SMTP_PORT", closed.port());
    let (api, ada, workspace) = acme(options).await;
    invite(&api, &ada, &workspace, "bob@example.com").await;

    api.run_jobs().await;

    let retried = jobs(
        &api,
        "state::text || ' ' || attempts || ' ' || (run_at > now() + interval '4 seconds')::text \
         || ' ' || failure",
    )
    .await;
    assert_eq!(
        retried,
        ["waiting 1 true internal error: the connection to the mail server failed"]
    );
    api.database()
        .owner_executes("UPDATE jobs SET attempts = 11, run_at = now()")
        .await;
    api.run_jobs().await;
    let dead = jobs(
        &api,
        "state::text || ' ' || attempts || ' ' || (dead_at IS NOT NULL)::text",
    )
    .await;
    assert_eq!(dead, ["dead 12 true"]);
}

#[tokio::test]
async fn mail_001_a_permanent_rejection_dead_letters_at_once_without_the_servers_text() {
    let server = refusing_server().await;
    let options = Options::default().set("SMTP_PORT", server.port());
    let (api, ada, workspace) = acme(options).await;
    invite(&api, &ada, &workspace, "bob@example.com").await;

    api.run_jobs().await;

    let dead = jobs(&api, "state::text || ' ' || attempts || ' ' || failure").await;
    assert_eq!(dead, ["dead 1 internal error: the mail server replied 550"]);
}

#[tokio::test]
async fn mail_002_the_invitation_email_reaches_the_invitee_with_both_parts() {
    let (api, ada, workspace) = acme(Options::default()).await;
    let invitee = mailbox::address("invitee");
    invite(&api, &ada, &workspace, &invitee).await;

    api.run_jobs().await;

    let emails = mailbox::to(&invitee).await;
    assert_eq!(emails.len(), 1);
    let email = &emails[0];
    assert_eq!(
        email.subject,
        "Ada Lovelace invited you to Acme on AntWorker"
    );
    for part in [&email.text, &email.html] {
        assert!(part.contains("Ada Lovelace"), "{part}");
        assert!(part.contains("the workspace"), "{part}");
        assert!(part.contains("as a member"), "{part}");
        assert!(part.contains("expires in 7 days"), "{part}");
        assert!(part.contains(&format!("with {invitee}")), "{part}");
    }
}

#[tokio::test]
async fn mail_004_with_email_off_nothing_is_enqueued_and_the_invitation_is_created() {
    let (api, ada, workspace) = acme(Options::default().set("EMAIL", "off")).await;

    let response = invite(&api, &ada, &workspace, "bob@example.com").await;
    let capabilities = api.get("/v1/capabilities").send().await;

    assert_eq!(response.status, 201);
    assert!(jobs(&api, "kind").await.is_empty());
    let invitations = api
        .database()
        .owner_reads("SELECT email FROM invitations")
        .await;
    assert_eq!(invitations, ["bob@example.com"]);
    assert_eq!(capabilities.json()["features"]["email"], false);
}

#[tokio::test]
async fn mail_004_a_replica_with_email_off_drops_a_job_enqueued_while_email_was_on() {
    let (api, ada, workspace) = acme(Options::default()).await;
    let invitee = mailbox::address("invitee");
    invite(&api, &ada, &workspace, &invitee).await;
    let database = api.database().app().await;
    let clock = Arc::new(TestClock::default());
    let (_, _, off) = application(
        &database,
        Options::default().set("EMAIL", "off"),
        clock.clone(),
        Readiness::new(),
    )
    .await;

    let clock: platform::clock::SharedClock = clock;
    let runs = off.run_pending(&database, &clock).await;

    assert_eq!(runs, 1);
    assert!(jobs(&api, "kind").await.is_empty());
    assert!(mailbox::to(&invitee).await.is_empty());
}

#[tokio::test]
async fn mail_005_in_development_every_email_reaches_the_mail_catcher_on_the_machine() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let example = tokio::fs::read_to_string(format!("{root}/.env.example"))
        .await
        .unwrap();
    let compose = tokio::fs::read_to_string(format!("{root}/compose.yaml"))
        .await
        .unwrap();
    for setting in [
        "EMAIL=smtp",
        "SMTP_HOST=127.0.0.1",
        "SMTP_PORT=1025",
        "SMTP_SECURITY=none",
    ] {
        assert!(example.lines().any(|line| line == setting), "{setting}");
    }
    assert!(compose.contains("\"127.0.0.1:1025:1025\""));
    let (api, ada, workspace) = acme(Options::default()).await;
    let invitee = mailbox::address("invitee");
    invite(&api, &ada, &workspace, &invitee).await;

    api.run_jobs().await;

    assert_eq!(mailbox::to(&invitee).await.len(), 1);
}

#[tokio::test]
async fn mail_007_every_email_comes_from_email_from_with_the_message_id_fixed_when_enqueued() {
    let closed = TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap();
    let (api, ada, workspace) = acme(Options::default()).await;
    let invitee = mailbox::address("invitee");
    invite(&api, &ada, &workspace, &invitee).await;
    let enqueued = jobs(&api, "payload->>'message_id'").await.remove(0);
    // A first attempt that fails, as one whose success went unrecorded would, keeps the job.
    let (_, _, failing) = application(
        &api.database().app().await,
        Options::default().set("SMTP_PORT", closed.port()),
        Arc::new(TestClock::default()),
        Readiness::new(),
    )
    .await;
    let clock: platform::clock::SharedClock = Arc::new(TestClock::default());
    failing
        .run_pending(&api.database().app().await, &clock)
        .await;
    api.database()
        .owner_executes("UPDATE jobs SET run_at = now()")
        .await;

    api.run_jobs().await;

    let emails = mailbox::to(&invitee).await;
    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].from, "noreply@example.com");
    assert_eq!(emails[0].from_name, "AntWorker");
    assert!(enqueued.ends_with("@example.com>"), "{enqueued}");
    assert_eq!(format!("<{}>", emails[0].message_id), enqueued);
}
