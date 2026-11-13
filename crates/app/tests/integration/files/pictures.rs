//! The picture that a provider gives for a new account, copied as the account's avatar
//! (ACCOUNTS-028, AUTH-092), through the tests' stand-in provider, which also serves the picture.

use platform::telemetry::testing;
use serde_json::{Value, json};

use crate::support::{
    Api,
    api::Options,
    providers::{self, gives, grant, sign_in, signed_in},
    scanner::{self, Script},
    upstream::Upstream,
    users::{self, SignedIn},
    workspaces,
};

/// The smallest PNG's bytes: its signature, then its header chunk.
const PNG: &[u8] =
    b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";

/// The path of Ada's picture at the stand-in.
const PICTURE: &str = "/pictures/ada.png";

/// A stand-in provider, and an API that signs in through it, with `options`.
async fn standing(options: Options) -> (Upstream, Api) {
    let upstream = Upstream::start().await;
    let mut settings = providers::stand_in(&upstream);
    settings.settings.extend(options.settings);
    let api = Api::with(settings).await;
    (upstream, api)
}

/// Makes the stand-in give a new user, Ada, whose address it proves, with `picture` as the
/// address of her picture.
fn pictured(upstream: &Upstream, picture: &str) {
    gives(
        upstream,
        json!({
            "sub": "subject-1",
            "email": "ada@example.com",
            "proved": true,
            "name": "Ada Lovelace",
            "picture": picture,
        }),
    );
}

/// Registers Ada through the stand-in, which serves `bytes` as her picture.
async fn registered(upstream: &Upstream, api: &Api, bytes: &[u8]) -> SignedIn {
    upstream.set_bytes("GET", PICTURE, 200, bytes);
    pictured(upstream, &upstream.url(PICTURE));
    signed_in(api).await
}

/// The link of `user`'s avatar, or null.
async fn avatar_url(api: &Api, user: &SignedIn) -> Value {
    let record = api.get("/v1/users/me").bearer(&user.access).send().await;
    assert_eq!(record.status, 200, "{}", record.json());
    record.json()["avatar_url"].clone()
}

/// The first column of each row of `sql`, as the `owner` reads it.
async fn reads(api: &Api, sql: &str) -> Vec<String> {
    api.database().owner_reads(sql).await
}

/// The jobs that copy a picture, each as its state, its attempts and its payload's address.
async fn copies(api: &Api) -> Vec<String> {
    reads(
        api,
        "SELECT state::text || ' ' || attempts || ' ' || (payload->>'url') FROM jobs \
         WHERE kind = 'accounts.copy_picture' ORDER BY created_at, id",
    )
    .await
}

/// How many errors the API has ignored as a picture that wasn't copied, in every test that runs
/// alongside (ERR-11).
fn not_copied() -> f64 {
    testing::recorded("platform.errors_ignored")
        .into_iter()
        .filter(|point| {
            point.labels.get("error.type").map(String::as_str) == Some("picture_not_copied")
        })
        .map(|point| point.value)
        .sum()
}

#[tokio::test]
async fn accounts_028_a_registration_through_a_provider_gets_its_picture_as_its_avatar() {
    let (upstream, api) = standing(Options::default()).await;
    let ada = registered(&upstream, &api, PNG).await;
    workspaces::member_of_many(api.database(), &ada.user_id, 1, "owner").await;
    let before = api.get("/v1/users/me").bearer(&ada.access).send().await;
    assert_eq!(before.json()["avatar_url"], Value::Null);
    api.database().owner_executes("DELETE FROM events").await;

    api.run_jobs().await;

    let after = api.get("/v1/users/me").bearer(&ada.access).send().await;
    let url = after.json()["avatar_url"].as_str().unwrap().to_owned();
    assert_ne!(after.header("etag"), before.header("etag"));
    let picture = api.get(&url).send().await;
    assert_eq!(picture.status, 200);
    assert_eq!(picture.header("content-type"), "image/png");
    assert_eq!(&picture.body[..], PNG);
    let id = url.rsplit('/').next().unwrap();
    assert_eq!(
        reads(
            &api,
            "SELECT user_id || ' ' || coalesce(actor_user_id::text, '-') || ' ' || resource_type \
             || ' ' || resource_id FROM audit_entries WHERE action = 'avatar.copied'"
        )
        .await,
        [format!("{} - avatar {id}", ada.user_id)]
    );
    assert_eq!(
        reads(
            &api,
            "SELECT name || ' ' || (payload->>'user_id') FROM events ORDER BY name"
        )
        .await,
        [
            format!("member.updated {}", ada.user_id),
            format!("user.updated {}", ada.user_id),
        ]
    );
    assert!(
        copies(&api).await.is_empty(),
        "a job that ran leaves no row"
    );
}

#[tokio::test]
async fn accounts_028_a_second_run_of_the_job_changes_nothing() {
    let (upstream, api) = standing(Options::default()).await;
    let ada = registered(&upstream, &api, PNG).await;
    api.run_jobs().await;
    let first = avatar_url(&api, &ada).await;
    // The job again, as a second delivery runs it.
    api.database()
        .owner_executes(&format!(
            "INSERT INTO jobs (kind, payload, user_id) VALUES ('accounts.copy_picture', \
             '{}', '{}')",
            json!({ "url": upstream.url(PICTURE) }),
            ada.user_id
        ))
        .await;

    api.run_jobs().await;

    assert!(first.is_string());
    assert_eq!(avatar_url(&api, &ada).await, first);
    assert_eq!(
        reads(
            &api,
            "SELECT count(*)::text FROM audit_entries WHERE action = 'avatar.copied'"
        )
        .await,
        ["1"]
    );
    assert!(copies(&api).await.is_empty());
}

#[tokio::test]
async fn accounts_028_an_account_that_set_an_avatar_meanwhile_keeps_it() {
    let (upstream, api) = standing(Options::default()).await;
    let ada = registered(&upstream, &api, PNG).await;
    let set = api
        .request(http::Method::PUT, "/v1/users/me/avatar")
        .header("content-type", "application/octet-stream")
        .body(PNG.to_vec())
        .bearer(&ada.access)
        .send()
        .await;
    assert_eq!(set.status, 200, "{}", set.json());
    let own = set.json()["avatar_url"].clone();

    api.run_jobs().await;

    assert_eq!(avatar_url(&api, &ada).await, own);
    assert!(
        reads(
            &api,
            "SELECT id::text FROM audit_entries WHERE action = 'avatar.copied'"
        )
        .await
        .is_empty()
    );
    assert!(copies(&api).await.is_empty());
}

#[tokio::test]
async fn accounts_028_a_picture_that_is_no_avatar_leaves_the_account_without_one() {
    // Bytes that are no image, more bytes than an avatar holds, and an answer that says the
    // address gives nothing.
    let large = [PNG, &vec![0; 1_048_576]].concat();
    for (status, bytes) in [(200, b"hello".as_slice()), (200, &large), (404, b"")] {
        let (upstream, api) = standing(Options::default()).await;
        upstream.set_bytes("GET", PICTURE, status, bytes);
        pictured(&upstream, &upstream.url(PICTURE));
        let ada = signed_in(&api).await;
        let ignored = not_copied();

        api.run_jobs().await;

        assert_eq!(avatar_url(&api, &ada).await, Value::Null, "{status}");
        assert!(copies(&api).await.is_empty(), "no retry and no dead letter");
        assert!(not_copied() >= ignored + 1.0, "ERR-11's count");
    }
}

#[tokio::test]
async fn accounts_028_a_picture_in_which_the_scanner_finds_malware_leaves_no_avatar() {
    let stand_in = scanner::scripted(Script::Answer("stream: Eicar-Test-Signature FOUND")).await;
    let options = Options::default().set("CLAMD_ADDRESS", &stand_in.address);
    let (upstream, api) = standing(options).await;
    let ada = registered(&upstream, &api, PNG).await;

    api.run_jobs().await;

    assert_eq!(avatar_url(&api, &ada).await, Value::Null);
    assert!(copies(&api).await.is_empty());
    let logged = testing::all_logged()
        .into_iter()
        .filter(|line| line["error.type"] == "picture_not_copied")
        .count();
    assert!(logged >= 1, "ERR-11's line");
}

#[tokio::test]
async fn accounts_028_an_address_that_the_untrusted_client_refuses_is_never_fetched() {
    // A loopback address that isn't the tests' origin, a name that resolves to one, and an
    // address that isn't HTTPS (SEC-10).
    for picture in [
        "https://127.0.0.1/pictures/ada.png",
        "https://localhost/pictures/ada.png",
        "http://pictures.example.com/ada.png",
    ] {
        let (upstream, api) = standing(Options::default()).await;
        upstream.set_bytes("GET", PICTURE, 200, PNG);
        pictured(&upstream, picture);
        let ada = signed_in(&api).await;
        let ignored = not_copied();

        api.run_jobs().await;

        assert_eq!(avatar_url(&api, &ada).await, Value::Null, "{picture}");
        assert!(copies(&api).await.is_empty(), "no retry and no dead letter");
        assert!(not_copied() >= ignored + 1.0, "ERR-11's count");
        assert!(
            upstream
                .received()
                .iter()
                .all(|request| request.path != PICTURE)
        );
    }
}

#[tokio::test]
async fn accounts_028_a_fetch_that_fails_is_tried_again_and_the_fifth_leaves_no_dead_letter() {
    let (upstream, api) = standing(Options::default()).await;
    upstream.set_bytes("GET", PICTURE, 503, b"");
    pictured(&upstream, &upstream.url(PICTURE));
    let ada = signed_in(&api).await;

    api.run_jobs().await;
    let retried = reads(
        &api,
        "SELECT state::text || ' ' || attempts || ' ' || (run_at > now())::text FROM jobs \
         WHERE kind = 'accounts.copy_picture'",
    )
    .await;
    // Its fifth attempt, the last.
    api.database()
        .owner_executes("UPDATE jobs SET attempts = 4, run_at = now()")
        .await;
    let ignored = not_copied();
    api.run_jobs().await;

    assert_eq!(retried, ["waiting 1 true"]);
    assert!(copies(&api).await.is_empty(), "no dead letter");
    assert!(not_copied() >= ignored + 1.0, "ERR-11's count");
    assert_eq!(avatar_url(&api, &ada).await, Value::Null);
}

#[tokio::test]
async fn accounts_028_a_scanner_that_doesn_t_answer_is_tried_again_until_the_fifth_attempt() {
    let stand_in = scanner::scripted(Script::Close).await;
    let options = Options::default().set("CLAMD_ADDRESS", &stand_in.address);
    let (upstream, api) = standing(options).await;
    let ada = registered(&upstream, &api, PNG).await;

    api.run_jobs().await;

    let retried = reads(
        &api,
        "SELECT state::text || ' ' || attempts FROM jobs \
         WHERE kind = 'accounts.copy_picture'",
    )
    .await;
    // Its fifth attempt, the last.
    api.database()
        .owner_executes("UPDATE jobs SET attempts = 4, run_at = now()")
        .await;
    let ignored = not_copied();
    api.run_jobs().await;

    assert_eq!(retried, ["waiting 1"]);
    assert!(copies(&api).await.is_empty(), "no dead letter");
    assert!(not_copied() >= ignored + 1.0, "ERR-11's count");
    assert_eq!(avatar_url(&api, &ada).await, Value::Null);
}

#[tokio::test]
async fn accounts_028_a_storage_that_doesn_t_answer_is_tried_again_until_the_fifth_attempt() {
    // Nothing listens on port 9 of the loopback address.
    let options = Options::default().set("S3_ENDPOINT", "http://127.0.0.1:9");
    let (upstream, api) = standing(options).await;
    let ada = registered(&upstream, &api, PNG).await;

    api.run_jobs().await;

    let retried = reads(
        &api,
        "SELECT state::text || ' ' || attempts FROM jobs \
         WHERE kind = 'accounts.copy_picture'",
    )
    .await;
    // Its fifth attempt, the last.
    api.database()
        .owner_executes("UPDATE jobs SET attempts = 4, run_at = now()")
        .await;
    let ignored = not_copied();
    api.run_jobs().await;

    assert_eq!(retried, ["waiting 1"]);
    assert!(copies(&api).await.is_empty(), "no dead letter");
    assert!(not_copied() >= ignored + 1.0, "ERR-11's count");
    assert_eq!(avatar_url(&api, &ada).await, Value::Null);
}

#[tokio::test]
async fn accounts_028_an_address_of_more_than_2048_characters_enqueues_nothing() {
    let (upstream, api) = standing(Options::default()).await;
    let long = format!("{}/{}", upstream.url("/pictures"), "a".repeat(2_048));
    pictured(&upstream, &long);

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert!(copies(&api).await.is_empty());
}

#[tokio::test]
async fn auth_092_a_registration_alone_hands_the_provider_s_picture_on() {
    let (upstream, api) = standing(Options::default()).await;
    let picture = upstream.url(PICTURE);
    pictured(&upstream, &picture);

    let ada = signed_in(&api).await;
    let handed = reads(
        &api,
        "SELECT kind || ' ' || user_id || ' ' || (payload->>'url') FROM jobs \
         WHERE kind = 'accounts.copy_picture'",
    )
    .await;
    api.database()
        .owner_executes("DELETE FROM jobs WHERE kind = 'accounts.copy_picture'")
        .await;
    // A sign-in to the account that the subject signs in to.
    let again = sign_in(&api, &grant()).await;

    assert_eq!(
        handed,
        [format!("accounts.copy_picture {} {picture}", ada.user_id)]
    );
    assert_eq!(again.status, 201, "{}", again.json());
    assert!(copies(&api).await.is_empty());
}

#[tokio::test]
async fn auth_092_a_link_hands_no_picture_on() {
    let (upstream, api) = standing(Options::default()).await;
    let ada = users::register(&api, "ada@example.com").await;
    pictured(&upstream, &upstream.url(PICTURE));

    let linked = api
        .post("/v1/users/me/identities/provider", &grant())
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(linked.status, 200, "{}", linked.json());
    assert!(copies(&api).await.is_empty());
}

#[tokio::test]
async fn auth_092_a_provider_that_gives_no_picture_hands_none_on() {
    let (upstream, api) = standing(Options::default()).await;
    gives(
        &upstream,
        providers::identity("subject-1", "ada@example.com", true),
    );

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert!(copies(&api).await.is_empty());
}
