//! The job `mail.send` (OPS-09): owned by `mail`, triggered by `Mail::send`, which a unit of work
//! that sends an email calls, and touching no table but the queue's own. It renders the email
//! and sends it through the mail server (MAIL-001).
//!
//! A run that sent the email and then stopped, before its success was written, leaves the job
//! to run again, which sends a second copy with the same `Message-ID`, so the receiving system
//! can recognize it (MAIL-007): no protocol sends an email exactly once (OPS-09).

use std::time::Duration;

use platform::{
    errors::Internal,
    jobs::{Duplicate, Job, JobFailure, Jobs},
    mail::Email,
};
use serde::{Deserialize, Serialize};

use super::{model::Message, service::Mail};

/// How long one send may take, its connection and replies included.
const SEND_TIMEOUT: Duration = Duration::from_secs(30);

/// The payload of the job `mail.send`: the email, and the `Message-ID` fixed when it was
/// enqueued (MAIL-007).
#[derive(Serialize, Deserialize)]
pub(super) struct Delivery {
    pub(super) message_id: String,
    pub(super) message: Message,
}

impl Job for Delivery {
    const KIND: &'static str = "mail.send";
    /// Spaces the retries over about 2 to 3.5 hours, so a mail server down for a few hours
    /// delays an email rather than losing it (MAIL-001).
    const MOST_ATTEMPTS: u32 = 12;
    const TIME_LIMIT: Duration = Duration::from_secs(60);
}

impl Mail {
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
        let email = Email {
            to: delivery.message.to().clone(),
            message_id: delivery.message_id,
            subject: rendered.subject,
            text: rendered.text,
            html: rendered.html,
        };
        tokio::select! {
            sent = sender.transport.send(email) => sent,
            () = sender.clock.sleep(SEND_TIMEOUT) => Err(JobFailure::Transient(Internal::new(
                "the send took longer than its timeout",
            ))),
        }
    }
}
