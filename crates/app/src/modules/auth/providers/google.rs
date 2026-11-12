//! Google, which follows OpenID Connect (the auth spec's "Signing in through a provider").

use serde::Deserialize;

use super::{Declared, Entry, Provider};

/// Google in the registry, with two clients: one for browsers, and one for desktop clients,
/// since Google's client for a desktop app takes a loopback redirect at any port, and its client
/// for a web application only redirect URIs that match exactly (AUTH-082).
pub(super) const ENTRY: Entry = Entry {
    key: "google",
    clients: &["GOOGLE_", "GOOGLE_DESKTOP_"],
    settings: &[],
    build: |_| Ok(Box::new(Google)),
};

/// The domain of the addresses that Google hosts for anyone, which it is the authority for
/// (AUTH-082).
const GMAIL: &str = "@gmail.com";

/// Google, whose access token lapses on its own, and isn't revoked, which would take back the
/// user's consent (D139).
struct Google;

/// The claims of the userinfo endpoint's answer that say whose address it gives: Google's `hd`
/// names the Google Workspace organization that an address belongs to.
#[derive(Deserialize)]
struct Address {
    email: Option<String>,
    email_verified: Option<bool>,
    hd: Option<String>,
}

impl Provider for Google {
    fn declared(&self) -> Declared<'_> {
        Declared {
            name: "Google",
            authorization_endpoint: "https://accounts.google.com/o/oauth2/v2/auth",
            token_endpoint: "https://oauth2.googleapis.com/token",
            identity_endpoint: "https://openidconnect.googleapis.com/v1/userinfo",
            scopes: &["openid", "email", "profile"],
        }
    }

    /// Google proves an address when it is the address's authority: one it marks verified, of
    /// Gmail or of a Google Workspace organization (AUTH-075, D140).
    fn proves(&self, claims: &serde_json::Value) -> bool {
        Address::deserialize(claims).is_ok_and(|address| {
            address.email_verified == Some(true)
                && address.email.is_some_and(|email| {
                    address.hd.is_some() || email.to_ascii_lowercase().trim().ends_with(GMAIL)
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn proves(email: &str, verified: bool, hd: Option<&str>) -> bool {
        Google.proves(&json!({
            "sub": "1",
            "email": email,
            "email_verified": verified,
            "hd": hd,
        }))
    }

    #[test]
    fn auth_082_google_proves_a_verified_gmail_or_workspace_address_alone() {
        assert!(proves("ada@gmail.com", true, None));
        assert!(proves("Ada@Gmail.com", true, None));
        assert!(proves("ada@example.com", true, Some("example.com")));
        assert!(!proves("ada@example.com", true, None));
        assert!(!proves("ada@gmail.com", false, None));
        assert!(!proves("ada@gmail.com.example.org", true, None));
    }
}
