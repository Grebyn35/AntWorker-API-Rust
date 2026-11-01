//! Sending email: a request enqueues it, and the job `mail.send` sends it, never inside a request
//! (MAIL-001, D1).

use std::sync::Arc;

use platform::{
    clock::SharedClock,
    db::UnitOfWork,
    errors::Internal,
    jobs::JobScope,
    mail::{MailConfig, Transport},
    random,
};

use super::{error::MailError, jobs::Delivery, model::Message};

/// The emails of the deployment: sent through its mail server, or none with email off
/// (MAIL-004). Clones share the transport.
#[derive(Clone)]
pub(crate) struct Mail {
    pub(super) sender: Option<Arc<Sender>>,
}

/// The transport, and the clock that bounds each send.
pub(super) struct Sender {
    pub(super) transport: Transport,
    pub(super) clock: SharedClock,
}

impl Mail {
    /// The emails that `config` describes, each send bounded by `clock`.
    pub(crate) fn new(config: MailConfig, clock: SharedClock) -> Result<Self, Internal> {
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
}
