//! The invitation email (INVITATIONS-010), rendered from its templates in
//! `templates/invitations/` (the platform spec's Mail).

use askama::Template;
use platform::{errors::Internal, mail::Email, types::EmailAddress};

use crate::modules::access::Role;

/// The product's name, which the email carries, and `just rename` rewrites (ARCH-15).
const PRODUCT: &str = "AntWorker";

/// The invitation email's values (INVITATIONS-010).
pub(super) struct Invitation {
    /// The invited address.
    pub(super) to: EmailAddress,
    /// The inviter's name.
    pub(super) inviter: String,
    /// The workspace's name.
    pub(super) workspace: String,
    /// The role the invitation offers.
    pub(super) role: Role,
    /// The days until the invitation expires.
    pub(super) expires_in_days: u32,
}

impl Invitation {
    /// The email, rendered from its templates.
    pub(super) fn render(self) -> Result<Email, Internal> {
        let values = Values {
            product: PRODUCT,
            inviter: &self.inviter,
            workspace: &self.workspace,
            role: &with_article(self.role.name()),
            expiry: &days(self.expires_in_days),
            to: self.to.as_str(),
        };
        let subject = format!(
            "{} invited you to {} on {PRODUCT}",
            self.inviter, self.workspace
        );
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
    inviter: &'a str,
    workspace: &'a str,
    role: &'a str,
    expiry: &'a str,
    to: &'a str,
}

/// The email's plain-text part, whose values aren't escaped.
#[derive(Template)]
#[template(path = "invitations/invitation.txt")]
struct Text<'a> {
    email: &'a Values<'a>,
}

/// The email's HTML part, whose values are escaped (INVITATIONS-010).
#[derive(Template)]
#[template(path = "invitations/invitation.html")]
struct Html<'a> {
    email: &'a Values<'a>,
}

/// `role` after its indefinite article, such as "an admin" or "a member".
fn with_article(role: &str) -> String {
    let article = if role.starts_with(['a', 'e', 'i', 'o', 'u']) {
        "an"
    } else {
        "a"
    };
    format!("{article} {role}")
}

/// `count` days, as a sentence says it.
fn days(count: u32) -> String {
    if count == 1 {
        "1 day".to_owned()
    } else {
        format!("{count} days")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invitation(inviter: &str, workspace: &str) -> Invitation {
        Invitation {
            to: EmailAddress::parse("alice@example.com").unwrap(),
            inviter: inviter.to_owned(),
            workspace: workspace.to_owned(),
            role: Role::Member,
            expires_in_days: 7,
        }
    }

    #[test]
    fn invitations_010_the_email_names_the_inviter_the_workspace_the_role_and_the_expiry() {
        let email = invitation("Bob Jones", "Acme").render().unwrap();
        assert_eq!(email.to.as_str(), "alice@example.com");
        assert_eq!(email.subject, "Bob Jones invited you to Acme on AntWorker");
        for part in [&email.text, &email.html] {
            assert!(part.contains("Bob Jones"), "{part}");
            assert!(part.contains("Acme"), "{part}");
            assert!(part.contains("as a member"), "{part}");
            assert!(part.contains("expires in 7 days"), "{part}");
            assert!(
                part.contains("sign in to AntWorker with alice@example.com"),
                "{part}"
            );
            assert!(part.contains("create an account"), "{part}");
        }
    }

    #[test]
    fn invitations_010_the_html_part_escapes_values_and_the_text_part_keeps_them() {
        let email = invitation("<b>Bob</b> & co", "Acme").render().unwrap();
        let html = &email.html;
        assert!(!html.contains("<b>Bob</b>"), "{html}");
        assert!(
            html.contains("&lt;b&gt;Bob&lt;/b&gt; &amp; co")
                || html.contains("&#60;b&#62;Bob&#60;/b&#62; &#38; co"),
            "{html}"
        );
        assert!(email.text.contains("<b>Bob</b> & co"));
    }

    #[test]
    fn invitations_010_a_role_takes_its_article_and_a_day_its_number() {
        assert_eq!(with_article("admin"), "an admin");
        assert_eq!(with_article("member"), "a member");
        assert_eq!(days(1), "1 day");
        assert_eq!(days(90), "90 days");
    }
}
