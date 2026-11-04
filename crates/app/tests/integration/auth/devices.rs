//! Device tokens, and the sign-in limit a valid one counts in (AUTH-006, AUTH-019).

use serde_json::json;

use crate::support::{
    Api, Options,
    api::{PEER, Response},
    users::{self, PASSWORD, WEB},
};

/// Signs `email` in with `password` from the client address `peer`, with `device_token` in the
/// body when given.
async fn sign_in(
    api: &Api,
    peer: &str,
    email: &str,
    password: &str,
    device_token: Option<&str>,
) -> Response {
    let mut body = json!({ "email": email, "password": password });
    if let Some(token) = device_token {
        body["device_token"] = json!(token);
    }
    api.post("/v1/sessions", &body).from(peer).send().await
}

/// The device token in `response`'s body.
fn device_token(response: &Response) -> String {
    let token = response.json()["device_token"].as_str().map(str::to_owned);
    assert!(token.is_some(), "{}", response.json());
    token.unwrap()
}

/// The bucket that `response`'s rate-limit headers describe: the one closest to refusing it.
fn bucket(response: &Response) -> String {
    let policy = response.header("ratelimit-policy");
    policy.split('"').nth(1).unwrap_or_default().to_owned()
}

/// Whether each of `tokens` is stored, by its hash, in the order of the rows' creation, each with
/// its lifetime in seconds.
async fn stored(api: &Api, tokens: &[&str]) -> Vec<String> {
    let mut rows = Vec::new();
    for token in tokens {
        let row = api
            .database()
            .owner_reads(&format!(
                "SELECT round(extract(epoch FROM expires_at - created_at))::text FROM device_tokens \
                 WHERE token_hash = sha256(convert_to('{token}', 'UTF8'))"
            ))
            .await;
        rows.push(row.first().cloned().unwrap_or_else(|| "none".to_owned()));
    }
    rows
}

#[tokio::test]
async fn auth_019_a_registration_and_a_sign_in_issue_a_device_token_stored_only_as_its_hash() {
    let api = Api::with(Options::default().set("DEVICE_TOKEN_SECONDS", 86_400)).await;

    let registered = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;
    let signed_in = users::sign_in(&api, "ada@example.com", PASSWORD).await;

    let (first, second) = (device_token(&registered), device_token(&signed_in));
    assert_eq!(first.len(), 43);
    assert_ne!(first, second);
    assert_eq!(stored(&api, &[&first, &second]).await, ["86400", "86400"]);
    let plain = api
        .database()
        .owner_reads(&format!(
            "SELECT count(*)::text FROM device_tokens \
             WHERE position(convert_to('{first}', 'UTF8') IN token_hash) > 0 \
             OR position(convert_to('ada@example.com', 'UTF8') IN email_hash) > 0"
        ))
        .await;
    assert_eq!(plain, ["0"]);
}

#[tokio::test]
async fn auth_019_a_sign_in_with_a_valid_token_ends_it_and_the_account_s_expired_ones() {
    let api = Api::new().await;
    let registered = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;
    let used = device_token(&registered);
    let expired = device_token(&users::sign_in(&api, "ada@example.com", PASSWORD).await);
    api.database()
        .owner_executes(&format!(
            "UPDATE device_tokens SET expires_at = now() - interval '1 second' \
             WHERE token_hash = sha256(convert_to('{expired}', 'UTF8'))"
        ))
        .await;

    let next = sign_in(&api, PEER, "ada@example.com", PASSWORD, Some(&used)).await;
    let again = sign_in(&api, PEER, "ada@example.com", PASSWORD, Some(&used)).await;

    assert_eq!(next.status, 201);
    assert_eq!(bucket(&next), "sign_in_device");
    let issued = device_token(&next);
    assert_eq!(
        stored(&api, &[&used, &expired, &issued]).await,
        ["none", "none", "31536000"]
    );
    assert_eq!(again.status, 201);
    assert_eq!(bucket(&again), "sign_in", "an ended token counts as none");
}

#[tokio::test]
async fn auth_006_a_valid_device_token_counts_only_in_its_own_limit() {
    let options = Options::default()
        .set("SIGN_IN_ATTEMPTS", 2)
        .set("SIGN_IN_EMAIL_ATTEMPTS", 2);
    let api = Api::with(options).await;
    let registered = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;
    let token = device_token(&registered);
    // An attacker at Ada's own address fills that address's count and the email's.
    for _ in 0..2 {
        sign_in(
            &api,
            "192.0.2.1:50000",
            "ada@example.com",
            "wrong-password-123",
            None,
        )
        .await;
    }
    let attacker = sign_in(
        &api,
        "192.0.2.2:50000",
        "ada@example.com",
        "wrong-password-123",
        None,
    )
    .await;

    let ada = sign_in(
        &api,
        "192.0.2.1:50000",
        "ada@example.com",
        PASSWORD,
        Some(&token),
    )
    .await;
    let next = device_token(&ada);
    let mut guesses = Vec::new();
    for _ in 0..3 {
        guesses.push(
            sign_in(
                &api,
                "192.0.2.3:50000",
                "ada@example.com",
                "wrong-password-123",
                Some(&next),
            )
            .await,
        );
    }

    assert_eq!(attacker.status, 429);
    assert_eq!(bucket(&attacker), "sign_in_email");
    assert_eq!(ada.status, 201);
    assert_eq!(bucket(&ada), "sign_in_device");
    let statuses: Vec<u16> = guesses.iter().map(|guess| guess.status.as_u16()).collect();
    assert_eq!(statuses, [401, 401, 429], "a stolen token's own limit");
    assert!(
        guesses[2]
            .header("ratelimit-policy")
            .starts_with("\"sign_in_device\";q=2;w=900")
    );
}

#[tokio::test]
async fn auth_019_a_token_of_another_email_an_expired_or_an_unknown_one_counts_as_none() {
    let api = Api::new().await;
    let ada = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;
    let grace = api
        .post("/v1/users", &users::registration("grace@example.com"))
        .send()
        .await;
    let (ada, grace) = (device_token(&ada), device_token(&grace));
    api.database()
        .owner_executes(&format!(
            "UPDATE device_tokens SET expires_at = now() - interval '1 second' \
             WHERE token_hash = sha256(convert_to('{ada}', 'UTF8'))"
        ))
        .await;

    let other_email = sign_in(&api, PEER, "ada@example.com", PASSWORD, Some(&grace)).await;
    let expired = sign_in(&api, PEER, "ada@example.com", PASSWORD, Some(&ada)).await;
    let unknown = sign_in(&api, PEER, "ada@example.com", PASSWORD, Some("nonsense")).await;
    let grace_own = sign_in(&api, PEER, "grace@example.com", PASSWORD, Some(&grace)).await;

    for (response, what) in [
        (&other_email, "another email's"),
        (&expired, "an expired one"),
        (&unknown, "an unknown one"),
    ] {
        assert_eq!(response.status, 201, "{what}");
        assert_eq!(bucket(response), "sign_in", "{what}");
    }
    assert_eq!(bucket(&grace_own), "sign_in_device");
}

#[tokio::test]
async fn auth_019_a_browser_s_sign_in_reads_its_device_token_from_its_cookie_which_signing_out_leaves()
 {
    let api = users::browsers_api().await;
    let registered = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .header("origin", WEB)
        .send()
        .await;
    let token = users::cookie(&registered, "device_token");
    let cookies = users::cookies(&registered);

    let signed_out = api
        .request(http::Method::DELETE, "/v1/users/me/sessions/current")
        .header("origin", WEB)
        .header("cookie", &cookies)
        .send()
        .await;
    let signed_in = api
        .post(
            "/v1/sessions",
            &json!({ "email": "ada@example.com", "password": PASSWORD }),
        )
        .header("origin", WEB)
        .header("cookie", &format!("__Host-device_token={token}"))
        .send()
        .await;

    assert_eq!(signed_out.status, 204);
    assert!(
        signed_out
            .headers("set-cookie")
            .iter()
            .all(|cookie| !cookie.starts_with("__Host-device_token=")),
        "{:?}",
        signed_out.headers("set-cookie")
    );
    assert_eq!(signed_in.status, 201);
    assert_eq!(bucket(&signed_in), "sign_in_device");
    assert!(signed_in.json().get("device_token").is_none());
    assert_ne!(users::cookie(&signed_in, "device_token"), token);
}
