//! The picture that a provider gives for a new account, copied as the account's avatar
//! (ACCOUNTS-028, AUTH-092), from the hosts that the provider names (AUTH-097), through the tests'
//! stand-in provider, which also serves the picture.

use platform::telemetry::testing;
use serde_json::{Value, json};

use super::{objects, uploads};
use crate::support::{
    Api,
    api::{self, Options},
    execute_on, files,
    providers::{self, gives, grant, sign_in, signed_in},
    query_waits_on_a_lock,
    scanner::{self, Script},
    upstream::Upstream,
    users::{self, SignedIn},
    workspaces,
};

/// The path of Ada's picture at the stand-in.
const PICTURE: &str = "/pictures/ada.png";

/// The setting that lists the hosts of the pictures of the stand-in's users.
const HOSTS_SETTING: &str = "STAND_IN_PICTURE_HOSTS";

/// The hosts that the stand-in's settings list here: a name that never resolves, and one that
/// resolves on the machine alone, to a loopback address. The untrusted client accepts the tests'
/// origin, which serves the picture, beside them (SEC-10).
const HOSTS: &str = "pictures.provider.test, localhost";

/// A stand-in provider, which lists [`HOSTS`], and an API that signs in through it, with
/// `options`.
async fn standing(options: Options) -> (Upstream, Api) {
    let upstream = Upstream::start().await;
    let mut settings = providers::stand_in(&upstream).set(HOSTS_SETTING, HOSTS);
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

/// How many times the stand-in was asked for Ada's picture.
fn fetches(upstream: &Upstream) -> usize {
    let received = upstream.received();
    received.iter().filter(|got| got.path == PICTURE).count()
}

/// Registers Ada through the stand-in, then runs the job that copies her picture while another
/// change of her avatar holds her row: the job has fetched and stored the picture when its unit
/// of work waits for the row, and the other change gives her an avatar before it commits. Gives
/// Ada, and the link of the avatar that the other change gave her.
async fn overtaken(upstream: &Upstream, api: &Api) -> (SignedIn, String) {
    let ada = registered(upstream, api, files::PNG).await;
    let db = api.database();
    let mut change = db
        .holding(&format!(
            "SELECT 1 FROM users WHERE id = '{}' FOR UPDATE",
            ada.user_id
        ))
        .await;

    let ((), runs) = tokio::join!(
        async {
            query_waits_on_a_lock(db, "SELECT id FROM users WHERE id = $1 FOR UPDATE").await;
            execute_on(
                &mut change,
                &format!(
                    "INSERT INTO avatars (id, user_id, media_type, size_bytes, crc32, object_key) \
                     VALUES ('{OTHER_AVATAR}', '{}', 'image/png', 5, 0, 'avatars/{}')",
                    ada.user_id,
                    "0".repeat(32)
                ),
            )
            .await;
            execute_on(&mut change, "COMMIT").await;
        },
        api.run_jobs()
    );
    assert_eq!(runs, 1);
    (ada, format!("/v1/public/avatars/{OTHER_AVATAR}"))
}

/// The ID of the avatar that another change gives Ada while the job fetches her picture.
const OTHER_AVATAR: &str = "018f0000-0000-7000-8000-000000000001";

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
    let ada = registered(&upstream, &api, files::PNG).await;
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
    assert_eq!(&picture.body[..], files::PNG);
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
    let ada = registered(&upstream, &api, files::PNG).await;
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
    // The second run asked the picture's host for nothing, and stored no object.
    assert_eq!(fetches(&upstream), 1);
    assert_eq!(objects(&api).await.len(), 1);
}

#[tokio::test]
async fn accounts_028_an_account_that_gets_an_avatar_while_its_picture_is_fetched_keeps_it() {
    let (upstream, api) = standing(Options::default()).await;

    let (ada, other) = overtaken(&upstream, &api).await;

    assert_eq!(avatar_url(&api, &ada).await, other);
    assert!(
        reads(
            &api,
            "SELECT id::text FROM audit_entries WHERE action = 'avatar.copied'"
        )
        .await
        .is_empty()
    );
    assert!(copies(&api).await.is_empty(), "no retry and no dead letter");
    // The job had fetched the picture, and stored the object that no row names (FILES-011).
    assert_eq!(fetches(&upstream), 1);
    assert_eq!(objects(&api).await.len(), 1);
}

#[tokio::test]
async fn files_013_a_copy_of_a_provider_s_picture_counts_as_an_avatar_s_upload() {
    let (upstream, api) = standing(Options::default()).await;
    let (stored, failed) = (uploads("stored"), uploads("failed"));

    registered(&upstream, &api, files::PNG).await;
    api.run_jobs().await;
    let copied = uploads("stored");
    let (overtaken_upstream, overtaken_api) = standing(Options::default()).await;
    overtaken(&overtaken_upstream, &overtaken_api).await;

    // Tests running alongside upload too, so the counts grow by at least these.
    assert!(copied >= stored + 1.0);
    assert!(uploads("failed") >= failed + 1.0);
}

#[tokio::test]
async fn accounts_028_an_account_that_set_an_avatar_before_the_job_ran_keeps_it() {
    let (upstream, api) = standing(Options::default()).await;
    let ada = registered(&upstream, &api, files::PNG).await;
    let set = api
        .request(http::Method::PUT, "/v1/users/me/avatar")
        .header("content-type", "application/octet-stream")
        .body(files::PNG.to_vec())
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
    assert_eq!(fetches(&upstream), 0, "the job read the avatar first");
}

#[tokio::test]
async fn accounts_028_a_picture_that_is_no_avatar_leaves_the_account_without_one() {
    // Bytes that are no image, more bytes than an avatar holds, and an answer that says the
    // address gives nothing.
    let large = [files::PNG, &vec![0; 1_048_576]].concat();
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
    let ada = registered(&upstream, &api, files::PNG).await;

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
    // A loopback address that isn't the tests' origin, a listed name that resolves to one, a
    // listed host over plain HTTP and at a port, and a host that the provider doesn't list
    // (SEC-10).
    for picture in [
        "https://127.0.0.1/pictures/ada.png",
        "https://localhost/pictures/ada.png",
        "http://pictures.provider.test/ada.png",
        "https://pictures.provider.test:8443/ada.png",
        "https://elsewhere.example.com/ada.png",
    ] {
        let (upstream, api) = standing(Options::default()).await;
        upstream.set_bytes("GET", PICTURE, 200, files::PNG);
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
    let ada = registered(&upstream, &api, files::PNG).await;

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
    let ada = registered(&upstream, &api, files::PNG).await;

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

#[tokio::test]
async fn auth_092_a_provider_that_names_no_host_of_its_pictures_hands_none_on() {
    // The stand-in's file names no host, and here its setting lists none.
    let upstream = Upstream::start().await;
    let api = Api::with(providers::stand_in(&upstream)).await;
    pictured(&upstream, &upstream.url(PICTURE));

    let response = sign_in(&api, &grant()).await;

    assert_eq!(response.status, 201, "{}", response.json());
    assert!(copies(&api).await.is_empty());
}

#[tokio::test]
async fn auth_097_the_copy_fetches_from_the_hosts_that_the_configured_providers_name() {
    // A host that the stand-in's setting lists, and one that it doesn't.
    let (upstream, api) = standing(Options::default()).await;
    pictured(&upstream, "https://localhost/pictures/ada.png");
    signed_in(&api).await;
    api.run_jobs().await;
    let (other, apart) = standing(Options::default()).await;
    pictured(&other, "https://elsewhere.example.com/pictures/ada.png");
    signed_in(&apart).await;
    apart.run_jobs().await;

    // The client took the listed host as far as its addresses, and never resolved the other.
    let causes: Vec<String> = testing::all_logged()
        .into_iter()
        .filter(|line| line["error.type"] == "picture_not_copied")
        .filter_map(|line| line["exception.message"].as_str().map(str::to_owned))
        .collect();
    let resolved = "GET of localhost: the name resolves to no public address";
    assert!(causes.iter().any(|cause| cause == resolved), "{causes:?}");
    assert!(
        !causes.iter().any(|cause| cause.contains("elsewhere")),
        "{causes:?}"
    );
    assert!(copies(&api).await.is_empty() && copies(&apart).await.is_empty());
}

#[tokio::test]
async fn auth_097_a_list_of_hosts_is_set_with_a_client_or_not_at_all() {
    let alone = Options::default().set(HOSTS_SETTING, "pictures.provider.test");
    assert_eq!(api::refusal(&alone), "STAND_IN_CLIENT_ID is required");

    let upstream = Upstream::start().await;
    api::configuration(&providers::stand_in(&upstream));
    api::configuration(&providers::stand_in(&upstream).set(HOSTS_SETTING, HOSTS));
}

#[tokio::test]
async fn auth_097_startup_refuses_a_list_that_holds_more_than_the_names_of_hosts() {
    let upstream = Upstream::start().await;
    for wrong in [
        "https://pictures.provider.test",
        "pictures.provider.test:443",
        "pictures.provider.test/ada.png",
        "Pictures.provider.test",
        "*.provider.test",
        "pictures.provider.test,",
        "pictures.provider.test,,localhost",
        "pictures.provider.test localhost",
        "203.0.113.7",
        "[2001:db8::1]",
    ] {
        let listed = providers::stand_in(&upstream).set(HOSTS_SETTING, wrong);
        assert_eq!(
            api::refusal(&listed),
            "STAND_IN_PICTURE_HOSTS must be the names of hosts, separated by commas, each in \
             lower case, with no scheme, port or path (AUTH-097)",
            "{wrong}"
        );
    }
}
