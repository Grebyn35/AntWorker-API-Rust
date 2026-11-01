//! The emails `mail` sends, each with its recipient and exactly the values its template needs
//! (the mail spec's Interface), and their rendering (MAIL-002, MAIL-003).

use std::fmt;

use askama::Template;
use platform::{errors::Internal, types::EmailAddress};
use serde::{Deserialize, Serialize};

/// The product's name, which the emails carry, and `just rename` rewrites (ARCH-15).
const PRODUCT: &str = "AntWorker";

/// An email, by kind. It is part of a job's payload, so a variant changes only by adding fields
/// that may be absent (the platform spec's Jobs).
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Message {
    /// The invitation email (MAIL-002).
    Invitation(Invitation),
}

/// The invitation email's values (MAIL-002).
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Invitation {
    /// The invited address.
    pub(crate) to: EmailAddress,
    /// The inviter's name.
    pub(crate) inviter: String,
    /// The workspace's name.
    pub(crate) workspace: String,
    /// The role the invitation offers, such as `member`.
    pub(crate) role: String,
    /// The days until the invitation expires.
    pub(crate) expires_in_days: u32,
}

impl fmt::Debug for Message {
    /// Names the kind only: every value is personal data (OBS-04).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invitation(_) => f.write_str("Message::Invitation([redacted])"),
        }
    }
}

/// An email, rendered.
pub(super) struct Rendered {
    /// Its subject.
    pub(super) subject: String,
    /// Its plain-text part.
    pub(super) text: String,
    /// Its HTML part.
    pub(super) html: String,
}

impl Message {
    /// The address the email goes to.
    pub(super) fn to(&self) -> &EmailAddress {
        match self {
            Self::Invitation(invitation) => &invitation.to,
        }
    }

    /// The email, rendered from its templates.
    pub(super) fn render(&self) -> Result<Rendered, Internal> {
        match self {
            Self::Invitation(invitation) => {
                let values = InvitationValues {
                    product: PRODUCT,
                    inviter: &invitation.inviter,
                    workspace: &invitation.workspace,
                    role: &with_article(&invitation.role),
                    expiry: &days(invitation.expires_in_days),
                    to: invitation.to.as_str(),
                };
                Ok(Rendered {
                    subject: format!(
                        "{} invited you to {} on {PRODUCT}",
                        invitation.inviter, invitation.workspace
                    ),
                    text: InvitationText { email: &values }
                        .render()
                        .map_err(Internal::new)?,
                    html: InvitationHtml { email: &values }
                        .render()
                        .map_err(Internal::new)?,
                })
            }
        }
    }
}

/// The values of the invitation email's templates.
struct InvitationValues<'a> {
    product: &'a str,
    inviter: &'a str,
    workspace: &'a str,
    role: &'a str,
    expiry: &'a str,
    to: &'a str,
}

/// The invitation email's plain-text part, whose values aren't escaped.
#[derive(Template)]
#[template(path = "email/invitation.txt")]
struct InvitationText<'a> {
    email: &'a InvitationValues<'a>,
}

/// The invitation email's HTML part, whose values are escaped (MAIL-003).
#[derive(Template)]
#[template(path = "email/invitation.html")]
struct InvitationHtml<'a> {
    email: &'a InvitationValues<'a>,
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

    fn invitation(inviter: &str, workspace: &str) -> Message {
        Message::Invitation(Invitation {
            to: EmailAddress::parse("alice@example.com").unwrap(),
            inviter: inviter.to_owned(),
            workspace: workspace.to_owned(),
            role: "member".to_owned(),
            expires_in_days: 7,
        })
    }

    #[test]
    fn mail_002_the_invitation_email_names_the_inviter_the_workspace_the_role_and_the_expiry() {
        let rendered = invitation("Bob Jones", "Acme").render().unwrap();
        assert_eq!(
            rendered.subject,
            "Bob Jones invited you to Acme on AntWorker"
        );
        for part in [&rendered.text, &rendered.html] {
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
    fn mail_003_the_html_part_escapes_values_and_the_text_part_keeps_them() {
        let rendered = invitation("<b>Bob</b> & co", "Acme").render().unwrap();
        let html = &rendered.html;
        assert!(!html.contains("<b>Bob</b>"), "{html}");
        assert!(
            html.contains("&lt;b&gt;Bob&lt;/b&gt; &amp; co")
                || html.contains("&#60;b&#62;Bob&#60;/b&#62; &#38; co"),
            "{html}"
        );
        assert!(rendered.text.contains("<b>Bob</b> & co"));
    }

    #[test]
    fn mail_002_a_role_takes_its_article_and_a_day_its_number() {
        assert_eq!(with_article("admin"), "an admin");
        assert_eq!(with_article("member"), "a member");
        assert_eq!(days(1), "1 day");
        assert_eq!(days(90), "90 days");
    }
}
