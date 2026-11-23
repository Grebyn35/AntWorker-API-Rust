//! The requests of a verification of an account's email address, and the code that its job
//! sends (AUTH-028, AUTH-030, AUTH-031), which the tests of several files send and read
//! (TEST-09).

use serde_json::json;

use super::{Api, api::Response, mailbox, users::SignedIn};

/// Asks for a code for `user`'s account.
pub async fn ask(api: &Api, user: &SignedIn) -> Response {
    api.request(http::Method::POST, "/v1/users/me/email/verification")
        .bearer(&user.access)
        .send()
        .await
}

/// Confirms `code` for `user`'s account.
pub async fn confirm(api: &Api, user: &SignedIn, code: &str) -> Response {
    api.post(
        "/v1/users/me/email/verification/confirm",
        &json!({ "code": code }),
    )
    .bearer(&user.access)
    .send()
    .await
}

/// The code in the newest email that `address` received, once the API's jobs have run.
pub async fn code_sent(api: &Api, address: &str) -> String {
    api.run_jobs().await;
    let emails = mailbox::to(address).await;
    let email = emails.first().expect("an email with a code");
    mailbox::digits_code_in(&email.text).expect("a code in the email")
}
