//! Builders of second factors, through the API's own routes (TEST-09), and the codes of an
//! authenticator app, computed as an app computes them, by the database's clock (AUTH-057).

use serde_json::{Value, json};

use super::{
    Api,
    api::Response,
    users::{PASSWORD, SignedIn},
};

/// An authenticator app that a test set up for a user: its secret in base32, and the recovery
/// codes that its confirmation returned, in groups.
pub struct App {
    pub secret: String,
    pub recovery_codes: Vec<String>,
}

/// The seconds since the Unix epoch, by the database's clock, as the API's check reads them.
async fn now(api: &Api) -> u64 {
    api.database()
        .owner_reads("SELECT floor(extract(epoch FROM now()))::int8::text")
        .await
        .remove(0)
        .parse()
        .unwrap()
}

/// The code that an app with `secret`, in base32, shows now, by the database's clock.
pub async fn code(api: &Api, secret: &str) -> String {
    code_at(secret, now(api).await)
}

/// The code that an app with `secret`, in base32, shows at `at`, seconds since the Unix epoch.
pub fn code_at(secret: &str, at: u64) -> String {
    let bytes = data_encoding::BASE32_NOPAD
        .decode(secret.as_bytes())
        .expect("a secret in base32");
    let bytes: [u8; 20] = bytes.try_into().expect("a secret of 20 bytes");
    totp_rs::Builder::new()
        .with_algorithm(totp_rs::Algorithm::SHA1)
        .with_digits(6)
        .with_skew(0)
        .with_step_duration(30)
        .with_secret(totp_rs::Secret::new_stack(bytes))
        .build()
        .expect("an app's parameters")
        .generate(at)
        .to_string()
}

/// A code of 6 digits that isn't `code`.
pub fn other_code(code: &str) -> String {
    let first = if code.starts_with('9') { '8' } else { '9' };
    format!("{first}{}", &code[1..])
}

/// Starts a setup of an app for `user`, whose session proved their password just before.
pub async fn start_setup(api: &Api, user: &SignedIn) -> Response {
    api.request(http::Method::POST, "/v1/users/me/second-factors/totp/setup")
        .bearer(&user.access)
        .send()
        .await
}

/// Confirms `user`'s setup with `code`.
pub async fn confirm(api: &Api, user: &SignedIn, code: &str) -> Response {
    api.post(
        "/v1/users/me/second-factors/totp/setup/confirm",
        &json!({ "code": code }),
    )
    .bearer(&user.access)
    .send()
    .await
}

/// Sets up and confirms an app for `user`, whose session proved their password within the last
/// 5 minutes, as a registration's has, then forgets the step that the confirmation used, so the
/// app's next code works at once, rather than in the next step (AUTH-057).
pub async fn add_app(api: &Api, user: &SignedIn) -> App {
    let setup = start_setup(api, user).await;
    assert_eq!(setup.status, 200, "{}", setup.json());
    let secret = setup.json()["secret"].as_str().unwrap().to_owned();
    let confirmed = confirm(api, user, &code(api, &secret).await).await;
    assert_eq!(confirmed.status, 200, "{}", confirmed.json());
    forget_steps(api, &user.user_id).await;
    let recovery_codes = confirmed.json()["recovery_codes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|code| code.as_str().unwrap().to_owned())
        .collect();
    App {
        secret,
        recovery_codes,
    }
}

/// Makes `user_id`'s app one that accepted no step since an hour ago, through the `owner`
/// connection, so a test checks the app's codes again without waiting for the next step.
pub async fn forget_steps(api: &Api, user_id: &str) {
    api.database()
        .owner_executes(&format!(
            "UPDATE totp_factors SET last_used_step = last_used_step - 120 \
             WHERE user_id = '{user_id}'"
        ))
        .await;
}

/// Signs `email` in with [`PASSWORD`], for an account with an app, and returns the challenge.
pub async fn challenge(api: &Api, email: &str) -> String {
    let response = super::users::sign_in(api, email, PASSWORD).await;
    assert_eq!(response.status, 200, "{}", response.json());
    response.json()["challenge"].as_str().unwrap().to_owned()
}

/// Completes the sign-in of `challenge` with `body`'s code, and returns the response.
pub async fn complete(api: &Api, challenge: &str, body: Value) -> Response {
    let mut body = body;
    body["challenge"] = json!(challenge);
    api.post("/v1/sessions/second-factor", &body).send().await
}
