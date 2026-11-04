//! Sending email from jobs (the platform spec's Sending email), with an email of the tests' own, which
//! the fixture `POST /v1/mail` sends (TEST-04).

use std::{net::SocketAddr, sync::Arc};

use platform::{
    clock::{SharedClock, TestClock},
    lifecycle::Readiness,
    mail::Mail,
};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

use crate::support::{
    Api, Options,
    api::{application, configuration},
    fixtures, mailbox,
    users::{self, SignedIn},
    workspaces,
};

/// An API with `options`, which also serves `POST /v1/mail` through a `Mail` of the same
/// settings, and Ada, signed in.
async fn api(options: Options) -> (Api, SignedIn) {
    let (config, _) = configuration(&options);
    let mail = Mail::new(config.mail, Arc::new(TestClock::default())).unwrap();
    let api = Api::with(options.with_routes(fixtures::mailing(mail))).await;
    let ada = users::register(&api, "ada@example.com").await;
    (api, ada)
}

/// Ada sends the fixture's email to `to`, which answers 204 once it is enqueued.
async fn send(api: &Api, ada: &SignedIn, to: &str) {
    let response = api
        .post("/v1/mail", &json!({ "to": to, "subject": "the fixture" }))
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(response.status, 204, "{:?}", response.json());
}

/// One column of the jobs table, as text.
async fn jobs(api: &Api, column: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT {column} FROM jobs ORDER BY created_at, id"
        ))
        .await
}

/// A port that nothing listens on: the closed listener's.
async fn closed() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .await
        .unwrap()
        .local_addr()
        .unwrap()
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
async fn platform_021_a_request_only_enqueues_its_email_and_a_job_sends_it() {
    let (api, ada) = api(Options::default()).await;
    let to = mailbox::address("enqueued");

    send(&api, &ada, &to).await;

    assert!(mailbox::to(&to).await.is_empty());
    assert_eq!(jobs(&api, "kind").await, ["platform.send_email"]);
    assert_eq!(api.run_jobs().await, 1);
    let emails = mailbox::to(&to).await;
    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].subject, "the fixture");
    assert_eq!(emails[0].text, "The plain-text part of the fixture.");
    assert!(
        emails[0]
            .html
            .contains("<p>The HTML part of the fixture.</p>"),
        "{}",
        emails[0].html
    );
    assert!(jobs(&api, "kind").await.is_empty());
}

#[tokio::test]
async fn platform_021_a_failed_send_is_retried_with_backoff_then_dead_lettered() {
    let options = Options::default().set("SMTP_PORT", closed().await.port());
    let (api, ada) = api(options).await;
    send(&api, &ada, "bob@example.com").await;

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
async fn platform_021_a_permanent_rejection_dead_letters_at_once_without_the_servers_text() {
    let server = refusing_server().await;
    let (api, ada) = api(Options::default().set("SMTP_PORT", server.port())).await;
    send(&api, &ada, "bob@example.com").await;

    api.run_jobs().await;

    let dead = jobs(&api, "state::text || ' ' || attempts || ' ' || failure").await;
    assert_eq!(dead, ["dead 1 internal error: the mail server replied 550"]);
}

#[tokio::test]
async fn platform_013_a_job_runs_for_its_workspace_scope_after_its_member_loses_their_role() {
    let (api, ada) = api(Options::default()).await;
    let bob = users::register(&api, "bob@example.com").await;
    let db = api.database();
    let workspace = workspaces::arrange(
        db,
        "Acme",
        &[(&ada.user_id, "owner"), (&bob.user_id, "admin")],
    )
    .await;
    let to = mailbox::address("scoped");
    let sent = api
        .post(
            &format!("/v1/workspaces/{workspace}/mail"),
            &json!({ "to": to, "subject": "the scoped fixture" }),
        )
        .bearer(&bob.access)
        .send()
        .await;
    assert_eq!(sent.status, 204);
    let removed = api
        .request(
            http::Method::DELETE,
            &format!("/v1/workspaces/{workspace}/members/{}", bob.user_id),
        )
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(removed.status, 204);

    let scope = jobs(&api, "workspace_id::text || ' ' || user_id::text").await;
    let runs = api.run_jobs().await;

    assert_eq!(scope, [format!("{workspace} {}", bob.user_id)]);
    assert_eq!(runs, 1);
    assert_eq!(mailbox::to(&to).await.len(), 1);
}

#[tokio::test]
async fn platform_022_with_email_off_nothing_is_enqueued_and_the_capabilities_say_so() {
    let (api, ada) = api(Options::default().set("EMAIL", "off")).await;

    send(&api, &ada, "bob@example.com").await;
    let capabilities = api.get("/v1/capabilities").send().await;

    assert!(jobs(&api, "kind").await.is_empty());
    assert_eq!(capabilities.json()["features"]["email"], false);
}

#[tokio::test]
async fn platform_022_a_replica_with_email_off_drops_a_job_enqueued_while_email_was_on() {
    let (api, ada) = api(Options::default()).await;
    let to = mailbox::address("dropped");
    send(&api, &ada, &to).await;
    let database = api.database().app().await;
    let clock = Arc::new(TestClock::default());
    let (_, _, off) = application(
        &database,
        Options::default().set("EMAIL", "off"),
        clock.clone(),
        Readiness::new(),
    )
    .await;

    let clock: SharedClock = clock;
    let runs = off.run_pending(&database, &clock).await;

    assert_eq!(runs, 1);
    assert!(jobs(&api, "kind").await.is_empty());
    assert!(mailbox::to(&to).await.is_empty());
}

#[tokio::test]
async fn platform_023_in_development_every_email_reaches_the_mail_catcher_on_the_machine() {
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
    let (api, ada) = api(Options::default()).await;
    let to = mailbox::address("caught");
    send(&api, &ada, &to).await;

    api.run_jobs().await;

    assert_eq!(mailbox::to(&to).await.len(), 1);
}

#[tokio::test]
async fn platform_025_every_email_comes_from_email_from_with_the_message_id_fixed_when_enqueued() {
    let closed = closed().await;
    let (api, ada) = api(Options::default()).await;
    let to = mailbox::address("identified");
    send(&api, &ada, &to).await;
    let enqueued = jobs(&api, "payload->>'message_id'").await.remove(0);
    // A first attempt that fails, as one whose success went unrecorded would, keeps the job.
    let (_, _, failing) = application(
        &api.database().app().await,
        Options::default().set("SMTP_PORT", closed.port()),
        Arc::new(TestClock::default()),
        Readiness::new(),
    )
    .await;
    let clock: SharedClock = Arc::new(TestClock::default());
    failing
        .run_pending(&api.database().app().await, &clock)
        .await;
    api.database()
        .owner_executes("UPDATE jobs SET run_at = now()")
        .await;

    api.run_jobs().await;

    let emails = mailbox::to(&to).await;
    assert_eq!(emails.len(), 1);
    assert_eq!(emails[0].from, "noreply@example.com");
    assert_eq!(emails[0].from_name, "AntWorker");
    assert!(enqueued.ends_with("@example.com>"), "{enqueued}");
    assert_eq!(format!("<{}>", emails[0].message_id), enqueued);
}

#[tokio::test]
async fn platform_025_a_second_run_of_a_mail_job_sends_a_copy_with_the_same_message_id() {
    let (api, ada) = api(Options::default()).await;
    let to = mailbox::address("copied");
    send(&api, &ada, &to).await;
    // The same job once more, as when a replica stopped after its send and before its success
    // was written.
    api.database()
        .owner_executes(
            "INSERT INTO jobs (kind, payload, user_id, workspace_id) \
             SELECT kind, payload, user_id, workspace_id FROM jobs",
        )
        .await;
    let enqueued = jobs(&api, "payload->>'message_id'").await.remove(0);

    let runs = api.run_jobs().await;

    assert_eq!(runs, 2);
    let emails = mailbox::to(&to).await;
    assert_eq!(emails.len(), 2);
    for email in emails {
        assert_eq!(format!("<{}>", email.message_id), enqueued);
    }
}
