//! The requests of a change of an account's email address, and the code that the change's job
//! sends to the new address (AUTH-048, AUTH-050, AUTH-051), which the tests of several files send
//! and read (TEST-09).

use serde_json::json;

use super::{Api, api::Response, mailbox, users::SignedIn};

/// Asks to change `user`'s address to `email`.
pub async fn ask(api: &Api, user: &SignedIn, email: &str) -> Response {
    api.post("/v1/users/me/email/change", &json!({ "email": email }))
        .bearer(&user.access)
        .send()
        .await
}

/// Confirms the change of `user`'s address to `email` with `code`.
pub async fn confirm(api: &Api, user: &SignedIn, email: &str, code: &str) -> Response {
    api.post(
        "/v1/users/me/email/change/confirm",
        &json!({ "email": email, "code": code }),
    )
    .bearer(&user.access)
    .send()
    .await
}

/// The change code in the newest email with one that `address` received, once the API's jobs
/// have run.
pub async fn code_sent(api: &Api, address: &str) -> String {
    api.run_jobs().await;
    let emails = mailbox::to(address).await;
    let email = emails
        .iter()
        .find(|email| email.subject.contains("email change code"))
        .expect("an email with a change code");
    mailbox::grouped_code_in(&email.text).expect("a change code in the email")
}
