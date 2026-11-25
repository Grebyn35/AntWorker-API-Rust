//! The requests of a verification of an account's email address, and the code that its job
//! sends (AUTH-028, AUTH-030, AUTH-031), which the tests of several files send and read, and the
//! keyed hash that the job stores of a code that it sends, which they compare (TEST-09).

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

/// The keyed hash, in hex, under the tests' `HMAC_KEY`, of the code of `characters` that the job
/// of `purpose`, such as `email_verification`, sends `address` for `user_id`'s account, as the
/// job computes the hash it stores (SEC-03, AUTH-030, AUTH-036, AUTH-050): of the kind
/// `verification_code`, over the user's ID, the purpose and the address, each of the last two
/// ended by a zero byte, and the characters, a long code's without its hyphens.
pub fn code_hash(user_id: &str, purpose: &str, address: &str, characters: &str) -> String {
    let id = platform::types::read_id(user_id).unwrap();
    let input = [
        id.as_bytes().as_slice(),
        purpose.as_bytes(),
        &[0],
        address.as_bytes(),
        &[0],
        characters.as_bytes(),
    ]
    .concat();
    super::hex(&super::api::hmac_key().hash("verification_code", &input))
}
