//! Google, through OpenID Connect (the auth spec's "Signing in through a provider").

use futures_util::future::BoxFuture;
use platform::{
    outbound::{Configured, Request},
    types::Secret,
};
use serde::Deserialize;

use super::{Client, Entry, Identity, Provider, ProviderError};

/// Google in the registry, with two clients: one for browsers, and one for desktop clients,
/// since Google's client for a desktop app takes a loopback redirect at any port, and its client
/// for a web application only redirect URIs that match exactly (AUTH-082).
pub(super) const ENTRY: Entry = Entry {
    key: "google",
    clients: &["GOOGLE_", "GOOGLE_DESKTOP_"],
    build: |clients| Box::new(Google { clients }),
};

const AUTHORIZATION_ENDPOINT: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const USERINFO_ENDPOINT: &str = "https://openidconnect.googleapis.com/v1/userinfo";

/// The domain of the addresses that Google hosts for anyone, which it is the authority for
/// (AUTH-082).
const GMAIL: &str = "@gmail.com";

/// Google, with the deployment's clients.
struct Google {
    clients: Vec<Client>,
}

/// The userinfo endpoint's answer (OpenID Connect Core, §5.3.2).
#[derive(Deserialize)]
struct UserInfo {
    sub: String,
    email: Option<String>,
    email_verified: Option<bool>,
    hd: Option<String>,
    name: Option<String>,
}

impl Provider for Google {
    fn key(&self) -> &'static str {
        ENTRY.key
    }

    fn name(&self) -> &'static str {
        "Google"
    }

    fn authorization_endpoint(&self) -> &'static str {
        AUTHORIZATION_ENDPOINT
    }

    fn scopes(&self) -> &'static [&'static str] {
        &["openid", "email", "profile"]
    }

    fn clients(&self) -> &[Client] {
        &self.clients
    }

    fn token_endpoint(&self) -> &'static str {
        TOKEN_ENDPOINT
    }

    /// Reads the identity from the userinfo endpoint, and leaves the token to lapse, since
    /// revoking it would take back the user's consent (D139).
    fn identify<'a>(
        &'a self,
        outbound: &'a Configured,
        _client: &'a Client,
        token: &'a Secret<String>,
    ) -> BoxFuture<'a, Result<Identity, ProviderError>> {
        Box::pin(async move {
            let answer = outbound
                .send(Request::get(USERINFO_ENDPOINT).bearer(Secret::new(token.expose().clone())))
                .await
                .map_err(|error| ProviderError::Unavailable(error.to_string()))?;
            let info: UserInfo = answer
                .is_success()
                .then(|| answer.json())
                .flatten()
                .ok_or_else(|| {
                    ProviderError::Unavailable(format!(
                        "the userinfo endpoint answered {}",
                        answer.status
                    ))
                })?;
            Ok(identity(info))
        })
    }
}

/// The identity in `info`, whose address Google proves when it is the address's authority: an
/// address it marks verified, of Gmail or of a Google Workspace organization (AUTH-075, D140).
fn identity(info: UserInfo) -> Identity {
    let proved = info.email_verified == Some(true)
        && info.email.as_deref().is_some_and(|email| {
            info.hd.is_some() || email.to_ascii_lowercase().trim().ends_with(GMAIL)
        });
    Identity {
        subject: info.sub,
        email: info.email,
        proved,
        name: info.name,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(email: &str, verified: bool, hd: Option<&str>) -> UserInfo {
        UserInfo {
            sub: "1".to_owned(),
            email: Some(email.to_owned()),
            email_verified: Some(verified),
            hd: hd.map(str::to_owned),
            name: None,
        }
    }

    #[test]
    fn auth_082_google_proves_a_verified_gmail_or_workspace_address_alone() {
        assert!(identity(info("ada@gmail.com", true, None)).proved);
        assert!(identity(info("Ada@Gmail.com", true, None)).proved);
        assert!(identity(info("ada@example.com", true, Some("example.com"))).proved);
        assert!(!identity(info("ada@example.com", true, None)).proved);
        assert!(!identity(info("ada@gmail.com", false, None)).proved);
        assert!(!identity(info("ada@gmail.com.example.org", true, None)).proved);
    }
}
