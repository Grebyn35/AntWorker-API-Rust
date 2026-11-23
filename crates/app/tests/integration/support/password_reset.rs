//! The request for a reset of a forgotten password, and the reset code that its job sends
//! (AUTH-034, AUTH-036), which the tests of several files send and read (TEST-09).

use serde_json::json;

use super::{Api, api::Response, mailbox};

/// Asks for a reset of `email`'s password.
pub async fn ask(api: &Api, email: &str) -> Response {
    api.post("/v1/password-resets", &json!({ "email": email }))
        .send()
        .await
}

/// The reset code in the newest reset email that `address` received, once the API's jobs have
/// run.
pub async fn code_sent(api: &Api, address: &str) -> String {
    api.run_jobs().await;
    let emails = mailbox::to(address).await;
    let email = emails
        .iter()
        .find(|email| email.subject.contains("password reset code"))
        .expect("an email with a reset code");
    mailbox::grouped_code_in(&email.text).expect("a reset code in the email")
}
