//! Sending email from jobs (the platform spec's Sending email), with an email of the tests' own, which
//! the fixture `POST /v1/mail` sends (TEST-04).

use std::{net::SocketAddr, sync::Arc, time::Duration};

use platform::{
    clock::{SharedClock, TestClock},
    jobs::JobFailure,
    lifecycle::Readiness,
    mail::{Email, Mail},
    telemetry::testing::{self, collector::Collector},
    types::EmailAddress,
};
use serde_json::json;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::TcpListener,
};

use crate::support::{
    self, Api, Options, TestDatabase,
    api::{SERVICE, application, configuration},
    counts, fixtures, mailbox,
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

/// One column of the jobs table of `db`, as text.
async fn jobs(db: &TestDatabase, column: &str) -> Vec<String> {
    db.owner_reads(&format!(
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
    scripted_server("220 refusing ESMTP", |_| "550 5.1.1").await
}

/// A mail server on a port of its own that greets with `greeting`, offers no extension, such as
/// STARTTLS, and answers each recipient with what `refusal` gives for the recipient as the
/// command names it, such as `<ada@example.com>`: a reply's code and its enhanced code, which the
/// recipient follows.
async fn scripted_server(greeting: &'static str, refusal: fn(&str) -> &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                write
                    .write_all(format!("{greeting}\r\n").as_bytes())
                    .await
                    .unwrap();
                while let Ok(Some(line)) = lines.next_line().await {
                    let upper = line.to_ascii_uppercase();
                    let reply = if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                        "250 scripted\r\n".to_owned()
                    } else if upper.starts_with("RCPT TO:") {
                        let recipient = &line[8..];
                        format!("{} {recipient} no such user\r\n", refusal(recipient))
                    } else if upper.starts_with("QUIT") {
                        // The client may have closed the connection already.
                        if let Err(gone) = write.write_all(b"221 bye\r\n").await {
                            platform::errors::ignore(&gone, "test_client_gone");
                        }
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
    assert_eq!(jobs(api.database(), "kind").await, ["platform.send_email"]);
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
    assert!(jobs(api.database(), "kind").await.is_empty());
}

#[tokio::test]
async fn platform_021_a_failed_send_is_retried_with_backoff_then_dead_lettered() {
    let options = Options::default().set("SMTP_PORT", closed().await.port());
    let (api, ada) = api(options).await;
    send(&api, &ada, "bob@example.com").await;

    api.run_jobs().await;

    let retried = jobs(
        api.database(),
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
        api.database(),
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

    let dead = jobs(
        api.database(),
        "state::text || ' ' || attempts || ' ' || failure",
    )
    .await;
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

    let scope = jobs(api.database(), "workspace_id::text || ' ' || user_id::text").await;
    let runs = api.run_jobs().await;

    assert_eq!(scope, [format!("{workspace} {}", bob.user_id)]);
    assert_eq!(runs, 1);
    assert_eq!(mailbox::to(&to).await.len(), 1);
}

#[tokio::test]
async fn platform_022_with_email_off_nothing_is_enqueued_and_the_capabilities_say_so() {
    let (api, ada) = api(Options::email_off()).await;

    send(&api, &ada, "bob@example.com").await;
    let capabilities = api.get("/v1/capabilities").send().await;

    assert!(jobs(api.database(), "kind").await.is_empty());
    assert_eq!(capabilities.json()["features"]["email"], false);
}

#[tokio::test]
async fn platform_022_a_replica_with_email_off_drops_a_job_enqueued_while_email_was_on() {
    let (api, ada) = api(Options::default()).await;
    let to = mailbox::address("dropped");
    send(&api, &ada, &to).await;
    let database = api.database().app().await;
    let clock = Arc::new(TestClock::default());
    let off = application(
        &database,
        Options::email_off(),
        clock.clone(),
        Readiness::new(),
    )
    .await
    .jobs;

    let clock: SharedClock = clock;
    let runs = off.run_pending(&database, &clock).await;

    assert_eq!(runs, 1);
    assert!(jobs(api.database(), "kind").await.is_empty());
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
    let enqueued = jobs(api.database(), "payload->>'message_id'")
        .await
        .remove(0);
    // A first attempt that fails, as one whose success went unrecorded would, keeps the job.
    let failing = application(
        &api.database().app().await,
        Options::default().set("SMTP_PORT", closed.port()),
        Arc::new(TestClock::default()),
        Readiness::new(),
    )
    .await
    .jobs;
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
    let enqueued = jobs(api.database(), "payload->>'message_id'")
        .await
        .remove(0);

    let runs = api.run_jobs().await;

    assert_eq!(runs, 2);
    let emails = mailbox::to(&to).await;
    assert_eq!(emails.len(), 2);
    for email in emails {
        assert_eq!(format!("<{}>", email.message_id), enqueued);
    }
}

/// A `Mail` of `options`'s settings, as a module's job holds one, on a clock that never moves.
fn mail(options: &Options) -> Mail {
    mail_on(options, Arc::new(TestClock::default()))
}

/// A `Mail` of `options`'s settings, whose sends `clock` bounds, with the tests' telemetry
/// installed, which counts its sends (PLATFORM-105).
fn mail_on(options: &Options, clock: Arc<TestClock>) -> Mail {
    testing::install(SERVICE).unwrap();
    let (config, _) = configuration(options);
    Mail::new(config.mail, clock).unwrap()
}

/// An email of the tests' own to `to`, which holds a secret, as one that `send_now` sends does.
fn secret_email(to: &str) -> Email {
    Email {
        to: EmailAddress::parse(to).unwrap(),
        subject: "The fixture's secret".to_owned(),
        text: "The secret: 424242.".to_owned(),
        html: "<p>The secret: 424242.</p>".to_owned(),
    }
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test of send_now calls it, as a job's handler does (PLATFORM-021)"
)]
async fn platform_021_send_now_sends_at_once_and_tells_its_job_whether_to_retry() {
    let to = mailbox::address("secret");

    mail(&Options::default())
        .send_now(secret_email(&to))
        .await
        .unwrap();
    let unreachable = Options::default().set("SMTP_PORT", closed().await.port());
    let failed = mail(&unreachable).send_now(secret_email(&to)).await;
    let refusing = Options::default().set("SMTP_PORT", refusing_server().await.port());
    let refused = mail(&refusing).send_now(secret_email(&to)).await;

    // Sent at once, with no job in between, and no queue that holds the secret.
    assert_eq!(mailbox::to(&to).await.len(), 1);
    // A failure that another attempt may not meet retries its job; a permanent rejection
    // dead-letters it at once.
    assert!(
        matches!(failed, Err(JobFailure::Transient(_))),
        "{failed:?}"
    );
    assert!(
        matches!(refused, Err(JobFailure::Permanent(_))),
        "{refused:?}"
    );
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test of send_now calls it, as a job's handler does (PLATFORM-021)"
)]
async fn platform_021_a_recipient_that_isn_t_an_address_smtp_takes_fails_send_now_for_good() {
    // An address that no account could hold, as a row that a fault wrote would hold it.
    let email = Email {
        to: EmailAddress::from_stored("no address".to_owned()),
        ..secret_email(&mailbox::address("unsent"))
    };

    let sent = mail(&Options::default()).send_now(email).await;

    assert!(matches!(sent, Err(JobFailure::Permanent(_))), "{sent:?}");
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test of send_now calls it, as a job's handler does (PLATFORM-022)"
)]
async fn platform_022_with_email_off_send_now_sends_nothing() {
    let to = mailbox::address("unsent");

    let sent = mail(&Options::email_off())
        .send_now(secret_email(&to))
        .await;

    assert!(sent.is_ok(), "{sent:?}");
    assert!(mailbox::to(&to).await.is_empty());
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test of send_now calls it, as a job's handler does (PLATFORM-021)"
)]
async fn platform_025_each_email_that_send_now_sends_carries_a_message_id_of_its_own() {
    let to = mailbox::address("each");
    let mail = mail(&Options::default());

    mail.send_now(secret_email(&to)).await.unwrap();
    mail.send_now(secret_email(&to)).await.unwrap();

    let emails = mailbox::to(&to).await;
    assert_eq!(emails.len(), 2);
    assert_ne!(emails[0].message_id, emails[1].message_id);
    for email in &emails {
        assert_eq!(email.from, "noreply@example.com");
        assert!(
            email.message_id.ends_with("@example.com"),
            "{}",
            email.message_id
        );
    }
}

/// How many sends to the mail server ended as `outcome`, in every test that runs alongside.
fn sends(outcome: &str) -> f64 {
    testing::recorded("platform.mail_sends")
        .into_iter()
        .filter(|point| point.labels.get("mail.outcome").map(String::as_str) == Some(outcome))
        .map(|point| point.value)
        .sum()
}

/// A mail server on a port of its own that takes each connection, and never answers on it.
async fn silent_server() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    address
}

/// Enqueues an email of the tests' own to `to`, as a job of `platform.send_email`, through the
/// `owner` connection, for a `serve` on `db` to send.
async fn enqueue(db: &TestDatabase, to: &str) {
    let payload = json!({
        "message_id": "<counted@example.com>",
        "email": {
            "to": to,
            "subject": "Counted",
            "text": "A counted send.",
            "html": "<p>A counted send.</p>",
        },
    });
    support::jobs::enqueued(
        db,
        "platform.send_email",
        &payload,
        "00-6f2d0c4e8a1b3c5d7e9f0a2b4c6d8e0f-00f067aa0ba902b7-01",
    )
    .await;
}

/// What a `serve` on `db` counted of its sends, by how each ended, such as `sent 1` (TEST-07),
/// with `env` in place of the settings of the mail server that it names. It exports to a
/// collector of its own, and stops once the collector holds the span of a send for each job that
/// `db` held, which ends once the send is counted. A failure that its job retries runs again 5
/// seconds later at the soonest (PLATFORM-014), and the server claims no job once it is told to
/// stop, and lets the runs in flight end (PLATFORM-018).
async fn sends_counted(db: &TestDatabase, env: &[(&str, &str)]) -> Vec<String> {
    let jobs: usize = db.owner_reads("SELECT count(*)::text FROM jobs").await[0]
        .parse()
        .unwrap();
    let collector = Collector::start().await.unwrap();
    let server = db.exporting_to(&collector, env).await;
    collector
        .wait_for(|received| {
            let sends = received.spans().into_iter();
            sends.filter(|(_, span)| span.name == "smtp send").count() >= jobs
        })
        .await;
    server.stop().await;
    counts::of(&collector.received(), "platform.mail_sends", "mail.outcome")
}

#[tokio::test]
async fn platform_105_a_send_the_server_takes_counts_as_sent_by_a_job_and_by_send_now() {
    let api = Api::new().await;
    let to = mailbox::address("counted");
    // A registration enqueues the job that sends its first code with `send_now` (AUTH-027).
    users::register_unverified(&api, &to).await;
    enqueue(api.database(), &to).await;

    let counted = sends_counted(api.database(), &[]).await;

    assert_eq!(mailbox::to(&to).await.len(), 2);
    assert_eq!(counted, ["sent 2"]);
}

#[tokio::test]
async fn platform_105_a_reply_from_400_to_599_but_421_counts_as_refused() {
    // It defers the recipient whose address starts with `deferred`, and rejects the other.
    let server = scripted_server("220 refusing ESMTP", |recipient| {
        if recipient.starts_with("<deferred") {
            "451 4.3.0"
        } else {
            "550 5.1.1"
        }
    })
    .await;
    let db = TestDatabase::migrated().await;
    enqueue(&db, &mailbox::address("rejected")).await;
    enqueue(&db, &mailbox::address("deferred")).await;
    let port = server.port().to_string();

    let counted = sends_counted(&db, &[("SMTP_PORT", &port)]).await;

    // The rejection dead-letters its job, and the deferral retries its own (PLATFORM-021).
    assert_eq!(
        jobs(&db, "state::text || ' ' || attempts || ' ' || failure").await,
        [
            "dead 1 internal error: the mail server replied 550",
            "waiting 1 internal error: the mail server replied 451"
        ]
    );
    assert_eq!(counted, ["refused 2"]);
}

#[tokio::test]
async fn platform_105_a_send_that_no_server_serves_counts_as_unavailable() {
    let closed = closed().await.port().to_string();
    let down = scripted_server("421 4.3.2 the service isn't available", |_| "550 5.1.1")
        .await
        .port()
        .to_string();
    let plain = refusing_server().await.port().to_string();
    // The settings of each `serve`, which reads one mail server.
    let servers = [
        vec![("SMTP_PORT", closed.as_str())],
        vec![("SMTP_PORT", down.as_str())],
        vec![("SMTP_PORT", plain.as_str()), ("SMTP_SECURITY", "starttls")],
    ];
    let mut databases = Vec::new();
    for _ in &servers {
        let db = TestDatabase::migrated().await;
        enqueue(&db, &mailbox::address("unavailable")).await;
        databases.push(db);
    }

    let counted = futures_util::future::join_all(
        databases
            .iter()
            .zip(&servers)
            .map(|(db, env)| sends_counted(db, env)),
    )
    .await;

    for db in &databases {
        // Each failure retries its job (PLATFORM-021).
        assert_eq!(
            jobs(db, "state::text || ' ' || attempts").await,
            ["waiting 1"]
        );
    }
    assert_eq!(
        counted,
        [["unavailable 1"], ["unavailable 1"], ["unavailable 1"]]
    );
}

#[tokio::test]
#[expect(
    clippy::disallowed_methods,
    reason = "the test of send_now calls it, as a job's handler does (PLATFORM-105)"
)]
async fn platform_105_a_send_that_takes_longer_than_its_30_seconds_counts_as_unavailable() {
    let silent = Options::default().set("SMTP_PORT", silent_server().await.port());
    let clock = Arc::new(TestClock::default());
    let mail = mail_on(&silent, clock.clone());
    let unavailable = sends("unavailable");

    let (sent, ()) = tokio::join!(
        mail.send_now(secret_email(&mailbox::address("unanswered"))),
        async {
            clock.sleeping(Duration::from_secs(30)).await;
            clock.advance(Duration::from_secs(30));
        }
    );

    let failure = format!("{sent:?}");
    assert!(
        failure.contains("the send took longer than its timeout"),
        "{failure}"
    );
    // The send's 30 seconds pass on the clock that its `Mail` is given, which a test moves only
    // for a `Mail` of its own process: a `serve` waits on the system's clock, which no test moves
    // (TEST-06). Other tests send too, so the count grows by at least this one (TEST-07).
    let after = sends("unavailable");
    assert!(after >= unavailable + 1.0, "{after} after {unavailable}");
}

#[tokio::test]
async fn sec_20_the_mail_library_encodes_a_header_s_value_whatever_it_holds() {
    let (api, ada) = api(Options::default()).await;
    let to = mailbox::address("encoded");
    let subject = "Zo\u{eb}'s caf\u{e9}\r\nBcc: eve@example.com";
    let response = api
        .post("/v1/mail", &json!({ "to": to, "subject": subject }))
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(response.status, 204, "{:?}", response.json());

    api.run_jobs().await;

    let raw = mailbox::raw_to(&to).await;
    assert_eq!(raw.len(), 1);
    let (head, _) = raw[0].split_once("\r\n\r\n").unwrap();
    assert!(head.is_ascii(), "{head}");
    assert!(
        !head
            .lines()
            .any(|line| line.to_ascii_lowercase().starts_with("bcc:")),
        "{head}"
    );
    let header = subject_of(head);
    assert!(header.contains("=?utf-8?"), "an encoded word: {header}");
    assert_eq!(mailbox::to(&to).await[0].subject, subject, "{header}");
}

/// The `Subject` header of `head`, a message's header lines, with its folded lines.
fn subject_of(head: &str) -> String {
    let mut lines = head
        .split("\r\n")
        .skip_while(|line| !line.starts_with("Subject:"));
    let mut header = lines.next().expect("a Subject").to_owned();
    for folded in lines.take_while(|line| line.starts_with([' ', '\t'])) {
        header.push_str(folded);
    }
    header
}
