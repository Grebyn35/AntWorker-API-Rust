//! The SMTP transport (architecture §4, the platform spec's Mail): one per process, with a pool
//! of connections, whose TLS verifies the server's certificate against the system's certificates
//! (D38), and whose every send is a client span (OBS-06). The emails and their templates live in
//! `app`'s `mail` module.

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
use tracing::Instrument;

use crate::{
    errors::Internal,
    jobs::JobFailure,
    telemetry::names,
    types::{EmailAddress, Secret},
};

/// How long a connection, or a reply of the server, may take.
const REPLY_TIMEOUT: Duration = Duration::from_secs(10);

/// Whether the deployment sends email, and through which server (MAIL-006).
#[derive(Clone)]
pub enum MailConfig {
    /// No email is sent (MAIL-004).
    Off,
    /// Email is sent through this server.
    Smtp(Smtp),
}

impl std::fmt::Debug for MailConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Off => f.write_str("Off"),
            Self::Smtp(smtp) => write!(f, "Smtp({}:{}, {:?})", smtp.host, smtp.port, smtp.security),
        }
    }
}

/// The mail server, and the sender.
#[derive(Clone)]
pub struct Smtp {
    /// Its host name or IP address.
    pub(crate) host: String,
    /// Its port.
    pub(crate) port: u16,
    /// How the connection is encrypted.
    pub(crate) security: Security,
    /// The user name and password, if the server asks for them.
    pub(crate) credentials: Option<(String, Secret<String>)>,
    /// The sender of every email (MAIL-007).
    pub(crate) from: Mailbox,
}

/// How the connection to the mail server is encrypted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Security {
    /// The connection starts with TLS.
    Tls,
    /// The connection upgrades to TLS before anything else is sent, and is refused when the
    /// server can't.
    StartTls,
    /// No encryption, for a mail catcher on a private network.
    None,
}

impl Security {
    /// The port the setting `SMTP_PORT` defaults to.
    pub(crate) fn default_port(self) -> u32 {
        match self {
            Self::Tls => 465,
            Self::StartTls => 587,
            Self::None => 25,
        }
    }
}

/// An email, rendered, to one recipient.
pub struct Email {
    /// Its recipient.
    pub to: EmailAddress,
    /// Its `Message-ID`, fixed when it was enqueued (MAIL-007).
    pub message_id: String,
    /// Its subject.
    pub subject: String,
    /// Its plain-text part.
    pub text: String,
    /// Its HTML part.
    pub html: String,
}

/// The mail server that the settings name.
pub struct Transport {
    smtp: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
    host: String,
    port: u16,
}

impl Transport {
    /// The transport to the server `smtp` names. It connects only when it sends. With TLS, it
    /// reads the system's certificates once, a blocking read that the platform spec's Mail names
    /// (CODE-05).
    pub fn new(smtp: Smtp) -> Result<Self, Internal> {
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
        let mut builder =
            AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(smtp.host.clone())
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
            host: smtp.host,
            port: smtp.port,
        })
    }

    /// The domain of the sender's address, under which each email's `Message-ID` is made.
    pub fn domain(&self) -> &str {
        self.from.email.domain()
    }

    /// Sends `email` from the sender (MAIL-007), in a client span of its own (OBS-06). A reply
    /// from 500 to 599 fails permanently; any other failure may pass on another attempt
    /// (MAIL-001).
    pub async fn send(&self, email: Email) -> Result<(), JobFailure> {
        let span = tracing::info_span!(
            "smtp",
            { names::OTEL_NAME } = "smtp send",
            { names::OTEL_KIND } = "client",
            { names::SERVER_ADDRESS } = %self.host,
            { names::SERVER_PORT } = self.port,
        );
        async {
            let to: Address = email.to.as_str().parse().map_err(|_| {
                JobFailure::Permanent(Internal::new("the recipient isn't an address SMTP takes"))
            })?;
            let message = lettre::Message::builder()
                .from(self.from.clone())
                .to(Mailbox::new(None, to))
                .subject(email.subject)
                .message_id(Some(email.message_id))
                .multipart(MultiPart::alternative_plain_html(email.text, email.html))
                .map_err(|_| JobFailure::Permanent(Internal::new("the email can't be built")))?;
            self.smtp.send(message).await.map(|_| ()).map_err(failure)
        }
        .instrument(span)
        .await
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
