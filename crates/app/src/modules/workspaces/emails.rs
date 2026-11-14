//! The email that tells an address a workspace was set up for its account (WORKSPACES-025),
//! rendered from its templates in `templates/workspaces/` (the platform spec's Sending email).

use askama::Template;
use platform::{errors::Internal, mail::Email, types::EmailAddress};

use super::model::WorkspaceName;
use crate::modules::PRODUCT;

/// The values of the email that tells an address a workspace was set up for its account
/// (WORKSPACES-025).
pub(super) struct SetUpEmail {
    /// The owner's address.
    pub(super) to: EmailAddress,
    /// The workspace's name.
    pub(super) workspace: WorkspaceName,
    /// Whether the account has no identity yet, so its owner sets a password before they sign in.
    pub(super) sets_password: bool,
}

impl SetUpEmail {
    /// The email, rendered from its templates: which workspace was set up, and how its owner
    /// signs in. It holds no secret and no link.
    pub(super) fn render(self) -> Result<Email, Internal> {
        let workspace = self.workspace.as_str();
        let to = self.to.as_str();
        let subject = format!("The workspace {workspace} was set up for you on {PRODUCT}");
        let what = format!(
            "{PRODUCT}'s operator set up the workspace {workspace} for you on {PRODUCT}, and you \
             are its owner."
        );
        let next = if self.sets_password {
            format!(
                "Your account is {to}, and it has no password yet. To sign in for the first \
                 time, ask for a password reset where you sign in to {PRODUCT}, with this \
                 address: the code it sends you sets your password."
            )
        } else {
            format!("Sign in to {PRODUCT} with {to} to find it.")
        };
        let values = Values {
            subject: &subject,
            what: &what,
            next: &next,
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

/// The values of the email's templates.
struct Values<'a> {
    subject: &'a str,
    what: &'a str,
    next: &'a str,
}

/// The email's plain-text part, whose values aren't escaped.
#[derive(Template)]
#[template(path = "workspaces/set_up.txt")]
struct Text<'a> {
    email: &'a Values<'a>,
}

/// The email's HTML part, whose values are escaped.
#[derive(Template)]
#[template(path = "workspaces/set_up.html")]
struct Html<'a> {
    email: &'a Values<'a>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email(sets_password: bool) -> Email {
        SetUpEmail {
            to: EmailAddress::parse("ada@example.com").unwrap(),
            workspace: WorkspaceName::parse("Acme <Labs>").unwrap(),
            sets_password,
        }
        .render()
        .unwrap()
    }

    #[test]
    fn workspaces_025_the_email_names_the_workspace_and_how_its_owner_signs_in_with_no_link() {
        let first = email(true);
        let known = email(false);

        for email in [&first, &known] {
            assert_eq!(email.to.as_str(), "ada@example.com");
            assert_eq!(
                email.subject,
                "The workspace Acme <Labs> was set up for you on AntWorker"
            );
            assert!(email.text.contains("the workspace Acme <Labs> for you"));
            // The HTML part escapes the name.
            assert!(
                email.html.contains("Acme &lt;Labs&gt;")
                    || email.html.contains("Acme &#60;Labs&#62;"),
                "{}",
                email.html
            );
            for part in [&email.text, &email.html] {
                assert!(part.contains("you are its owner"), "{part}");
                assert!(part.contains("ada@example.com"), "{part}");
                assert!(!part.contains("http"), "{part}");
            }
        }
        for part in [&first.text, &first.html] {
            assert!(part.contains("it has no password yet"), "{part}");
            assert!(part.contains("ask for a password reset"), "{part}");
        }
        for part in [&known.text, &known.html] {
            assert!(part.contains("Sign in to AntWorker with"), "{part}");
            assert!(!part.contains("password"), "{part}");
        }
    }
}
