//! The exchange of a code for an access token that RFC 6749 defines (§4.1.3), which every
//! provider shares (AUTH-077).

use platform::{
    outbound::{Configured, Request, Response},
    types::Secret,
};
use serde::Deserialize;

use super::{Client, On, ProviderError};

/// What a request exchanges at a provider: its code, its verifier, and its redirect URI, with
/// the client that lists the URI (AUTH-073, AUTH-078).
pub(in crate::modules::auth) struct Exchange<'a> {
    pub(in crate::modules::auth) client: &'a Client,
    pub(in crate::modules::auth) code: &'a Secret<String>,
    pub(in crate::modules::auth) verifier: &'a Secret<String>,
    pub(in crate::modules::auth) redirect_uri: &'a str,
}

impl On {
    /// Exchanges `exchange`'s code for an access token, as RFC 6749 asks (§4.1.3) (AUTH-073,
    /// AUTH-077). A refusal of the code, its verifier or its redirect URI is the token endpoint's
    /// `invalid_grant` (§5.2); any other refusal is of the API's own client or request, which no
    /// user can fix, so it is an unavailable provider.
    pub(in crate::modules::auth) async fn token(
        &self,
        outbound: &Configured,
        exchange: &Exchange<'_>,
    ) -> Result<Secret<String>, ProviderError> {
        let client = exchange.client;
        let mut fields = vec![
            ("grant_type", "authorization_code"),
            ("code", exchange.code.expose().as_str()),
            ("code_verifier", exchange.verifier.expose().as_str()),
            ("redirect_uri", exchange.redirect_uri),
        ];
        let request = Request::post(self.provider.declared().token_endpoint)
            .header("accept", "application/json");
        let request = if self.provider.basic() {
            // Each is encoded as a form encodes it, before the scheme's own encoding (§2.3.1).
            request.basic(
                &form_encoded(&client.id),
                &Secret::new(form_encoded(client.secret.expose())),
            )
        } else {
            fields.push(("client_id", client.id.as_str()));
            fields.push(("client_secret", client.secret.expose().as_str()));
            request
        };
        let answer = outbound
            .send(request.form(&fields))
            .await
            .map_err(|error| ProviderError::Unavailable(error.to_string()))?;
        if !answer.is_success() {
            return Err(refusal(&answer));
        }
        self.provider.token(&answer)
    }
}

/// `value` as an HTML form encodes it, as RFC 6749 encodes a client's ID and its secret for the
/// Basic scheme (§2.3.1).
fn form_encoded(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// Why the token endpoint refused the exchange, from its answer `answer` (RFC 6749, §5.2).
fn refusal(answer: &Response) -> ProviderError {
    #[derive(Deserialize)]
    struct Refusal {
        error: String,
    }
    match answer.json::<Refusal>() {
        Some(refusal) if refusal.error == "invalid_grant" => ProviderError::Rejected,
        Some(refusal) => ProviderError::Unavailable(format!(
            "the token endpoint refused the API's request with {}, and status {}",
            error_code(&refusal.error),
            answer.status
        )),
        None => {
            ProviderError::Unavailable(format!("the token endpoint answered {}", answer.status))
        }
    }
}

/// The code of a token endpoint's refusal when it is one that RFC 6749 defines (§5.2), or
/// `another code`, so the logs name a refusal, such as `invalid_client` for a client's secret set
/// wrong, but nothing else that the provider sent (AUTH-077).
fn error_code(error: &str) -> &'static str {
    const CODES: [&str; 6] = [
        "invalid_request",
        "invalid_client",
        "invalid_grant",
        "unauthorized_client",
        "unsupported_grant_type",
        "invalid_scope",
    ];
    CODES
        .into_iter()
        .find(|code| *code == error)
        .unwrap_or("another code")
}
