//! Sending email (architecture §4, the platform spec's Sending email). A module renders each email it
//! declares in its `emails.rs` into an [`Email`], [`Mail::send`] enqueues it in the module's unit
//! of work, and the job `platform.send_email` sends it through the mail server, never inside a request
//! (PLATFORM-021, D1).
//!
//! The job `platform.send_email` (OPS-09) is owned by this area, triggered by `Mail::send`, and touches no
//! table but the queue's own. A run that sent the email and then stopped, before its success was written,
//! leaves the job to run again, which sends a second copy with the same `Message-ID`, so the
//! receiving system can recognize it (PLATFORM-025): no protocol sends an email exactly once.
//!
//! An email that holds a secret that no row may keep, such as a verification code, is sent instead
//! by [`Mail::send_now`], from the run of the module's own job that makes the secret, so the queue
//! never holds it (SEC-03).

mod transport;

use std::{
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use self::transport::Transport;
pub use self::transport::{Security, Smtp};
use crate::{
    clock::SharedClock,
    db::UnitOfWork,
    errors::Internal,
    jobs::{Duplicate, Job, JobFailure, JobScope, Jobs},
    random,
    types::EmailAddress,
};

/// How long one send may take, its connection and replies included.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// Whether the deployment sends email, and through which server (PLATFORM-024).
#[derive(Clone)]
pub enum MailConfig {
    /// No email is sent (PLATFORM-022).
    Off,
    /// Email is sent through this server.
    Smtp(Smtp),
}

impl fmt::Debug for MailConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Off => f.write_str("Off"),
            Self::Smtp(smtp) => write!(f, "Smtp({}:{}, {:?})", smtp.host, smtp.port, smtp.security),
        }
    }
}

/// An email, rendered, to one recipient: what a module's `emails.rs` makes of an email it
/// declares, and hands [`Mail::send`]. It is part of a job's payload, so it changes only by
/// adding fields that may be absent (DATA-10, the platform spec's "The job queue").
#[derive(Clone, Serialize, Deserialize)]
pub struct Email {
    /// Its recipient.
    pub to: EmailAddress,
    /// Its subject.
    pub subject: String,
    /// Its plain-text part.
    pub text: String,
    /// Its HTML part.
    pub html: String,
}

impl fmt::Debug for Email {
    /// Shows none of the values: each is personal data, or may hold some (OBS-04).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Email([redacted])")
    }
}

/// The emails of the deployment: sent through its mail server, or none with email off
/// (PLATFORM-022). Clones share the transport.
#[derive(Clone)]
pub struct Mail {
    sender: Option<Arc<Sender>>,
    /// Where the emails go in place of the mail server, for the seed (the system spec's Seed).
    inbox: Option<Inbox>,
}

/// The emails that a [`Mail`] from [`Mail::with_inbox`] sent at once, in place of its mail server:
/// how the seed reads an email that a request of its own sent, as a client reads its mailbox (the
/// system spec's Seed). Clones share the emails.
#[derive(Clone, Default)]
pub struct Inbox(Arc<Mutex<Vec<Email>>>);

impl Inbox {
    /// The emails delivered to `to`, the oldest first.
    pub fn to(&self, to: &EmailAddress) -> Result<Vec<Email>, Internal> {
        let emails = self
            .0
            .lock()
            .map_err(|_| Internal::new("a thread panicked while it held the inbox"))?;
        Ok(emails
            .iter()
            .filter(|email| email.to.as_str() == to.as_str())
            .cloned()
            .collect())
    }

    /// Delivers `email` here.
    fn deliver(&self, email: Email) -> Result<(), JobFailure> {
        self.0
            .lock()
            .map_err(|_| {
                JobFailure::Permanent(Internal::new("a thread panicked while it held the inbox"))
            })?
            .push(email);
        Ok(())
    }
}

/// The transport, and the clock that bounds each send.
struct Sender {
    transport: Transport,
    clock: SharedClock,
}

/// The payload of the job `platform.send_email`: the email as rendered, so a job that one release enqueued
/// sends the same email when a replica of another runs it, and the `Message-ID` fixed when it was
/// enqueued (PLATFORM-025).
#[derive(Serialize, Deserialize)]
struct Delivery {
    message_id: String,
    email: Email,
}

impl Job for Delivery {
    const KIND: &'static str = "platform.send_email";
    /// The platform spec's Sending email says why 12 (PLATFORM-021).
    const MOST_ATTEMPTS: u32 = 12;
    const TIME_LIMIT: Duration = Duration::from_secs(60);
}

impl Mail {
    /// The emails that `config` describes, each send bounded by `clock`. With TLS, building the
    /// transport reads the system's certificates once (CODE-05).
    pub fn new(config: MailConfig, clock: SharedClock) -> Result<Self, Internal> {
        let sender = match config {
            MailConfig::Off => None,
            MailConfig::Smtp(smtp) => Some(Arc::new(Sender {
                transport: Transport::new(smtp)?,
                clock,
            })),
        };
        Ok(Self {
            sender,
            inbox: None,
        })
    }

    /// The same emails, but that each one [`Mail::send_now`] sends goes to the returned inbox
    /// rather than the mail server: the seed's, which composes the API in process and reads the
    /// code that a registration's job sends (the system spec's Seed). What [`Mail::send`]
    /// enqueued still goes to the mail server. Only the seed calls it; clippy bans it elsewhere.
    #[must_use]
    pub fn with_inbox(self) -> (Self, Inbox) {
        let inbox = Inbox::default();
        let mail = Self {
            sender: self.sender,
            inbox: Some(inbox.clone()),
        };
        (mail, inbox)
    }

    /// Whether the deployment sends email, which the capabilities endpoint reports
    /// (PLATFORM-022).
    pub fn is_on(&self) -> bool {
        self.sender.is_some()
    }

    /// Enqueues `email` in `uow`, to be sent for `scope` once `uow` commits (PLATFORM-021), or
    /// does nothing with email off (PLATFORM-022).
    pub async fn send(
        &self,
        uow: &mut UnitOfWork,
        scope: JobScope,
        email: Email,
    ) -> Result<(), Internal> {
        let Some(sender) = &self.sender else {
            return Ok(());
        };
        let id = uuid::Builder::from_random_bytes(random::bytes::<16>()?).into_uuid();
        let delivery = Delivery {
            message_id: format!("<{id}@{}>", sender.transport.domain()),
            email,
        };
        uow.enqueue(scope, &delivery).await
    }

    /// Registers the job that sends what [`Mail::send`] enqueued.
    pub fn register(&self, jobs: &mut Jobs) -> Result<(), Duplicate> {
        let mail = self.clone();
        jobs.queued(move |_, delivery: Delivery| {
            let mail = mail.clone();
            async move { mail.deliver(delivery).await }
        })
    }

    /// Sends `delivery`. A replica with email off drops it, as one enqueued while email was on
    /// (PLATFORM-022).
    async fn deliver(&self, delivery: Delivery) -> Result<(), JobFailure> {
        let Some(sender) = &self.sender else {
            tracing::debug!("email is off, so an email enqueued while it was on is dropped");
            return Ok(());
        };
        tokio::select! {
            sent = sender.transport.send(delivery.message_id, delivery.email) => sent,
            () = sender.clock.sleep(SEND_TIMEOUT) => Err(JobFailure::Transient(Internal::new(
                "the send took longer than its timeout",
            ))),
        }
    }

    /// Sends `email` at once, with a new `Message-ID`, from the run of a job that made a secret
    /// the email holds, such as a verification code, so no job's payload holds it (SEC-03); or
    /// sends nothing with email off (PLATFORM-022). A failure tells the job whether its next
    /// attempt may succeed, as `platform.send_email`'s does (PLATFORM-021). Only a job's handler
    /// calls it, since no request waits on the mail server; clippy bans it elsewhere.
    pub async fn send_now(&self, email: Email) -> Result<(), JobFailure> {
        let Some(sender) = &self.sender else {
            return Ok(());
        };
        let id =
            uuid::Builder::from_random_bytes(random::bytes::<16>().map_err(JobFailure::Transient)?)
                .into_uuid();
        if let Some(inbox) = &self.inbox {
            return inbox.deliver(email);
        }
        let delivery = Delivery {
            message_id: format!("<{id}@{}>", sender.transport.domain()),
            email,
        };
        self.deliver(delivery).await
    }
}
