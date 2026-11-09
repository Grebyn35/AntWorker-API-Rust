//! The emails of `accounts`: the one that tells an account's address the account is deleted
//! (ACCOUNTS-015), and the one that says its export is ready (ACCOUNTS-025), rendered from their
//! templates in `templates/accounts/` (the platform spec's Sending email).

use askama::Template;
use platform::{
    errors::Internal,
    mail::Email,
    types::{EmailAddress, Timestamp},
};

use crate::modules::PRODUCT;

/// The deletion email's values (ACCOUNTS-015).
pub(super) struct Deleted {
    /// The deleted account's address.
    pub(super) to: EmailAddress,
}

impl Deleted {
    /// The email, rendered from its templates.
    pub(super) fn render(self) -> Result<Email, Internal> {
        let values = Values {
            product: PRODUCT,
            to: self.to.as_str(),
        };
        let subject = format!("Your {PRODUCT} account is deleted");
        let text = Text { email: &values }.render().map_err(Internal::new)?;
        let html = Html { email: &values }.render().map_err(Internal::new)?;
        Ok(Email {
            to: self.to,
            subject,
            text,
            html,
        })
    }
}

/// The values of the email's templates.
struct Values<'a> {
    product: &'a str,
    to: &'a str,
}

/// The values of the email that says an export is ready (ACCOUNTS-025).
pub(super) struct ExportReady {
    /// The account's address.
    pub(super) to: EmailAddress,
    /// When the export expires, which the email gives as a day and a time, in UTC.
    pub(super) expires_at: Timestamp,
}

impl ExportReady {
    /// The email, rendered from its templates.
    pub(super) fn render(self) -> Result<Email, Internal> {
        let at = self.expires_at.at().to_offset(time::UtcOffset::UTC);
        let until = format!(
            "{:04}-{:02}-{:02} {:02}:{:02} UTC",
            at.year(),
            u8::from(at.month()),
            at.day(),
            at.hour(),
            at.minute()
        );
        let values = ReadyValues {
            product: PRODUCT,
            until: &until,
        };
        let subject = format!("Your {PRODUCT} data export is ready");
        let text = ReadyText { email: &values }
            .render()
            .map_err(Internal::new)?;
        let html = ReadyHtml { email: &values }
            .render()
            .map_err(Internal::new)?;
        Ok(Email {
            to: self.to,
            subject,
            text,
            html,
        })
    }
}

/// The values of the ready email's templates.
struct ReadyValues<'a> {
    product: &'a str,
    until: &'a str,
}

/// The ready email's plain-text part, whose values aren't escaped.
#[derive(Template)]
#[template(path = "accounts/export_ready.txt")]
struct ReadyText<'a> {
    email: &'a ReadyValues<'a>,
}

/// The ready email's HTML part, whose values are escaped.
#[derive(Template)]
#[template(path = "accounts/export_ready.html")]
struct ReadyHtml<'a> {
    email: &'a ReadyValues<'a>,
}

/// The email's plain-text part, whose values aren't escaped.
#[derive(Template)]
#[template(path = "accounts/deleted.txt")]
struct Text<'a> {
    email: &'a Values<'a>,
}

/// The email's HTML part, whose values are escaped.
#[derive(Template)]
#[template(path = "accounts/deleted.html")]
struct Html<'a> {
    email: &'a Values<'a>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accounts_015_the_email_names_the_account_and_carries_no_link() {
        let to = EmailAddress::parse("alice@example.com").unwrap();
        let email = Deleted { to }.render().unwrap();
        assert_eq!(email.to.as_str(), "alice@example.com");
        assert_eq!(email.subject, "Your AntWorker account is deleted");
        for part in [&email.text, &email.html] {
            assert!(
                part.contains("Your AntWorker account for alice@example.com is deleted"),
                "{part}"
            );
            assert!(!part.contains("http"), "{part}");
        }
    }

    #[test]
    fn accounts_025_the_email_says_until_when_and_carries_no_link() {
        let to = EmailAddress::parse("alice@example.com").unwrap();
        let expires_at = Timestamp::from_database(
            time::OffsetDateTime::from_unix_timestamp(1_794_300_005).unwrap(),
        );
        let email = ExportReady { to, expires_at }.render().unwrap();
        assert_eq!(email.to.as_str(), "alice@example.com");
        assert_eq!(email.subject, "Your AntWorker data export is ready");
        for part in [&email.text, &email.html] {
            assert!(part.contains("until 2026-11-10 08:40 UTC"), "{part}");
            assert!(part.contains("sign in to AntWorker"), "{part}");
            assert!(!part.contains("http"), "{part}");
        }
    }
}
