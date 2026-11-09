//! The email that tells an account's address the account is deleted (ACCOUNTS-015), rendered
//! from its templates in `templates/accounts/` (the platform spec's Sending email).

use askama::Template;
use platform::{errors::Internal, mail::Email, types::EmailAddress};

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
}
