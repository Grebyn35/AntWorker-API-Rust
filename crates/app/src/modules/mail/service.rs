//! Sending email: a request enqueues it, and the job `mail.send` renders and sends it, never
//! inside a request (MAIL-001, D1).

use std::{sync::Arc, time::Duration};

use platform::{
    clock::SharedClock,
    db::UnitOfWork,
    errors::Internal,
    jobs::{Duplicate, Job, JobFailure, JobScope, Jobs},
    random,
};
use serde::{Deserialize, Serialize};

use super::{error::MailError, message::Message, settings::Settings, transport::Transport};

/// How long one send may take, its connection and replies included.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// The emails of the deployment: sent through its mail server, or none with email off
/// (MAIL-004). Clones share the transport.
#[derive(Clone)]
pub(crate) struct Mail {
    sender: Option<Arc<Sender>>,
}

/// The transport, and the clock that bounds each send.
struct Sender {
    transport: Transport,
    clock: SharedClock,
}

/// The payload of the job `mail.send`: the email, and the `Message-ID` fixed when it was
/// enqueued, so an email that a retry delivers twice arrives with the same one (MAIL-007).
#[derive(Serialize, Deserialize)]
struct Delivery {
    message_id: String,
    message: Message,
}

impl Job for Delivery {
    const KIND: &'static str = "mail.send";
    /// Spaces the retries over about 2 to 3.5 hours, so a mail server down for a few hours
    /// delays an email rather than losing it (MAIL-001).
    const MOST_ATTEMPTS: u32 = 12;
    const TIME_LIMIT: Duration = Duration::from_secs(60);
}

impl Mail {
    /// The emails that `settings` describe, each send bounded by `clock`.
    pub(crate) fn new(settings: Settings, clock: SharedClock) -> Result<Self, Internal> {
        let sender = match settings {
            Settings::Off => None,
            Settings::Smtp(smtp) => Some(Arc::new(Sender {
                transport: Transport::new(smtp)?,
                clock,
            })),
        };
        Ok(Self { sender })
    }

    /// Whether the deployment sends email, which the capabilities endpoint reports
    /// (SYSTEM-001).
    pub(crate) fn is_on(&self) -> bool {
        self.sender.is_some()
    }

    /// Enqueues `message` in `uow`, to be sent for `scope` once `uow` commits, or does nothing
    /// with email off (MAIL-004).
    pub(crate) async fn send(
        &self,
        uow: &mut UnitOfWork,
        scope: JobScope,
        message: Message,
    ) -> Result<(), MailError> {
        let Some(sender) = &self.sender else {
            return Ok(());
        };
        let id = uuid::Builder::from_random_bytes(random::bytes::<16>()?).into_uuid();
        let delivery = Delivery {
            message_id: format!("<{id}@{}>", sender.transport.domain()),
            message,
        };
        Ok(uow.enqueue(scope, &delivery).await?)
    }

    /// Registers the job that sends what [`Mail::send`] enqueued.
    pub(crate) fn register(&self, jobs: &mut Jobs) -> Result<(), Duplicate> {
        let mail = self.clone();
        jobs.queued(move |_, delivery: Delivery| {
            let mail = mail.clone();
            async move { mail.deliver(delivery).await }
        })
    }

    /// Renders and sends `delivery`. A replica with email off drops it, as one enqueued while
    /// email was on (MAIL-004).
    async fn deliver(&self, delivery: Delivery) -> Result<(), JobFailure> {
        let Some(sender) = &self.sender else {
            tracing::debug!("email is off, so an email enqueued while it was on is dropped");
            return Ok(());
        };
        let rendered = delivery.message.render()?;
        let to = delivery.message.to();
        tokio::select! {
            sent = sender.transport.send(&delivery.message_id, to, rendered) => sent,
            () = sender.clock.sleep(SEND_TIMEOUT) => Err(JobFailure::Transient(Internal::new(
                "the send took longer than its timeout",
            ))),
        }
    }
}
