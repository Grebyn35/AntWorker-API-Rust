//! The picture that a provider gives for a new account, copied as the account's avatar
//! (ACCOUNTS-028 to ACCOUNTS-030, AUTH-092), from the hosts that the provider names (AUTH-097), through the tests'
//! stand-in provider, which also serves the picture.

use platform::telemetry::testing::{self, collector::Collector};
use serde_json::{Value, json};

use super::{objects, uploads_counted};
use crate::support::{
    Api,
    api::reads,
    api::{self, Options},
    counts, execute_on, files,
    providers::{self, gives, grant, sign_in, signed_in},
    query_waits_on_a_lock,
    scanner::{self, Script},
    upstream::{PICTURE, Upstream},
    users::{self, SignedIn},
    workspaces,
};

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

/// Registers Ada through the stand-in, which serves [`files::PNG`] as her picture.
async fn registered(upstream: &Upstream, api: &Api) -> SignedIn {
    upstream.set_bytes(200, files::PNG);
    pictured(upstream, &upstream.url(PICTURE));
    signed_in(api).await
}

/// The link of `user`'s avatar, or null.
async fn avatar_url(api: &Api, user: &SignedIn) -> Value {
    let record = api.get("/v1/users/me").bearer(&user.access).send().await;
    assert_eq!(record.status, 200, "{}", record.json());
    record.json()["avatar_url"].clone()
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
    let (ada, avatar, runs) = overtaken_while(upstream, api, api.run_jobs()).await;
    assert_eq!(runs, 1);
    (ada, avatar)
}

/// Registers Ada through the stand-in, as [`overtaken`] does, and gives her, the link of the
/// avatar that the other change gave her, and the output of `runs`, which runs the job that copies
/// her picture once the other change holds her row.
async fn overtaken_while<T>(
    upstream: &Upstream,
    api: &Api,
    runs: impl Future<Output = T>,
) -> (SignedIn, String, T) {
    let ada = registered(upstream, api).await;
    let db = api.database();
    let mut change = db
        .holding(&format!(
            "SELECT 1 FROM users WHERE id = '{}' FOR UPDATE",
            ada.user_id
        ))
        .await;

    let ((), ran) = tokio::join!(
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
        runs
    );
    (ada, format!("/v1/public/avatars/{OTHER_AVATAR}"), ran)
}

/// Runs a `serve` of `api`'s database, with the settings of the stand-in at `upstream`, which
/// exports to `collector`, until it has run every job that copies a picture, then stops it.
async fn copied_by_serve(upstream: &Upstream, api: &Api, collector: &Collector) {
    let options = providers::stand_in(upstream).set(HOSTS_SETTING, HOSTS);
    let env = options.env();
    let server = files::serving(api.database(), collector, &env).await;
    while !copies(api).await.is_empty() {
        tokio::task::yield_now().await;
    }
    server.stop().await;
}

/// The ID of the avatar that another change gives Ada while the job fetches her picture.
const OTHER_AVATAR: &str = "018f0000-0000-7000-8000-000000000001";

/// How many lines the test's jobs, which run on its thread, wrote of an error ignored as a
/// picture that wasn't copied (ERR-11): other tests log the same kind on threads of their own.
fn not_copied() -> usize {
    testing::logged_on_this_thread()
        .into_iter()
        .filter(|line| line["error.type"] == "picture_not_copied")
        .count()
}

#[tokio::test]
async fn accounts_028_a_registration_through_a_provider_gets_its_picture_as_its_avatar() {
    let (upstream, api) = standing(Options::default()).await;
    let ada = registered(&upstream, &api).await;
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
async fn accounts_030_a_second_run_of_the_job_changes_nothing() {
    let (upstream, api) = standing(Options::default()).await;
    let ada = registered(&upstream, &api).await;
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
async fn accounts_030_an_account_that_gets_an_avatar_while_its_picture_is_fetched_keeps_it() {
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
    let (overtaken_upstream, overtaken_api) = standing(Options::default()).await;
    let (copying, overtaking) = (
        Collector::start().await.unwrap(),
        Collector::start().await.unwrap(),
    );

    registered(&upstream, &api).await;
    copied_by_serve(&upstream, &api, &copying).await;
    overtaken_while(
        &overtaken_upstream,
        &overtaken_api,
        copied_by_serve(&overtaken_upstream, &overtaken_api, &overtaking),
    )
    .await;

    assert_eq!(uploads_counted(&copying), ["stored 1"]);
    assert_eq!(uploads_counted(&overtaking), ["failed 1"]);
}

#[tokio::test]
async fn accounts_030_an_account_that_set_an_avatar_before_the_job_ran_keeps_it() {
    let (upstream, api) = standing(Options::default()).await;
    let ada = registered(&upstream, &api).await;
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
async fn accounts_029_a_picture_that_is_no_avatar_leaves_the_account_without_one() {
    // Bytes that are no image, a byte more than an avatar holds, and an answer that says the
    // address gives nothing.
    let mut large = files::PNG.to_vec();
    large.resize(1_048_577, 0);
    for (status, bytes) in [(200, b"hello".as_slice()), (200, &large), (404, b"")] {
        let (upstream, api) = standing(Options::default()).await;
        upstream.set_bytes(status, bytes);
        pictured(&upstream, &upstream.url(PICTURE));
        let ada = signed_in(&api).await;
        let collector = Collector::start().await.unwrap();

        copied_by_serve(&upstream, &api, &collector).await;

        assert_eq!(avatar_url(&api, &ada).await, Value::Null, "{status}");
        assert!(copies(&api).await.is_empty(), "no retry and no dead letter");
        let ignored = counts::of(
            &collector.received(),
            "platform.errors_ignored",
            "error.type",
        );
        assert!(
            ignored.contains(&"picture_not_copied 1".to_owned()),
            "{status}: ERR-11's count: {ignored:?}"
        );
    }
}

#[tokio::test]
async fn accounts_029_a_picture_of_1_mib_becomes_the_avatar() {
    let (upstream, api) = standing(Options::default()).await;
    let mut largest = files::PNG.to_vec();
    largest.resize(1_048_576, 0);
    upstream.set_bytes(200, &largest);
    pictured(&upstream, &upstream.url(PICTURE));
    let ada = signed_in(&api).await;

    api.run_jobs().await;

    let url = avatar_url(&api, &ada).await;
    let picture = api.get(url.as_str().unwrap()).send().await;
    assert_eq!(picture.status, 200);
    assert_eq!(picture.body, largest);
    assert!(copies(&api).await.is_empty());
}

#[tokio::test]
async fn accounts_029_a_picture_in_which_the_scanner_finds_malware_leaves_no_avatar() {
    let stand_in = scanner::scripted(Script::Answer("stream: Eicar-Test-Signature FOUND")).await;
    let options = Options::default().set("CLAMD_ADDRESS", &stand_in.address);
    let (upstream, api) = standing(options).await;
    let ada = registered(&upstream, &api).await;

    api.run_jobs().await;

    assert_eq!(avatar_url(&api, &ada).await, Value::Null);
    assert!(copies(&api).await.is_empty());
    // The lines of this test's jobs, which run on its thread: other tests log the same kind.
    let logged = testing::logged_on_this_thread()
        .into_iter()
        .filter(|line| line["error.type"] == "picture_not_copied")
        .count();
    assert_eq!(logged, 1, "ERR-11's line");
}

#[tokio::test]
async fn accounts_029_an_address_that_the_untrusted_client_refuses_is_never_fetched() {
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
        upstream.set_bytes(200, files::PNG);
        pictured(&upstream, picture);
        let ada = signed_in(&api).await;
        let ignored = not_copied();

        api.run_jobs().await;

        assert_eq!(avatar_url(&api, &ada).await, Value::Null, "{picture}");
        assert!(copies(&api).await.is_empty(), "no retry and no dead letter");
        assert_eq!(not_copied(), ignored + 1, "ERR-11's line");
        assert!(
            upstream
                .received()
                .iter()
                .all(|request| request.path != PICTURE)
        );
    }
}

#[tokio::test]
async fn accounts_029_a_fetch_that_fails_is_tried_again_and_the_fifth_leaves_no_dead_letter() {
    let (upstream, api) = standing(Options::default()).await;
    upstream.set_bytes(503, b"");
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
    assert_eq!(not_copied(), ignored + 1, "ERR-11's line");
    assert_eq!(avatar_url(&api, &ada).await, Value::Null);
}

#[tokio::test]
async fn accounts_029_a_scanner_that_doesn_t_answer_is_tried_again_until_the_fifth_attempt() {
    let stand_in = scanner::scripted(Script::Close).await;
    let options = Options::default().set("CLAMD_ADDRESS", &stand_in.address);
    let (upstream, api) = standing(options).await;
    let ada = registered(&upstream, &api).await;

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
    assert_eq!(not_copied(), ignored + 1, "ERR-11's line");
    assert_eq!(avatar_url(&api, &ada).await, Value::Null);
}

#[tokio::test]
async fn accounts_029_a_storage_that_doesn_t_answer_is_tried_again_until_the_fifth_attempt() {
    // Nothing listens on port 9 of the loopback address.
    let options = Options::default().set("S3_ENDPOINT", "http://127.0.0.1:9");
    let (upstream, api) = standing(options).await;
    let ada = registered(&upstream, &api).await;

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
    assert_eq!(not_copied(), ignored + 1, "ERR-11's line");
    assert_eq!(avatar_url(&api, &ada).await, Value::Null);
}

// On each side of the limit: an address of 2,049 characters enqueues nothing, and one of 2,048 the
// copy of its picture.
#[tokio::test]
async fn accounts_028_an_address_of_more_than_2048_characters_enqueues_nothing() {
    for (length, enqueued) in [(2_049, false), (2_048, true)] {
        let (upstream, api) = standing(Options::default()).await;
        let folder = upstream.url("/pictures/");
        let address = format!("{folder}{}", "a".repeat(length - folder.chars().count()));
        pictured(&upstream, &address);

        let response = sign_in(&api, &grant()).await;

        assert_eq!(response.status, 201, "{}", response.json());
        let copied = if enqueued {
            vec![format!("waiting 0 {address}")]
        } else {
            Vec::new()
        };
        assert_eq!(copies(&api).await, copied, "{length} characters");
    }
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

    // The client took the listed host as far as its addresses, and never resolved the other, as
    // the lines of this test's jobs, which run on its thread, say: other tests log the same kind.
    let causes: Vec<String> = testing::logged_on_this_thread()
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
