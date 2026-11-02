//! Sending email (architecture §4, the platform spec's Sending email). A module renders each email it
//! declares in its `emails.rs` into an [`Email`], [`Mail::send`] enqueues it in the module's unit
//! of work, and the job `platform.send_email` sends it through the mail server, never inside a request
//! (PLATFORM-066, D1).
//!
//! The job `platform.send_email` (OPS-09) is owned by this area, triggered by `Mail::send`, and touches no
//! table but the queue's own. A run that sent the email and then stopped, before its success was written,
//! leaves the job to run again, which sends a second copy with the same `Message-ID`, so the
//! receiving system can recognize it (PLATFORM-070): no protocol sends an email exactly once.

mod transport;

use std::{fmt, sync::Arc, time::Duration};

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

/// Whether the deployment sends email, and through which server (PLATFORM-069).
#[derive(Clone)]
pub enum MailConfig {
    /// No email is sent (PLATFORM-067).
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
/// adding fields that may be absent (the platform spec's Jobs).
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
/// (PLATFORM-067). Clones share the transport.
#[derive(Clone)]
pub struct Mail {
    sender: Option<Arc<Sender>>,
}

/// The transport, and the clock that bounds each send.
struct Sender {
    transport: Transport,
    clock: SharedClock,
}

/// The payload of the job `platform.send_email`: the email as rendered, so a job that one release enqueued
/// sends the same email when a replica of another runs it, and the `Message-ID` fixed when it was
/// enqueued (PLATFORM-070).
#[derive(Serialize, Deserialize)]
struct Delivery {
    message_id: String,
    email: Email,
}

impl Job for Delivery {
    const KIND: &'static str = "platform.send_email";
    /// The platform spec's Sending email says why 12 (PLATFORM-066).
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
        Ok(Self { sender })
    }

    /// Whether the deployment sends email, which the capabilities endpoint reports
    /// (PLATFORM-067).
    pub fn is_on(&self) -> bool {
        self.sender.is_some()
    }

    /// Enqueues `email` in `uow`, to be sent for `scope` once `uow` commits (PLATFORM-066), or
    /// does nothing with email off (PLATFORM-067).
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
    /// (PLATFORM-067).
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
}
