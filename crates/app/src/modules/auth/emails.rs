//! The email of `auth`: the one that carries the code that proves an account's email address
//! (AUTH-030), rendered from its templates in `templates/auth/` (the platform spec's Sending
//! email).

use askama::Template;
use platform::{errors::Internal, mail::Email, types::EmailAddress};

use super::model::VerificationCode;
use crate::modules::PRODUCT;

/// The values of the email that carries a verification code (AUTH-030).
pub(super) struct CodeEmail {
    /// The account's address, which the code proves.
    pub(super) to: EmailAddress,
    /// The code.
    pub(super) code: VerificationCode,
}

impl CodeEmail {
    /// The email, rendered from its templates.
    pub(super) fn render(self) -> Result<Email, Internal> {
        let values = Values {
            product: PRODUCT,
            to: self.to.as_str(),
            code: self.code.digits(),
        };
        let subject = format!("Your {PRODUCT} verification code");
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
    code: &'a str,
}

/// The email's plain-text part, whose values aren't escaped.
#[derive(Template)]
#[template(path = "auth/verification_code.txt")]
struct Text<'a> {
    email: &'a Values<'a>,
}

/// The email's HTML part, whose values are escaped.
#[derive(Template)]
#[template(path = "auth/verification_code.html")]
struct Html<'a> {
    email: &'a Values<'a>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_030_the_email_carries_the_code_says_a_newer_one_replaces_it_and_has_no_link() {
        let to = EmailAddress::parse("alice@example.com").unwrap();
        let code = VerificationCode::parse("042917").unwrap();
        let email = CodeEmail { to, code }.render().unwrap();
        assert_eq!(email.to.as_str(), "alice@example.com");
        assert_eq!(email.subject, "Your AntWorker verification code");
        assert!(!email.subject.contains("042917"));
        for part in [&email.text, &email.html] {
            assert!(part.contains("042917"), "{part}");
            assert!(part.contains("10 minutes"), "{part}");
            // How the account's owner recognizes the email of a second attempt (OPS-09).
            assert!(part.contains("a newer code replaces it"), "{part}");
            assert!(part.contains("alice@example.com"), "{part}");
            assert!(!part.contains("http"), "{part}");
        }
    }
}
