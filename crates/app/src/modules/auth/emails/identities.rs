//! The email that tells an address that the ways its account signs in changed (AUTH-091).

use askama::Template;
use platform::{errors::Internal, mail::Email, types::EmailAddress};

use crate::modules::PRODUCT;

/// What happened to an account's identities, which its email tells the address (AUTH-091). A
/// provider is named as a client shows it, or by its key when the deployment doesn't configure
/// it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::modules::auth) enum IdentityChange {
    /// A password now signs in to the account (AUTH-086).
    PasswordAdded,
    /// The account's password was removed (AUTH-088).
    PasswordRemoved,
    /// The provider now signs in to the account (AUTH-087).
    ProviderLinked(String),
    /// The provider signs in to the account no more (AUTH-088).
    ProviderUnlinked(String),
}

/// The values of the email that tells an address that its account's identities changed
/// (AUTH-091).
pub(in crate::modules::auth) struct IdentityChangedEmail {
    /// The account's address.
    pub(in crate::modules::auth) to: EmailAddress,
    /// What changed.
    pub(in crate::modules::auth) change: IdentityChange,
}

impl IdentityChangedEmail {
    /// The email, rendered from its templates: what happened, and what the owner does if it
    /// wasn't them. It holds no secret and no link.
    pub(in crate::modules::auth) fn render(self) -> Result<Email, Internal> {
        let (subject, what, advice) = said(&self.change, self.to.as_str());
        let values = Values {
            subject: &subject,
            what: &what,
            advice: &advice,
        };
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

/// What the email of `change` says to `to`: its subject, what happened, and what the owner does
/// if it wasn't them (AUTH-091). A reset ends every session, whoever holds it, and sets a
/// password that only the address's reader knows (AUTH-037).
fn said(change: &IdentityChange, to: &str) -> (String, String, String) {
    match change {
        IdentityChange::PasswordAdded => (
            format!("A password now signs in to your {PRODUCT} account"),
            format!("A password now signs in to your {PRODUCT} account, {to}."),
            format!(
                "If you added it, there is nothing to do. If you didn't, someone who could \
                 prove to {PRODUCT} that they are you did: reset your password in {PRODUCT} \
                 now, which replaces theirs and ends every session of the account."
            ),
        ),
        IdentityChange::PasswordRemoved => (
            format!("The password of your {PRODUCT} account was removed"),
            format!(
                "The password of your {PRODUCT} account, {to}, was removed, so the account \
                 signs in without one."
            ),
            format!(
                "If you removed it, there is nothing to do. If you didn't, reset your password \
                 in {PRODUCT} now, which sets a new one and ends every session of the account."
            ),
        ),
        IdentityChange::ProviderLinked(provider) => (
            format!("{provider} now signs in to your {PRODUCT} account"),
            format!("An account at {provider} now signs in to your {PRODUCT} account, {to}."),
            format!(
                "If you linked it, there is nothing to do. If you didn't, someone who could \
                 prove to {PRODUCT} that they are you did: reset your password in {PRODUCT} \
                 now, which ends every session of the account, then sign in and remove \
                 {provider} from the ways the account signs in."
            ),
        ),
        IdentityChange::ProviderUnlinked(provider) => (
            format!("{provider} signs in to your {PRODUCT} account no more"),
            format!(
                "Your account at {provider} signs in to your {PRODUCT} account, {to}, no more."
            ),
            format!(
                "If you removed it, there is nothing to do. If you didn't, reset your password \
                 in {PRODUCT} now, which ends every session of the account, then sign in and \
                 link {provider} again."
            ),
        ),
    }
}

/// The values of the email's templates.
struct Values<'a> {
    subject: &'a str,
    what: &'a str,
    advice: &'a str,
}

/// The email's plain-text part, whose values aren't escaped.
#[derive(Template)]
#[template(path = "auth/identity_changed.txt")]
struct Text<'a> {
    email: &'a Values<'a>,
}

/// The email's HTML part, whose values are escaped.
#[derive(Template)]
#[template(path = "auth/identity_changed.html")]
struct Html<'a> {
    email: &'a Values<'a>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_091_each_change_s_email_says_what_happened_and_what_to_do_without_a_link() {
        for (change, subject, what) in [
            (
                IdentityChange::PasswordAdded,
                "A password now signs in",
                "A password now signs in to your",
            ),
            (
                IdentityChange::PasswordRemoved,
                "was removed",
                "signs in without one",
            ),
            (
                IdentityChange::ProviderLinked("Stand-in".to_owned()),
                "Stand-in now signs in",
                "An account at Stand-in now signs in",
            ),
            (
                IdentityChange::ProviderUnlinked("Stand-in".to_owned()),
                "Stand-in signs in",
                "Your account at Stand-in signs in",
            ),
        ] {
            let to = EmailAddress::parse("alice@example.com").unwrap();
            let email = IdentityChangedEmail { to, change }.render().unwrap();

            assert!(email.subject.contains(subject), "{}", email.subject);
            for part in [&email.text, &email.html] {
                assert!(part.contains(what), "{part}");
                assert!(part.contains("alice@example.com"), "{part}");
                assert!(part.contains("reset your password"), "{part}");
            }
            // The HTML part escapes the apostrophe.
            assert!(email.text.contains("If you didn't"), "{}", email.text);
            assert!(!email.text.contains("http"), "{}", email.text);
            assert!(!email.html.contains("href"), "{}", email.html);
        }
    }

    #[test]
    fn auth_091_the_html_part_escapes_a_provider_s_name() {
        let to = EmailAddress::parse("alice@example.com").unwrap();
        let change = IdentityChange::ProviderLinked("<b>Acme</b>".to_owned());
        let email = IdentityChangedEmail { to, change }.render().unwrap();

        assert!(email.text.contains("<b>Acme</b>"));
        assert!(!email.html.contains("<b>Acme</b>"), "{}", email.html);
    }
}
