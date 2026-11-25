//! Device tokens, the cookies that hold browsers', and the sign-in limit a valid one counts in
//! (AUTH-006, AUTH-015, AUTH-019).

use serde_json::json;

use super::held;
use crate::support::{
    Api, Options,
    api::{PEER, Response, hmac_key, refusal},
    email_change, execute_on, hex, mailbox,
    providers::{self, gives, grant, identity},
    queries_wait_on_locks, second_factors,
    upstream::Upstream,
    users::{self, PASSWORD, SignedIn, WEB, sign_in_from},
};

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

/// Whether each of `tokens` is stored, by its hash, in the order of `tokens`: its lifetime in
/// seconds, or `none`.
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
async fn auth_019_a_device_token_lasts_1_day_to_5_years() {
    let api = Api::with(Options::default().set("DEVICE_TOKEN_SECONDS", 157_680_000)).await;

    let registered = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;

    assert_eq!(
        stored(&api, &[&device_token(&registered)]).await,
        ["157680000"]
    );
    for seconds in [86_399, 157_680_001] {
        assert_eq!(
            refusal(&Options::default().set("DEVICE_TOKEN_SECONDS", seconds)),
            "DEVICE_TOKEN_SECONDS must be a whole number from 86400 to 157680000",
            "{seconds}"
        );
    }
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
    // Another account's expired token.
    let grace = api
        .post("/v1/users", &users::registration("grace@example.com"))
        .send()
        .await;
    let graces = device_token(&grace);
    expire(&api, &graces).await;

    let next = sign_in_from(&api, PEER, "ada@example.com", PASSWORD, Some(&used)).await;
    let again = sign_in_from(&api, PEER, "ada@example.com", PASSWORD, Some(&used)).await;

    assert_eq!(next.status, 201);
    assert_eq!(bucket(&next), "sign_in_device");
    let issued = device_token(&next);
    assert_eq!(
        stored(&api, &[&used, &expired, &issued]).await,
        ["none", "none", "31536000"]
    );
    assert_ne!(
        stored(&api, &[&graces]).await,
        ["none"],
        "another account's expired token stays"
    );
    assert_eq!(again.status, 201);
    assert_eq!(bucket(&again), "sign_in", "an ended token counts as none");
}

#[tokio::test]
async fn auth_006_a_valid_device_token_counts_only_in_its_own_limit() {
    let options = Options::default()
        .set("SIGN_IN_ATTEMPTS", 2)
        .set("SIGN_IN_EMAIL_ATTEMPTS", 4);
    let api = Api::with(options).await;
    let registered = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;
    let token = device_token(&registered);
    // An attacker fills the count of Ada's own address, then the email's from another.
    for peer in [
        "192.0.2.1:50000",
        "192.0.2.1:50000",
        "192.0.2.2:50000",
        "192.0.2.2:50000",
    ] {
        sign_in_from(&api, peer, "ada@example.com", "wrong-password-123", None).await;
    }
    let attacker = sign_in_from(
        &api,
        "192.0.2.4:50000",
        "ada@example.com",
        "wrong-password-123",
        None,
    )
    .await;

    let ada = sign_in_from(
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
            sign_in_from(
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

/// Changes the address of `user`'s account to `email`, with the code that the API's jobs send
/// there (AUTH-048, AUTH-051).
async fn change_address(api: &Api, user: &SignedIn, email: &str) {
    let asked = email_change::ask(api, user, email).await;
    assert_eq!(asked.status, 202, "{}", asked.json());
    let code = email_change::code_sent(api, email).await;
    let confirmed = email_change::confirm(api, user, email, &code).await;
    assert_eq!(confirmed.status, 200, "{}", confirmed.json());
}

#[tokio::test]
async fn auth_019_a_token_got_before_a_change_of_the_address_counts_as_none_for_the_address_before()
{
    let api = Api::new().await;
    let (before, after) = (mailbox::address("ada"), mailbox::address("ada-new"));
    let registered = api
        .post("/v1/users", &users::registration(&before))
        .send()
        .await;
    let token = device_token(&registered);
    change_address(&api, &SignedIn::from(&registered), &after).await;
    // Another account takes the address that Ada's gave up.
    users::register(&api, &before).await;

    let signed_in = sign_in_from(&api, PEER, &before, PASSWORD, Some(&token)).await;

    assert_eq!(signed_in.status, 201);
    assert_eq!(bucket(&signed_in), "sign_in");
    assert_eq!(stored(&api, &[&token]).await, ["none"]);
}

#[tokio::test]
async fn auth_019_a_sign_in_that_waited_for_a_change_of_the_address_gets_a_token_for_the_address_after()
 {
    let api = Api::new().await;
    let (before, after) = (mailbox::address("ada"), mailbox::address("ada-new"));
    let ada = users::register(&api, &before).await;
    let db = api.database();
    // A change of the address that holds the user's row, as its update holds it until it
    // commits, while a sign-in that found the account by the address before inserts its session.
    let mut change = db
        .holding(&format!(
            "UPDATE users SET email = '{after}' WHERE id = '{}'",
            ada.user_id
        ))
        .await;

    let signing_in = {
        let (api, before) = (api.clone(), before.clone());
        tokio::spawn(async move { sign_in_from(&api, PEER, &before, PASSWORD, None).await })
    };
    queries_wait_on_locks(db, 1).await;
    execute_on(&mut change, "COMMIT").await;
    let signed_in = signing_in.await.unwrap();

    assert_eq!(signed_in.status, 201, "{}", signed_in.json());
    let token = device_token(&signed_in);
    users::register(&api, &before).await;
    let for_before = sign_in_from(&api, PEER, &before, PASSWORD, Some(&token)).await;
    let for_after = sign_in_from(&api, PEER, &after, PASSWORD, Some(&token)).await;
    assert_eq!(bucket(&for_before), "sign_in");
    assert_eq!(bucket(&for_after), "sign_in_device");
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
    // An unknown token of 64 characters, the most that the field takes.
    let longest = "x".repeat(64);

    // Ada's expired token is brought first, while it is still stored: the first sign-in of hers
    // that succeeds ends it.
    let expired = sign_in_from(&api, PEER, "ada@example.com", PASSWORD, Some(&ada)).await;
    let other_email = sign_in_from(&api, PEER, "ada@example.com", PASSWORD, Some(&grace)).await;
    let unknown = sign_in_from(&api, PEER, "ada@example.com", PASSWORD, Some(&longest)).await;
    let grace_own = sign_in_from(&api, PEER, "grace@example.com", PASSWORD, Some(&grace)).await;

    for (response, what) in [
        (&expired, "an expired one"),
        (&other_email, "another email's"),
        (&unknown, "an unknown one"),
    ] {
        assert_eq!(response.status, 201, "{what}");
        assert_eq!(bucket(response), "sign_in", "{what}");
    }
    assert_eq!(bucket(&grace_own), "sign_in_device");
}

#[tokio::test]
async fn auth_019_a_device_token_that_is_empty_or_longer_than_64_characters_answers_400_at_its_field()
 {
    let (upstream, api) = providers::standing().await;
    users::register(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "grace@example.com", true));
    let too_long = "x".repeat(65);

    for token in ["", too_long.as_str()] {
        let mut through = grant();
        through["device_token"] = json!(token);
        let with_password =
            json!({ "email": "ada@example.com", "password": PASSWORD, "device_token": token });

        let answers = [
            api.post_raw("/v1/sessions", &with_password).send().await,
            api.post_raw("/v1/sessions/provider", &through).send().await,
        ];

        for response in answers {
            assert_eq!(response.status, 400, "{token}: {}", response.json());
            assert_eq!(
                response.json()["errors"],
                json!([{
                    "field": "/device_token",
                    "code": "invalid_device_token",
                    "message": "The token must be 1 to 64 characters.",
                }])
            );
        }
    }
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
    let name = users::device_cookie(&registered);
    let token = users::cookie(&registered, &name);
    let cookies = users::cookies(&registered);

    let signed_out = users::sign_out(&api)
        .header("origin", WEB)
        .header("cookie", &cookies)
        .send()
        .await;
    let signed_in = held::sign_in(&api, "ada@example.com", &format!("__Host-{name}={token}")).await;

    assert_eq!(signed_out.status, 204);
    assert!(
        signed_out
            .headers("set-cookie")
            .iter()
            .all(|cookie| !cookie.starts_with("__Host-device_token_")),
        "{:?}",
        signed_out.headers("set-cookie")
    );
    assert_eq!(signed_in.status, 201);
    assert_eq!(bucket(&signed_in), "sign_in_device");
    assert!(signed_in.json().get("device_token").is_none());
    assert_eq!(users::device_cookie(&signed_in), name);
    assert_ne!(users::cookie(&signed_in, &name), token);
}

#[tokio::test]
async fn auth_019_a_sign_in_records_the_device_token_it_issues() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;

    let issued = device_token(&users::sign_in(&api, "ada@example.com", PASSWORD).await);

    // One entry: the registration's token goes in `user.registered` (AUTH-001).
    assert_eq!(
        api.database()
            .owner_reads(&format!(
                "SELECT e.user_id || ' ' || e.actor_user_id || ' ' || e.resource_type || ' ' || \
                 (e.resource_id = d.id)::text \
                 FROM audit_entries e LEFT JOIN device_tokens d \
                 ON d.token_hash = sha256(convert_to('{issued}', 'UTF8')) \
                 WHERE e.action = 'device_token.issued'"
            ))
            .await,
        [format!("{0} {0} device_token true", ada.user_id)]
    );
}

/// The entries `device_token.issued` of `user_id`, each as its actor and whether its target is
/// the device token `token`, in the order of their creation.
async fn issued_entries(api: &Api, user_id: &str, token: &str) -> Vec<String> {
    api.database()
        .owner_reads(&format!(
            "SELECT e.actor_user_id || ' ' || e.resource_type || ' ' || \
             coalesce((e.resource_id = d.id)::text, 'false') \
             FROM audit_entries e LEFT JOIN device_tokens d \
             ON d.token_hash = sha256(convert_to('{token}', 'UTF8')) \
             WHERE e.action = 'device_token.issued' AND e.user_id = '{user_id}' \
             ORDER BY e.created_at, e.id"
        ))
        .await
}

/// Makes the device token `token` one that expired a second ago, a time in the past that only a
/// test sets (TEST-09).
async fn expire(api: &Api, token: &str) {
    api.database()
        .owner_executes(&format!(
            "UPDATE device_tokens SET expires_at = now() - interval '1 second' \
             WHERE token_hash = sha256(convert_to('{token}', 'UTF8'))"
        ))
        .await;
}

#[tokio::test]
async fn auth_019_a_completion_of_a_sign_in_ends_the_token_that_its_sign_in_brought_and_issues_the_next()
 {
    let api = Api::with(Options::default().set("DEVICE_TOKEN_SECONDS", 86_400)).await;
    let registered = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .send()
        .await;
    let ada = users::SignedIn::from(&registered);
    users::verified(&api, &ada.user_id).await;
    let used = device_token(&registered);
    let expired = device_token(&users::sign_in(&api, "ada@example.com", PASSWORD).await);
    expire(&api, &expired).await;
    let app = second_factors::add_app(&api, &ada).await;
    let challenged = sign_in_from(&api, PEER, "ada@example.com", PASSWORD, Some(&used)).await;
    assert_eq!(challenged.status, 200, "{}", challenged.json());
    assert!(challenged.json().get("device_token").is_none());
    let challenge = challenged.json()["challenge"].as_str().unwrap().to_owned();

    let completed = second_factors::complete(
        &api,
        &challenge,
        json!({ "code": second_factors::code(&api, &app.secret).await }),
    )
    .await;

    assert_eq!(completed.status, 201, "{}", completed.json());
    let issued = device_token(&completed);
    assert_eq!(issued.len(), 43);
    assert_eq!(
        stored(&api, &[&used, &expired, &issued]).await,
        ["none", "none", "86400"],
        "the token the sign-in brought and the expired one end"
    );
    let entries = issued_entries(&api, &ada.user_id, &issued).await;
    assert_eq!(
        entries.last().unwrap(),
        &format!("{} device_token true", ada.user_id)
    );
}

#[tokio::test]
async fn auth_019_a_sign_in_through_a_provider_issues_a_token_and_ends_the_one_it_brought() {
    let upstream = Upstream::start().await;
    let options = providers::stand_in(&upstream).set("DEVICE_TOKEN_SECONDS", 86_400);
    let api = Api::with(options).await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let registered = providers::sign_in(&api, &grant()).await;
    assert_eq!(registered.status, 201, "{}", registered.json());
    let user_id = registered.json()["user_id"].as_str().unwrap().to_owned();
    let used = device_token(&registered);
    let expired = device_token(&providers::sign_in(&api, &grant()).await);
    expire(&api, &expired).await;
    let mut body = grant();
    body["device_token"] = json!(used);

    let next = providers::sign_in(&api, &body).await;

    assert_eq!(next.status, 201, "{}", next.json());
    let issued = device_token(&next);
    assert_eq!(issued.len(), 43);
    assert_ne!(issued, used);
    assert_eq!(
        stored(&api, &[&used, &expired, &issued]).await,
        ["none", "none", "86400"],
        "the token it brought and the expired one end"
    );
    let plain = api
        .database()
        .owner_reads(&format!(
            "SELECT count(*)::text FROM device_tokens \
             WHERE position(convert_to('{issued}', 'UTF8') IN token_hash) > 0 \
             OR position(convert_to('ada@example.com', 'UTF8') IN email_hash) > 0"
        ))
        .await;
    assert_eq!(plain, ["0"], "stored only as its hash");
    let entries = issued_entries(&api, &user_id, &issued).await;
    assert_eq!(
        entries.last().unwrap(),
        &format!("{user_id} device_token true")
    );
}

#[tokio::test]
async fn auth_015_a_browser_keeps_a_device_token_for_each_account_it_signs_in_to() {
    let api = users::browsers_api().await;
    let emails = ["ada@example.com", "bob@example.com"];
    let mut names = Vec::new();
    let mut jar = Vec::new();
    for email in emails {
        let registered = api
            .post("/v1/users", &users::registration(email))
            .header("origin", WEB)
            .send()
            .await;
        assert_eq!(registered.status, 201);
        let name = users::device_cookie(&registered);
        jar.push(format!(
            "__Host-{name}={}",
            users::cookie(&registered, &name)
        ));
        names.push(name);
    }

    let signed_in = held::sign_in(&api, "ada@example.com", &jar.join("; ")).await;

    assert_ne!(
        names[0], names[1],
        "each account's token has a cookie of its own"
    );
    assert!(
        names
            .iter()
            .all(|name| name.len() == "device_token_".len() + 16)
    );
    // The first 16 hexadecimal digits of the email's keyed hash under the tests' `HMAC_KEY`, of
    // the kind that names a device token's cookie.
    let keyed: Vec<String> = emails
        .iter()
        .map(|email| {
            let hash = hex(&hmac_key().hash("device_token_cookie", email.as_bytes()));
            format!("device_token_{}", &hash[..16])
        })
        .collect();
    assert_eq!(names, keyed, "a keyed hash of each email");
    assert_eq!(signed_in.status, 201);
    assert_eq!(
        bucket(&signed_in),
        "sign_in_device",
        "Ada's token, after Bob's"
    );
    assert_eq!(users::device_cookie(&signed_in), names[0]);
}

#[tokio::test]
async fn auth_015_a_sign_in_reads_its_email_s_cookie_when_its_body_holds_no_token_with_or_without_origin()
 {
    let api = users::browsers_api().await;
    let registered = api
        .post("/v1/users", &users::registration("ada@example.com"))
        .header("origin", WEB)
        .send()
        .await;
    let name = users::device_cookie(&registered);
    let in_cookie = users::cookie(&registered, &name);
    let jar = format!("__Host-{name}={in_cookie}");
    // A second valid token, which a sign-in without `Origin` takes in its body.
    let in_body = device_token(&users::sign_in(&api, "ada@example.com", PASSWORD).await);

    let body_first = api
        .post(
            "/v1/sessions",
            &json!({ "email": "ada@example.com", "password": PASSWORD, "device_token": in_body }),
        )
        .header("cookie", &jar)
        .send()
        .await;
    let held = stored(&api, &[&in_body, &in_cookie]).await;
    let without_origin = api
        .post(
            "/v1/sessions",
            &json!({ "email": "ada@example.com", "password": PASSWORD }),
        )
        .header("cookie", &jar)
        .send()
        .await;

    assert_eq!(body_first.status, 201);
    assert_eq!(
        held,
        ["none", "31536000"],
        "the body's token is read, not the cookie's"
    );
    assert_eq!(without_origin.status, 201, "{}", without_origin.json());
    assert_eq!(bucket(&without_origin), "sign_in_device");
    assert_eq!(stored(&api, &[&in_cookie]).await, ["none"]);
}
