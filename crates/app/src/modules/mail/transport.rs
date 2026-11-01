//! The SMTP transport (the mail spec's Sending): one per process, with a pool of connections,
//! whose TLS verifies the server's certificate against the system's certificates (D38).

use std::time::Duration;

use lettre::{
    Address, AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
    message::{Mailbox, MultiPart},
    transport::smtp::{
        PoolConfig,
        authentication::Credentials,
        client::{CertificateStore, Tls, TlsParameters},
        extension::ClientId,
    },
};
use platform::{errors::Internal, jobs::JobFailure, types::EmailAddress};

use super::{
    message::Rendered,
    settings::{Security, Smtp},
};

/// How long a connection, or a reply of the server, may take.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// The mail server, as the settings name it.
pub(super) struct Transport {
    smtp: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl Transport {
    /// The transport to the server `smtp` names. It connects only when it sends.
    pub(super) fn new(smtp: Smtp) -> Result<Self, Internal> {
        let tls = || {
            TlsParameters::builder(smtp.host.clone())
                .certificate_store(CertificateStore::Default)
                .build_rustls()
                .map_err(Internal::new)
        };
        let tls = match smtp.security {
            Security::Tls => Tls::Wrapper(tls()?),
            Security::StartTls => Tls::Required(tls()?),
            Security::None => Tls::None,
        };
        let mut builder = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(smtp.host)
            .port(smtp.port)
            .tls(tls)
            .timeout(Some(REPLY_TIMEOUT))
            .hello_name(ClientId::Domain(smtp.from.email.domain().to_owned()))
            .pool_config(PoolConfig::new());
        if let Some((username, password)) = smtp.credentials {
            builder = builder.credentials(Credentials::new(username, password.expose().clone()));
        }
        Ok(Self {
            smtp: builder.build(),
            from: smtp.from,
        })
    }

    /// The domain of the sender's address, under which each email's `Message-ID` is made.
    pub(super) fn domain(&self) -> &str {
        self.from.email.domain()
    }

    /// Sends `rendered` to `to`, with `message_id`, from the sender (MAIL-007). A reply from
    /// 500 to 599 fails permanently; any other failure may pass on another attempt (MAIL-001).
    pub(super) async fn send(
        &self,
        message_id: &str,
        to: &EmailAddress,
        rendered: Rendered,
    ) -> Result<(), JobFailure> {
        let to: Address = to.as_str().parse().map_err(|_| {
            JobFailure::Permanent(Internal::new("the recipient isn't an address SMTP takes"))
        })?;
        let email = lettre::Message::builder()
            .from(self.from.clone())
            .to(Mailbox::new(None, to))
            .subject(rendered.subject)
            .message_id(Some(message_id.to_owned()))
            .multipart(MultiPart::alternative_plain_html(
                rendered.text,
                rendered.html,
            ))
            .map_err(|_| JobFailure::Permanent(Internal::new("the email can't be built")))?;
        self.smtp.send(email).await.map(|_| ()).map_err(failure)
    }
}

/// The failure that `error` stands for, holding the reply's code or the failure's kind only:
/// the error's own text may quote the server's reply, which may quote the recipient's address,
/// and neither the logs nor a dead letter may hold it (OBS-04, ERR-04).
fn failure(error: lettre::transport::smtp::Error) -> JobFailure {
    if let Some(code) = error.status() {
        let failure = Internal::new(format!("the mail server replied {code}"));
        return if error.is_permanent() {
            JobFailure::Permanent(failure)
        } else {
            JobFailure::Transient(failure)
        };
    }
    let kind = if error.is_timeout() {
        "the mail server didn't answer in time"
    } else if error.is_tls() {
        "TLS with the mail server failed"
    } else if error.is_transport_shutdown() {
        "the transport to the mail server shut down"
    } else {
        "the connection to the mail server failed"
    };
    JobFailure::Transient(Internal::new(kind))
}
