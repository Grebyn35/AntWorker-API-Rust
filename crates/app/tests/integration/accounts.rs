//! The user record (the accounts spec).

use serde_json::json;

use crate::support::{
    Api,
    users::{self, PASSWORD},
};

#[tokio::test]
async fn accounts_001_the_caller_reads_their_own_record_and_no_other_user_s() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    users::register(&api, "grace@example.com").await;

    let response = api.get("/v1/users/me").bearer(&ada.access).send().await;

    assert_eq!(response.status, 200);
    assert_eq!(
        response.json(),
        json!({
            "id": ada.user_id,
            "email": "ada@example.com",
            "name": "Ada Lovelace",
            "email_verified": false,
        })
    );
}

#[tokio::test]
async fn accounts_002_an_email_address_is_stored_trimmed_and_lowercased() {
    let api = Api::new().await;
    users::register(&api, "  Ada@Example.COM ").await;

    let stored = api.database().owner_reads("SELECT email FROM users").await;
    assert_eq!(stored, ["ada@example.com"]);
}

#[tokio::test]
async fn accounts_002_an_invalid_email_address_is_refused() {
    let api = Api::new().await;

    let response = api
        .post("/v1/users", &users::registration("not-an-address"))
        .send()
        .await;

    assert_eq!(response.status, 400);
    let body = response.json();
    assert_eq!(body["code"], "request.invalid_fields");
    assert_eq!(
        body["errors"],
        json!([{ "field": "/email", "code": "invalid_email", "message": "This isn't an email address." }])
    );
}

#[tokio::test]
async fn accounts_003_a_display_name_is_stored_trimmed_collapsed_and_normalized() {
    let api = Api::new().await;
    let body = json!({
        "email": "ada@example.com",
        "password": PASSWORD,
        "name": "  Rene\u{301}e \t  Lovelace ",
    });

    let response = api.post("/v1/users", &body).send().await;

    assert_eq!(response.status, 201);
    assert_eq!(response.json()["user"]["name"], "Ren\u{e9}e Lovelace");
}

#[tokio::test]
async fn accounts_004_no_two_users_have_the_same_email() {
    let api = Api::new().await;
    users::register(&api, "ada@example.com").await;

    let response = api
        .post("/v1/users", &users::registration("ADA@example.com"))
        .send()
        .await;

    assert_eq!(response.status, 409);
    let count = api
        .database()
        .owner_reads("SELECT count(*)::text FROM users")
        .await;
    assert_eq!(count, ["1"]);
}
