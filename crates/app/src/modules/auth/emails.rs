//! The emails of `auth`: the one that carries the code that proves an account's email address
//! (AUTH-030), the one that carries the code that resets its password (AUTH-036), the one that
//! tells the address its password changed (AUTH-041), the one that carries the code that changes
//! the account's address to the one it goes to (AUTH-050), and the one that tells the address
//! before a change that it changed (AUTH-053), each rendered from its templates in
//! `templates/auth/` (the platform spec's Sending email).

use askama::Template;
use platform::{errors::Internal, mail::Email, types::EmailAddress};

use super::model::{ChangeCode, ResetCode, VerificationCode};
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

/// The values of the email that carries a reset code (AUTH-036).
pub(super) struct ResetCodeEmail {
    /// The account's address, whose password the code resets.
    pub(super) to: EmailAddress,
    /// The code.
    pub(super) code: ResetCode,
}

impl ResetCodeEmail {
    /// The email, rendered from its templates, with the code in four groups of four.
    pub(super) fn render(self) -> Result<Email, Internal> {
        let code = self.code.grouped();
        let values = Values {
            product: PRODUCT,
            to: self.to.as_str(),
            code: &code,
        };
        let subject = format!("Your {PRODUCT} password reset code");
        let text = ResetText { email: &values }
            .render()
            .map_err(Internal::new)?;
        let html = ResetHtml { email: &values }
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

/// The reset code email's plain-text part, whose values aren't escaped.
#[derive(Template)]
#[template(path = "auth/reset_code.txt")]
struct ResetText<'a> {
    email: &'a Values<'a>,
}

/// The reset code email's HTML part, whose values are escaped.
#[derive(Template)]
#[template(path = "auth/reset_code.html")]
struct ResetHtml<'a> {
    email: &'a Values<'a>,
}

/// The values of the email that tells an address its account's password changed (AUTH-041).
pub(super) struct PasswordChangedEmail {
    /// The account's address.
    pub(super) to: EmailAddress,
}

impl PasswordChangedEmail {
    /// The email, rendered from its templates.
    pub(super) fn render(self) -> Result<Email, Internal> {
        let values = Notice {
            product: PRODUCT,
            to: self.to.as_str(),
        };
        let subject = format!("Your {PRODUCT} password has changed");
        let text = ChangedText { email: &values }
            .render()
            .map_err(Internal::new)?;
        let html = ChangedHtml { email: &values }
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

/// The values of the email that tells an address its password changed.
struct Notice<'a> {
    product: &'a str,
    to: &'a str,
}

/// The password changed email's plain-text part, whose values aren't escaped.
#[derive(Template)]
#[template(path = "auth/password_changed.txt")]
struct ChangedText<'a> {
    email: &'a Notice<'a>,
}

/// The password changed email's HTML part, whose values are escaped.
#[derive(Template)]
#[template(path = "auth/password_changed.html")]
struct ChangedHtml<'a> {
    email: &'a Notice<'a>,
}

/// The values of the email that carries a change code to the address a change asked for
/// (AUTH-050).
pub(super) struct ChangeCodeEmail {
    /// The address asked for, which the code makes the account's.
    pub(super) to: EmailAddress,
    /// The code.
    pub(super) code: ChangeCode,
}

impl ChangeCodeEmail {
    /// The email, rendered from its templates, with the code in four groups of four.
    pub(super) fn render(self) -> Result<Email, Internal> {
        let code = self.code.grouped();
        let values = Values {
            product: PRODUCT,
            to: self.to.as_str(),
            code: &code,
        };
        let subject = format!("Your {PRODUCT} email change code");
        let text = ChangeCodeText { email: &values }
            .render()
            .map_err(Internal::new)?;
        let html = ChangeCodeHtml { email: &values }
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

/// The change code email's plain-text part, whose values aren't escaped.
#[derive(Template)]
#[template(path = "auth/email_change_code.txt")]
struct ChangeCodeText<'a> {
    email: &'a Values<'a>,
}

/// The change code email's HTML part, whose values are escaped.
#[derive(Template)]
#[template(path = "auth/email_change_code.html")]
struct ChangeCodeHtml<'a> {
    email: &'a Values<'a>,
}

/// The values of the email that tells an address its account's address changed to another
/// (AUTH-053).
pub(super) struct EmailChangedEmail {
    /// The account's address before the change.
    pub(super) to: EmailAddress,
    /// The account's address now.
    pub(super) new: EmailAddress,
}

impl EmailChangedEmail {
    /// The email, rendered from its templates.
    pub(super) fn render(self) -> Result<Email, Internal> {
        let values = Moved {
            product: PRODUCT,
            to: self.to.as_str(),
            new: self.new.as_str(),
        };
        let subject = format!("Your {PRODUCT} email address has changed");
        let text = MovedText { email: &values }
            .render()
            .map_err(Internal::new)?;
        let html = MovedHtml { email: &values }
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

/// The values of the email that tells an address its account's address changed.
struct Moved<'a> {
    product: &'a str,
    to: &'a str,
    new: &'a str,
}

/// The address changed email's plain-text part, whose values aren't escaped.
#[derive(Template)]
#[template(path = "auth/email_changed.txt")]
struct MovedText<'a> {
    email: &'a Moved<'a>,
}

/// The address changed email's HTML part, whose values are escaped.
#[derive(Template)]
#[template(path = "auth/email_changed.html")]
struct MovedHtml<'a> {
    email: &'a Moved<'a>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_036_the_email_carries_the_reset_code_says_how_long_it_works_and_has_no_link() {
        let to = EmailAddress::parse("alice@example.com").unwrap();
        let code = ResetCode::parse("7KQM2XWD9RTBH4NP").unwrap();
        let email = ResetCodeEmail { to, code }.render().unwrap();
        assert_eq!(email.to.as_str(), "alice@example.com");
        assert_eq!(email.subject, "Your AntWorker password reset code");
        for part in [&email.text, &email.html] {
            assert!(part.contains("7KQM-2XWD-9RTB-H4NP"), "{part}");
            assert!(part.contains("It works once, for 10 minutes."), "{part}");
            assert!(
                part.contains("ignore this email: nothing changes"),
                "{part}"
            );
            assert!(part.contains("alice@example.com"), "{part}");
            assert!(!part.contains("http"), "{part}");
        }
    }

    #[test]
    fn auth_041_the_email_says_the_password_changed_and_how_to_reset_it_with_no_secret_or_link() {
        let to = EmailAddress::parse("alice@example.com").unwrap();
        let email = PasswordChangedEmail { to }.render().unwrap();
        assert_eq!(email.to.as_str(), "alice@example.com");
        assert_eq!(email.subject, "Your AntWorker password has changed");
        for part in [&email.text, &email.html] {
            assert!(part.contains("has changed"), "{part}");
            assert!(part.contains("reset your password"), "{part}");
            assert!(part.contains("alice@example.com"), "{part}");
            assert!(!part.contains("http"), "{part}");
        }
    }

    #[test]
    fn auth_050_the_email_carries_the_change_code_says_what_it_does_and_has_no_link() {
        let to = EmailAddress::parse("ada@new.example.com").unwrap();
        let code = ChangeCode::parse("7KQM2XWD9RTBH4NP").unwrap();
        let email = ChangeCodeEmail { to, code }.render().unwrap();
        assert_eq!(email.to.as_str(), "ada@new.example.com");
        assert_eq!(email.subject, "Your AntWorker email change code");
        assert!(!email.subject.contains("7KQM"));
        for part in [&email.text, &email.html] {
            assert!(part.contains("7KQM-2XWD-9RTB-H4NP"), "{part}");
            assert!(
                part.contains("to make ada@new.example.com the email address of your account"),
                "{part}"
            );
            assert!(
                part.contains("It works once, for 10 minutes, and a newer code replaces it."),
                "{part}"
            );
            assert!(
                part.contains("ignore this email: nothing changes"),
                "{part}"
            );
            assert!(!part.contains("http"), "{part}");
        }
    }

    #[test]
    fn auth_053_the_email_names_the_new_address_and_how_to_change_it_back_with_no_link() {
        let to = EmailAddress::parse("ada@example.com").unwrap();
        let new = EmailAddress::parse("ada@new.example.com").unwrap();
        let email = EmailChangedEmail { to, new }.render().unwrap();
        assert_eq!(email.to.as_str(), "ada@example.com");
        assert_eq!(email.subject, "Your AntWorker email address has changed");
        for part in [&email.text, &email.html] {
            assert!(
                part.contains("has changed from ada@example.com to ada@new.example.com"),
                "{part}"
            );
            assert!(part.contains("Sign in with ada@new.example.com"), "{part}");
            assert!(
                part.contains(
                    "change the address back to ada@example.com, then change your password"
                ),
                "{part}"
            );
            assert!(!part.contains("http"), "{part}");
        }
    }

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
