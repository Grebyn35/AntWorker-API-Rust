//! The email settings (MAIL-006, the mail spec's Settings).

use std::{ffi::OsString, io, path::Path};

use lettre::message::Mailbox;
use platform::{
    config::{ConfigError, Settings as Source},
    types::Secret,
};

/// Whether the deployment sends email, and through which server.
pub(crate) enum Settings {
    /// No email is sent (MAIL-004).
    Off,
    /// Email is sent through this server.
    Smtp(Smtp),
}

/// The mail server, and the sender.
pub(crate) struct Smtp {
    /// Its host name or IP address.
    pub(super) host: String,
    /// Its port.
    pub(super) port: u16,
    /// How the connection is encrypted.
    pub(super) security: Security,
    /// The user name and password, if the server asks for them.
    pub(super) credentials: Option<(String, Secret<String>)>,
    /// The sender of every email (MAIL-007).
    pub(super) from: Mailbox,
}

/// How the connection to the mail server is encrypted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Security {
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
    fn default_port(self) -> u32 {
        match self {
            Self::Tls => 465,
            Self::StartTls => 587,
            Self::None => 25,
        }
    }
}

impl Settings {
    /// The email settings from `settings`. With `EMAIL=off`, no other email setting is read.
    pub(crate) fn read<E, F>(settings: &Source<E, F>) -> Result<Self, ConfigError>
    where
        E: Fn(&str) -> Option<OsString>,
        F: Fn(&Path) -> io::Result<String>,
    {
        let on = settings.with("EMAIL", "smtp or off", |value| match value {
            "smtp" => Some(true),
            "off" => Some(false),
            _ => None,
        })?;
        if !on {
            return Ok(Self::Off);
        }
        let host = settings.with("SMTP_HOST", "a host name or an IP address", |value| {
            url::Host::parse(value).ok().map(|_| value.to_owned())
        })?;
        let security = settings
            .optional_with(
                "SMTP_SECURITY",
                "tls, starttls or none",
                |value| match value {
                    "tls" => Some(Security::Tls),
                    "starttls" => Some(Security::StartTls),
                    "none" => Some(Security::None),
                    _ => None,
                },
            )?
            .unwrap_or(Security::StartTls);
        let port = settings.number("SMTP_PORT", security.default_port(), 1..=65_535)?;
        let port = u16::try_from(port)
            .map_err(|_| ConfigError::invalid("SMTP_PORT", "a port from 1 to 65535"))?;
        let username =
            settings.optional_with("SMTP_USERNAME", "text", |value| Some(value.to_owned()))?;
        let password = settings.optional_with("SMTP_PASSWORD", "text", |value| {
            Some(Secret::new(value.to_owned()))
        })?;
        let credentials = match (username, password) {
            (Some(username), Some(password)) => Some((username, password)),
            (None, None) => None,
            (Some(_), None) => {
                return Err(ConfigError::invalid(
                    "SMTP_PASSWORD",
                    "set when SMTP_USERNAME is",
                ));
            }
            (None, Some(_)) => {
                return Err(ConfigError::invalid(
                    "SMTP_USERNAME",
                    "set when SMTP_PASSWORD is",
                ));
            }
        };
        if credentials.is_some() && security == Security::None {
            return Err(ConfigError::invalid(
                "SMTP_SECURITY",
                "tls or starttls while SMTP_USERNAME is set, so no password crosses the network \
                 unencrypted",
            ));
        }
        let from = settings.with(
            "EMAIL_FROM",
            "a name and an address, such as AntWorker <noreply@example.com>",
            |value| value.parse::<Mailbox>().ok(),
        )?;
        Ok(Self::Smtp(Smtp {
            host,
            port,
            security,
            credentials,
            from,
        }))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// The email settings read from `values`, or the message of their refusal.
    fn read(values: &[(&str, &str)]) -> Result<Settings, String> {
        let env: HashMap<String, String> = values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let settings = Source::new(
            |name: &str| env.get(name).map(OsString::from),
            |_: &Path| Err(io::Error::other("no files")),
        );
        Settings::read(&settings).map_err(|error| error.to_string())
    }

    const SMTP: [(&str, &str); 3] = [
        ("EMAIL", "smtp"),
        ("SMTP_HOST", "smtp.example.com"),
        ("EMAIL_FROM", "AntWorker <noreply@example.com>"),
    ];

    fn smtp(more: &[(&'static str, &'static str)]) -> Vec<(&'static str, &'static str)> {
        let mut values = SMTP.to_vec();
        values.extend_from_slice(more);
        values
    }

    #[test]
    fn mail_006_email_is_required_and_is_smtp_or_off() {
        assert_eq!(read(&[]).err().unwrap(), "EMAIL is required");
        assert_eq!(
            read(&[("EMAIL", "sendmail")]).err().unwrap(),
            "EMAIL must be smtp or off"
        );
        assert!(matches!(read(&[("EMAIL", "off")]), Ok(Settings::Off)));
    }

    #[test]
    fn mail_006_with_email_off_no_other_email_setting_is_read() {
        let settings = read(&[
            ("EMAIL", "off"),
            ("SMTP_PORT", "not a port"),
            ("SMTP_PASSWORD", "x"),
        ]);

        assert!(matches!(settings, Ok(Settings::Off)));
    }

    #[test]
    fn mail_006_smtp_requires_the_host_and_the_sender() {
        assert_eq!(
            read(&[("EMAIL", "smtp"), ("EMAIL_FROM", "a@example.com")])
                .err()
                .unwrap(),
            "SMTP_HOST is required"
        );
        assert_eq!(
            read(&[("EMAIL", "smtp"), ("SMTP_HOST", "smtp.example.com")])
                .err()
                .unwrap(),
            "EMAIL_FROM is required"
        );
        assert!(read(&smtp(&[])).is_ok());
    }

    #[test]
    fn mail_006_credentials_come_in_pairs_and_never_unencrypted() {
        assert_eq!(
            read(&smtp(&[("SMTP_USERNAME", "api")])).err().unwrap(),
            "SMTP_PASSWORD must be set when SMTP_USERNAME is"
        );
        assert_eq!(
            read(&smtp(&[("SMTP_PASSWORD", "secret")])).err().unwrap(),
            "SMTP_USERNAME must be set when SMTP_PASSWORD is"
        );
        let unencrypted = read(&smtp(&[
            ("SMTP_SECURITY", "none"),
            ("SMTP_USERNAME", "api"),
            ("SMTP_PASSWORD", "secret"),
        ]));
        assert!(
            unencrypted
                .err()
                .unwrap()
                .starts_with("SMTP_SECURITY must be tls or starttls")
        );
        let paired = read(&smtp(&[
            ("SMTP_USERNAME", "api"),
            ("SMTP_PASSWORD", "secret"),
        ]));
        assert!(paired.is_ok());
    }

    #[test]
    fn mail_006_the_port_follows_the_security_unless_set() {
        let port = |more: &[(&'static str, &'static str)]| match read(&smtp(more)) {
            Ok(Settings::Smtp(smtp)) => (smtp.security, smtp.port),
            _ => unreachable!("the settings are valid"),
        };

        assert_eq!(port(&[]), (Security::StartTls, 587));
        assert_eq!(port(&[("SMTP_SECURITY", "tls")]), (Security::Tls, 465));
        assert_eq!(port(&[("SMTP_SECURITY", "none")]), (Security::None, 25));
        assert_eq!(port(&[("SMTP_PORT", "2525")]), (Security::StartTls, 2525));
    }

    #[test]
    fn mail_006_values_are_checked() {
        assert_eq!(
            read(&smtp(&[("SMTP_SECURITY", "ssl")])).err().unwrap(),
            "SMTP_SECURITY must be tls, starttls or none"
        );
        assert_eq!(
            read(&[
                ("EMAIL", "smtp"),
                ("SMTP_HOST", "smtp example"),
                ("EMAIL_FROM", "a@example.com"),
            ])
            .err()
            .unwrap(),
            "SMTP_HOST must be a host name or an IP address"
        );
        assert!(
            read(&[
                ("EMAIL", "smtp"),
                ("SMTP_HOST", "smtp.example.com"),
                ("EMAIL_FROM", "not an address"),
            ])
            .err()
            .unwrap()
            .starts_with("EMAIL_FROM must be a name and an address")
        );
    }
}
