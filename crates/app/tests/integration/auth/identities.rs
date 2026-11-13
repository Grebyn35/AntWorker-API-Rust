//! A user's identities, the ways their account signs in: reading them, adding a password,
//! linking a provider and removing one, with what each change publishes and tells the address
//! (AUTH-085 to AUTH-091, the auth spec's "Identities"), through the tests' stand-in provider.

use serde_json::{Value, json};

use crate::support::{
    Api,
    api::Response,
    execute_on, mailbox,
    providers::{self, gives, grant, identity, sign_in, signed_in},
    queries_wait_on_locks, second_factors, streams,
    upstream::Upstream,
    users::{self, PASSWORD, SignedIn},
};

/// The path of the caller's identities.
const IDENTITIES: &str = "/v1/users/me/identities";

/// A stand-in provider, and an API that signs in through it.
async fn standing() -> (Upstream, Api) {
    let upstream = Upstream::start().await;
    let api = Api::with(providers::stand_in(&upstream)).await;
    (upstream, api)
}

/// The first column of each row of `sql`, as the `owner` reads it.
async fn reads(api: &Api, sql: &str) -> Vec<String> {
    api.database().owner_reads(sql).await
}

/// Reads `user`'s identities.
async fn read(api: &Api, user: &SignedIn) -> Response {
    api.get(IDENTITIES).bearer(&user.access).send().await
}

/// Adds `password` to `user`'s account.
async fn add_password(api: &Api, user: &SignedIn, password: &str) -> Response {
    api.post(
        &format!("{IDENTITIES}/password"),
        &json!({ "password": password }),
    )
    .bearer(&user.access)
    .send()
    .await
}

/// Links the account that the stand-in gives for `body`'s code to `user`'s account.
async fn link(api: &Api, user: &SignedIn, body: &Value) -> Response {
    api.post(&format!("{IDENTITIES}/provider"), body)
        .bearer(&user.access)
        .send()
        .await
}

/// Removes `user`'s identity `id`.
async fn remove(api: &Api, user: &SignedIn, id: &str) -> Response {
    api.request(http::Method::DELETE, &format!("{IDENTITIES}/{id}"))
        .bearer(&user.access)
        .send()
        .await
}

/// The ID of `user`'s password identity.
async fn password_id(api: &Api, user: &SignedIn) -> String {
    reads(
        api,
        &format!(
            "SELECT id::text FROM identities WHERE user_id = '{}'",
            user.user_id
        ),
    )
    .await
    .remove(0)
}

/// The ID of `user`'s identity at the stand-in.
async fn provider_id(api: &Api, user: &SignedIn) -> String {
    reads(
        api,
        &format!(
            "SELECT id::text FROM provider_identities \
             WHERE user_id = '{}' AND provider = 'stand_in'",
            user.user_id
        ),
    )
    .await
    .remove(0)
}

/// Gives `user` an identity at `retired`, a provider that the deployment configures no more,
/// linked a day ago, a row that no route of this API writes (TEST-09), and returns its ID.
async fn retired_identity(api: &Api, user: &SignedIn) -> String {
    reads(
        api,
        &format!(
            "INSERT INTO provider_identities (user_id, provider, subject, created_at) \
             VALUES ('{}', 'retired', 'subject-0', now() - interval '1 day') RETURNING id::text",
            user.user_id
        ),
    )
    .await
    .remove(0)
}

/// Each entry of `action` in `user`'s own log, as its actor, its resource's type and ID, and its
/// details.
async fn entries(api: &Api, user: &SignedIn, action: &str) -> Vec<String> {
    reads(
        api,
        &format!(
            "SELECT actor_user_id || ' ' || resource_type || ' ' || resource_id || ' ' \
             || details::text FROM audit_entries \
             WHERE user_id = '{}' AND action = '{action}' ORDER BY created_at, id",
            user.user_id
        ),
    )
    .await
}

/// Whether `user`'s session still signs them in.
async fn works(api: &Api, user: &SignedIn) -> bool {
    let response = api.get("/v1/users/me").bearer(&user.access).send().await;
    assert!(
        matches!(response.status.as_u16(), 200 | 401),
        "{}",
        response.status
    );
    response.status == 200
}

/// The bucket of the changes of `user`'s identities, which a first change adds once a first
/// authenticated request has added the bucket of their requests (AUTH-090, PLATFORM-054).
async fn change_bucket(api: &Api, user: &SignedIn) -> String {
    assert_eq!(read(api, user).await.status, 200);
    users::added_bucket(
        api,
        remove(api, user, "0195f0a4-0000-7000-8000-000000000000"),
    )
    .await
}

/// The names of `value`'s members, sorted.
fn members(value: &Value) -> Vec<&str> {
    let mut names: Vec<&str> = value
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    names.sort_unstable();
    names
}

#[tokio::test]
async fn auth_085_the_identities_are_the_password_and_each_provider_identity_the_earliest_first() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    assert_eq!(link(&api, &ada, &grant()).await.status, 200);
    let retired = retired_identity(&api, &ada).await;
    // Another user's identities, which hers leave out.
    gives(&upstream, identity("subject-2", "bob@example.com", true));
    signed_in(&api).await;

    let response = read(&api, &ada).await;

    assert_eq!(response.status, 200, "{}", response.json());
    let body = response.json();
    assert_eq!(members(&body), ["password", "providers"]);
    assert_eq!(members(&body["password"]), ["created_at", "id"]);
    assert_eq!(body["password"]["id"], password_id(&api, &ada).await);
    let providers = body["providers"].as_array().unwrap();
    assert_eq!(providers.len(), 2);
    for provider in providers {
        assert_eq!(members(provider), ["created_at", "id", "provider"]);
    }
    assert_eq!(providers[0]["provider"], "retired");
    assert_eq!(providers[0]["id"], retired);
    assert_eq!(providers[1]["provider"], "stand_in");
    assert_eq!(providers[1]["id"], provider_id(&api, &ada).await);
    assert!(!response.json().to_string().contains("subject-"));
}

#[tokio::test]
async fn auth_085_an_account_without_a_password_reads_null_for_it() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;

    let response = read(&api, &ada).await;

    assert_eq!(response.status, 200);
    assert_eq!(response.json()["password"], Value::Null);
    assert_eq!(response.json()["providers"][0]["provider"], "stand_in");
}

#[tokio::test]
async fn auth_086_adding_a_password_needs_a_recent_proof_then_the_password_signs_in() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    users::authenticated_ago(&api, &ada, 10).await;

    let refused = add_password(&api, &ada, PASSWORD).await;
    let without = reads(&api, "SELECT count(*)::text FROM identities").await;
    users::authenticated_ago(&api, &ada, 0).await;
    let added = add_password(&api, &ada, PASSWORD).await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "auth.reauthentication_required");
    assert_eq!(without, ["0"]);
    assert_eq!(added.status, 200, "{}", added.json());
    let body = added.json();
    let id = body["password"]["id"].as_str().unwrap();
    assert_eq!(body["providers"][0]["provider"], "stand_in");
    assert_eq!(
        reads(
            &api,
            "SELECT id::text || ' ' || left(password_hash, 31) FROM identities"
        )
        .await,
        [format!("{id} $argon2id$v=19$m=19456,t=2,p=1$")],
        "AUTH-014"
    );
    assert_eq!(
        entries(&api, &ada, "password.added").await,
        [format!("{} identity {id} {{}}", ada.user_id)]
    );
    assert_eq!(
        users::sign_in(&api, "ada@example.com", PASSWORD)
            .await
            .status,
        201
    );
    assert!(works(&api, &ada).await, "the account's sessions stay");
}

#[tokio::test]
async fn auth_086_an_account_that_has_a_password_answers_409_and_keeps_it() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let other = "lantern-orchard-58-brisk";

    let response = add_password(&api, &ada, other).await;

    assert_eq!(response.status, 409);
    assert_eq!(response.json()["code"], "auth.password_already_set");
    assert_eq!(
        users::sign_in(&api, "ada@example.com", PASSWORD)
            .await
            .status,
        201
    );
    assert_eq!(
        users::sign_in(&api, "ada@example.com", other).await.status,
        401
    );
    assert!(entries(&api, &ada, "password.added").await.is_empty());
}

#[tokio::test]
async fn auth_086_a_password_that_the_policy_refuses_answers_400() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;

    let response = api
        .post_raw(
            &format!("{IDENTITIES}/password"),
            &json!({ "password": "short" }),
        )
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 400);
    assert_eq!(response.json()["errors"][0]["field"], "/password");
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM identities").await,
        ["0"]
    );
}

#[tokio::test]
async fn auth_086_adding_a_password_whose_session_ended_once_it_was_authenticated_answers_401() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    let bucket = change_bucket(&api, &ada).await;

    let (status, body) = users::after_its_session_ended(
        &api,
        &ada,
        &bucket,
        (http::Method::POST, &format!("{IDENTITIES}/password")),
        Some(json!({ "password": PASSWORD })),
    )
    .await;

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM identities").await,
        ["0"]
    );
}

#[tokio::test]
async fn auth_087_linking_needs_a_recent_proof_then_the_provider_signs_in_to_the_account() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    // The provider gives another address than the account's.
    gives(
        &upstream,
        identity("subject-1", "ada@elsewhere.example", true),
    );
    users::authenticated_ago(&api, &ada, 10).await;

    let refused = link(&api, &ada, &grant()).await;
    let without = reads(&api, "SELECT count(*)::text FROM provider_identities").await;
    users::authenticated_ago(&api, &ada, 0).await;
    let linked = link(&api, &ada, &grant()).await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "auth.reauthentication_required");
    assert_eq!(without, ["0"]);
    assert_eq!(linked.status, 200, "{}", linked.json());
    let body = linked.json();
    assert_eq!(body["password"]["id"], password_id(&api, &ada).await);
    let id = body["providers"][0]["id"].as_str().unwrap();
    assert_eq!(body["providers"][0]["provider"], "stand_in");
    assert_eq!(
        reads(
            &api,
            "SELECT id::text || ' ' || user_id || ' ' || provider || ' ' || subject \
             FROM provider_identities"
        )
        .await,
        [format!("{id} {} stand_in subject-1", ada.user_id)]
    );
    assert_eq!(
        entries(&api, &ada, "provider.linked").await,
        [format!(
            "{} identity {id} {{\"provider\": \"stand_in\"}}",
            ada.user_id
        )]
    );
    let session = sign_in(&api, &grant()).await;
    assert_eq!(session.status, 201, "{}", session.json());
    assert_eq!(session.json()["user_id"], ada.user_id.as_str());
    assert_eq!(
        reads(&api, "SELECT email FROM users").await,
        ["ada@example.com"]
    );
    assert!(works(&api, &ada).await, "the account's sessions stay");
}

#[tokio::test]
async fn auth_087_a_caller_with_an_identity_of_the_provider_answers_409_whatever_its_subject() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    assert_eq!(link(&api, &ada, &grant()).await.status, 200);

    let same = link(&api, &ada, &grant()).await;
    gives(&upstream, identity("subject-2", "ada@example.com", true));
    let another = link(&api, &ada, &grant()).await;

    for response in [&same, &another] {
        assert_eq!(response.status, 409);
        assert_eq!(response.json()["code"], "auth.provider_already_linked");
    }
    assert_eq!(
        reads(&api, "SELECT subject FROM provider_identities").await,
        ["subject-1"]
    );
    assert_eq!(entries(&api, &ada, "provider.linked").await.len(), 1);
}

#[tokio::test]
async fn auth_087_a_subject_that_another_account_holds_answers_409() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "bob@example.com", true));
    let bob = signed_in(&api).await;
    let ada = users::register(&api, "ada@example.com").await;

    let response = link(&api, &ada, &grant()).await;

    assert_eq!(response.status, 409);
    assert_eq!(response.json()["code"], "auth.identity_in_use");
    assert_eq!(
        reads(&api, "SELECT user_id::text FROM provider_identities").await,
        [bob.user_id.as_str()]
    );
    assert!(entries(&api, &ada, "provider.linked").await.is_empty());
}

#[tokio::test]
async fn auth_087_the_address_that_the_provider_gives_decides_nothing() {
    let (upstream, api) = standing().await;
    users::register(&api, "carol@example.com").await;
    let ada = users::register(&api, "ada@example.com").await;
    let dan = users::register(&api, "dan@example.com").await;

    // An address that another account holds.
    gives(&upstream, identity("subject-1", "carol@example.com", true));
    let held = link(&api, &ada, &grant()).await;
    // No address.
    upstream.set("GET", "/identity", 200, json!({ "sub": "subject-2" }));
    let none = link(&api, &dan, &grant()).await;

    assert_eq!(held.status, 200, "{}", held.json());
    assert_eq!(none.status, 200, "{}", none.json());
    assert_eq!(
        reads(&api, "SELECT email FROM users ORDER BY 1").await,
        ["ada@example.com", "carol@example.com", "dan@example.com"]
    );
}

#[tokio::test]
async fn auth_087_a_code_that_the_provider_refuses_answers_403_and_the_session_stays() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    upstream.set("POST", "/token", 400, json!({ "error": "invalid_grant" }));

    let response = link(&api, &ada, &grant()).await;

    assert_eq!(response.status, 403);
    assert_eq!(response.json()["code"], "auth.provider_proof_rejected");
    assert!(works(&api, &ada).await);
}

#[tokio::test]
async fn auth_087_a_malformed_link_answers_400_naming_every_field_before_the_exchange() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    let malformed = json!({
        "provider": "stand_in",
        "code": "a code with spaces",
        "code_verifier": "too-short",
        "redirect_uri": "https://evil.example/callback",
    });

    let response = api
        .post_raw(&format!("{IDENTITIES}/provider"), &malformed)
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 400);
    let body = response.json();
    let fields: Vec<&str> = body["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|error| error["field"].as_str().unwrap())
        .collect();
    assert_eq!(
        fields,
        ["/code", "/code_verifier", "/redirect_uri"],
        "API-10"
    );
    assert!(upstream.received().is_empty());
}

#[tokio::test]
async fn auth_077_a_link_answers_a_provider_it_can_t_exchange_at_as_a_sign_in_does() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    let mut nobody = grant();
    nobody["provider"] = json!("nobody");
    upstream.set("POST", "/token", 500, json!({}));

    let unknown = link(&api, &ada, &nobody).await;
    let failing = link(&api, &ada, &grant()).await;
    let plain = Api::new().await;
    let grace = users::register(&plain, "grace@example.com").await;
    let unconfigured = link(&plain, &grace, &grant()).await;

    assert_eq!(unknown.status, 404);
    assert_eq!(unknown.json()["code"], "auth.provider_not_found");
    assert_eq!(failing.status, 503);
    assert_eq!(failing.json()["code"], "auth.provider_unavailable");
    assert_eq!(failing.header("retry-after"), "30");
    assert_eq!(unconfigured.status, 503);
    assert_eq!(unconfigured.json()["code"], "request.feature_disabled");
}

#[tokio::test]
async fn auth_087_a_link_whose_session_ended_once_it_was_authenticated_answers_401() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let bucket = change_bucket(&api, &ada).await;

    let (status, body) = users::after_its_session_ended(
        &api,
        &ada,
        &bucket,
        (http::Method::POST, &format!("{IDENTITIES}/provider")),
        Some(grant()),
    )
    .await;

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM provider_identities").await,
        ["0"]
    );
}

#[tokio::test]
async fn auth_074_a_registration_that_meets_a_link_of_its_subject_answers_409() {
    let (upstream, api) = standing().await;
    let carol = users::register(&api, "carol@example.com").await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let db = api.database();
    // A link of the subject to Carol's account, which hasn't committed, so the sign-in reads
    // that no account holds the subject.
    let mut linking = db
        .holding(&format!(
            "INSERT INTO provider_identities (user_id, provider, subject) \
             VALUES ('{}', 'stand_in', 'subject-1')",
            carol.user_id
        ))
        .await;

    let registering = {
        let api = api.clone();
        tokio::spawn(async move { sign_in(&api, &grant()).await })
    };
    queries_wait_on_locks(db, 1).await;
    execute_on(&mut linking, "COMMIT").await;
    let response = registering.await.unwrap();

    assert_eq!(response.status, 409, "{}", response.json());
    assert_eq!(response.json()["code"], "auth.identity_in_use");
    assert_eq!(
        reads(&api, "SELECT email FROM users").await,
        ["carol@example.com"]
    );
}

#[tokio::test]
async fn auth_073_a_sign_in_that_waited_for_the_removal_of_its_identity_reaches_no_account() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    let db = api.database();
    // A removal of her provider identity, which holds her turn.
    let mut removal = db
        .holding(&format!(
            "SELECT 1 FROM users WHERE id = '{}' FOR NO KEY UPDATE",
            ada.user_id
        ))
        .await;

    let signing_in = {
        let api = api.clone();
        tokio::spawn(async move { sign_in(&api, &grant()).await })
    };
    queries_wait_on_locks(db, 1).await;
    execute_on(
        &mut removal,
        &format!(
            "DELETE FROM provider_identities WHERE user_id = '{}'",
            ada.user_id
        ),
    )
    .await;
    execute_on(&mut removal, "COMMIT").await;
    let response = signing_in.await.unwrap();

    // The subject is no account's, and the address it gives is her account's (AUTH-076).
    assert_eq!(response.status, 409, "{}", response.json());
    assert_eq!(response.json()["code"], "auth.email_in_use");
    assert_eq!(
        reads(&api, "SELECT id::text FROM sessions").await,
        [ada.session_id.as_str()]
    );
}

#[tokio::test]
async fn auth_074_a_registration_after_a_removal_that_its_sign_in_waited_for_counts_in_its_bucket()
{
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    // Her account at the provider gives another address, which the provider doesn't prove.
    gives(&upstream, identity("subject-1", "other@example.com", false));
    assert_eq!(link(&api, &ada, &grant()).await.status, 200);
    let db = api.database();
    // A removal of her provider identity, which holds her turn.
    let mut removal = db
        .holding(&format!(
            "SELECT 1 FROM users WHERE id = '{}' FOR NO KEY UPDATE",
            ada.user_id
        ))
        .await;

    let signing_in = {
        let api = api.clone();
        tokio::spawn(async move { sign_in(&api, &grant()).await })
    };
    queries_wait_on_locks(db, 1).await;
    execute_on(
        &mut removal,
        &format!(
            "DELETE FROM provider_identities WHERE user_id = '{}'",
            ada.user_id
        ),
    )
    .await;
    execute_on(&mut removal, "COMMIT").await;
    let registered = signing_in.await.unwrap();

    // The subject is no account's by then, and no account holds its address: the sign-in
    // registers, and the code it sends counts in the address's bucket of 3 (AUTH-029), though
    // the sign-in read a linked account before it waited.
    assert_eq!(registered.status, 201, "{}", registered.json());
    assert_ne!(registered.json()["user_id"], ada.user_id.as_str());
    gives(&upstream, identity("subject-2", "other@example.com", false));
    for _ in 0..2 {
        assert_eq!(sign_in(&api, &grant()).await.status, 409);
    }
    let limited = sign_in(&api, &grant()).await;
    assert_eq!(limited.status, 429);
    assert_eq!(limited.json()["code"], "request.rate_limited");
}

#[tokio::test]
async fn auth_074_beyond_its_limit_a_registration_after_such_a_wait_answers_429_and_writes_nothing()
{
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "other@example.com", false));
    assert_eq!(link(&api, &ada, &grant()).await.status, 200);
    // The address's bucket of 3 is full: a registration of another subject's, which holds the
    // address since, and two more that met it (AUTH-029).
    gives(&upstream, identity("subject-2", "other@example.com", false));
    assert_eq!(sign_in(&api, &grant()).await.status, 201);
    gives(&upstream, identity("subject-3", "other@example.com", false));
    for _ in 0..2 {
        assert_eq!(sign_in(&api, &grant()).await.status, 409);
    }
    gives(&upstream, identity("subject-1", "other@example.com", false));
    let db = api.database();
    let before = reads(&api, "SELECT count(*)::text FROM users").await;
    // A removal of her provider identity, which holds her turn.
    let mut removal = db
        .holding(&format!(
            "SELECT 1 FROM users WHERE id = '{}' FOR NO KEY UPDATE",
            ada.user_id
        ))
        .await;

    let signing_in = {
        let api = api.clone();
        tokio::spawn(async move { sign_in(&api, &grant()).await })
    };
    queries_wait_on_locks(db, 1).await;
    execute_on(
        &mut removal,
        &format!(
            "DELETE FROM provider_identities WHERE user_id = '{}'",
            ada.user_id
        ),
    )
    .await;
    execute_on(&mut removal, "COMMIT").await;
    let limited = signing_in.await.unwrap();

    // The sign-in has become a registration, which its address's limit refuses before its unit
    // of work reads that an account holds the address, which would answer 409.
    assert_eq!(limited.status, 429, "{}", limited.json());
    assert_eq!(limited.json()["code"], "request.rate_limited");
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM users").await,
        before
    );
}

#[tokio::test]
async fn auth_074_a_registration_after_a_deletion_that_its_sign_in_waited_for_counts_in_its_bucket()
{
    let (upstream, api) = standing().await;
    // Her provider doesn't prove her address, so her registration counted once in the address's
    // bucket of 3 (AUTH-029).
    gives(&upstream, identity("subject-1", "ada@example.com", false));
    let ada = signed_in(&api).await;
    let db = api.database();
    // A deletion of her account, which holds her turn.
    let mut deletion = db
        .holding(&format!("DELETE FROM users WHERE id = '{}'", ada.user_id))
        .await;

    let signing_in = {
        let api = api.clone();
        tokio::spawn(async move { sign_in(&api, &grant()).await })
    };
    queries_wait_on_locks(db, 1).await;
    execute_on(&mut deletion, "COMMIT").await;
    let registered = signing_in.await.unwrap();

    // Her account is gone by then, and its address with it: the sign-in registers, and counts a
    // second time in the address's bucket, though it read a linked account before it waited.
    assert_eq!(registered.status, 201, "{}", registered.json());
    assert_ne!(registered.json()["user_id"], ada.user_id.as_str());
    gives(&upstream, identity("subject-2", "ada@example.com", false));
    assert_eq!(sign_in(&api, &grant()).await.status, 409);
    let limited = sign_in(&api, &grant()).await;
    assert_eq!(limited.status, 429);
    assert_eq!(limited.json()["code"], "request.rate_limited");
}

#[tokio::test]
async fn auth_088_removing_the_password_needs_a_recent_proof_then_ends_its_other_sessions() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    assert_eq!(link(&api, &ada, &grant()).await.status, 200);
    let phone = users::new_session(&api, "ada@example.com").await;
    let tablet = signed_in(&api).await;
    let mut stream = streams::opened(&api, &phone).await;
    let password = password_id(&api, &ada).await;
    users::authenticated_ago(&api, &ada, 10).await;

    let refused = remove(&api, &ada, &password).await;
    let still = works(&api, &phone).await;
    users::authenticated_ago(&api, &ada, 0).await;
    let mut signals = api.database().signals().await;
    let removed = remove(&api, &ada, &password).await;

    assert_eq!(refused.status, 403);
    assert_eq!(refused.json()["code"], "auth.reauthentication_required");
    assert!(still);
    assert_eq!(removed.status, 204);
    assert!(removed.body.is_empty());
    assert_eq!(read(&api, &ada).await.json()["password"], Value::Null);
    assert!(!works(&api, &phone).await, "it signed in with the password");
    assert!(
        works(&api, &tablet).await,
        "it signed in through the provider"
    );
    assert!(works(&api, &ada).await, "the request's own session stays");
    assert_eq!(
        users::sign_in(&api, "ada@example.com", PASSWORD)
            .await
            .status,
        401
    );
    assert_eq!(
        entries(&api, &ada, "password.removed").await,
        [format!("{} identity {password} {{}}", ada.user_id)]
    );
    assert_eq!(
        entries(&api, &ada, "session.ended").await,
        [format!("{} session {} {{}}", ada.user_id, phone.session_id)]
    );
    let received: Vec<Value> = signals.received().await;
    assert_eq!(received.len(), 1);
    assert_eq!(received[0]["kind"], "session_ended");
    assert_eq!(received[0]["session_id"], phone.session_id.as_str());
    let (_, reason) = stream.closed().await;
    assert_eq!(reason, "session_ended");
}

#[tokio::test]
async fn auth_088_removing_a_provider_identity_ends_the_other_sessions_that_signed_in_through_it() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    assert_eq!(add_password(&api, &ada, PASSWORD).await.status, 200);
    let tablet = signed_in(&api).await;
    let phone = users::new_session(&api, "ada@example.com").await;
    let id = provider_id(&api, &ada).await;

    let removed = remove(&api, &ada, &id).await;

    assert_eq!(removed.status, 204);
    assert!(
        works(&api, &ada).await,
        "the request's own session stays, whatever way it signed in"
    );
    assert!(
        !works(&api, &tablet).await,
        "it signed in through the provider"
    );
    assert!(works(&api, &phone).await, "it signed in with the password");
    assert_eq!(read(&api, &ada).await.json()["providers"], json!([]));
    assert_eq!(
        entries(&api, &ada, "provider.unlinked").await,
        [format!(
            "{} identity {id} {{\"provider\": \"stand_in\"}}",
            ada.user_id
        )]
    );
    assert_eq!(
        entries(&api, &ada, "session.ended").await,
        [format!(
            "{} session {} {{}}",
            ada.user_id, tablet.session_id
        )]
    );
    // The provider's account signs in to hers no more: it meets her address, as a stranger's
    // would (AUTH-076).
    let again = sign_in(&api, &grant()).await;
    assert_eq!(again.status, 409);
    assert_eq!(again.json()["code"], "auth.email_in_use");
}

#[tokio::test]
async fn auth_046_a_removal_of_an_identity_publishes_session_ended_for_each_session_it_ends() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    assert_eq!(add_password(&api, &ada, PASSWORD).await.status, 200);
    let tablet = signed_in(&api).await;
    let phone = users::new_session(&api, "ada@example.com").await;
    let provider = provider_id(&api, &ada).await;
    let before = users::session_events(&api, "session.ended").await;

    let unlinked = remove(&api, &ada, &provider).await;

    assert_eq!(unlinked.status, 204);
    assert!(before.is_empty());
    // The session that signed in through the provider, and neither the request's own nor the
    // one that signed in with the password.
    assert_eq!(
        users::session_events(&api, "session.ended").await,
        [format!("{} {}", ada.user_id, tablet.session_id)]
    );
    assert!(works(&api, &phone).await);
}

#[tokio::test]
async fn auth_088_an_identity_of_a_provider_that_the_deployment_dropped_is_removed() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let phone = users::new_session(&api, "ada@example.com").await;
    let retired = retired_identity(&api, &ada).await;
    // A session that signed in through the provider while the deployment configured it.
    api.database()
        .owner_executes(&format!(
            "UPDATE sessions SET provider = 'retired' WHERE id = '{}'",
            phone.session_id
        ))
        .await;

    let removed = remove(&api, &ada, &retired).await;

    assert_eq!(removed.status, 204);
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM provider_identities").await,
        ["0"]
    );
    assert!(!works(&api, &phone).await);
    assert_eq!(
        entries(&api, &ada, "provider.unlinked").await,
        [format!(
            "{} identity {retired} {{\"provider\": \"retired\"}}",
            ada.user_id
        )]
    );
}

#[tokio::test]
async fn auth_088_the_account_s_last_identity_stays() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "bob@example.com", true));
    let bob = signed_in(&api).await;

    let password = remove(&api, &ada, &password_id(&api, &ada).await).await;
    let provider = remove(&api, &bob, &provider_id(&api, &bob).await).await;

    for response in [&password, &provider] {
        assert_eq!(response.status, 422);
        assert_eq!(response.json()["code"], "auth.last_identity");
    }
    assert_eq!(
        users::sign_in(&api, "ada@example.com", PASSWORD)
            .await
            .status,
        201
    );
    assert_eq!(sign_in(&api, &grant()).await.status, 201);
    assert!(entries(&api, &ada, "password.removed").await.is_empty());
    assert!(entries(&api, &bob, "provider.unlinked").await.is_empty());
}

#[tokio::test]
async fn auth_088_an_id_that_names_none_of_the_caller_s_identities_answers_404_once_proved() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    let hers = password_id(&api, &grace).await;
    let unknown = "0195f0a4-0000-7000-8000-000000000000";

    let mut answers = Vec::new();
    for id in [hers.as_str(), unknown, "not-a-uuid"] {
        answers.push(remove(&api, &ada, id).await);
    }
    users::authenticated_ago(&api, &ada, 10).await;
    let unproved = remove(&api, &ada, "not-a-uuid").await;

    for answer in &answers {
        assert_eq!(answer.status, 404);
        assert_eq!(answer.json()["code"], "auth.identity_not_found");
    }
    // An ID that isn't a UUID answers as one that names no identity does, after the proof.
    assert_eq!(unproved.status, 403);
    assert_eq!(unproved.json()["code"], "auth.reauthentication_required");
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM identities").await,
        ["2"]
    );
}

#[tokio::test]
async fn auth_088_two_removals_at_once_take_turns_and_leave_the_account_an_identity() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    assert_eq!(link(&api, &ada, &grant()).await.status, 200);
    let tablet = signed_in(&api).await;
    let password = password_id(&api, &ada).await;
    let provider = provider_id(&api, &ada).await;
    let db = api.database();
    // A lock on her row holds both removals at their turn.
    let mut holder = db
        .holding(&format!(
            "SELECT 1 FROM users WHERE id = '{}' FOR UPDATE",
            ada.user_id
        ))
        .await;

    let first = api.spawn(
        http::Method::DELETE,
        &format!("{IDENTITIES}/{password}"),
        &ada.access,
        None,
    );
    let second = api.spawn(
        http::Method::DELETE,
        &format!("{IDENTITIES}/{provider}"),
        &tablet.access,
        None,
    );
    queries_wait_on_locks(db, 2).await;
    execute_on(&mut holder, "COMMIT").await;
    let (first, second) = (first.await.unwrap(), second.await.unwrap());

    let (removed, refused) = if first.0 == 204 {
        (first, second)
    } else {
        (second, first)
    };
    assert_eq!(removed.0, 204);
    assert_eq!(refused.0, 422);
    assert_eq!(refused.1["code"], "auth.last_identity");
    assert_eq!(
        reads(
            &api,
            "SELECT ((SELECT count(*) FROM identities) \
             + (SELECT count(*) FROM provider_identities))::text"
        )
        .await,
        ["1"]
    );
}

#[tokio::test]
async fn auth_088_a_removal_whose_session_ended_once_it_was_authenticated_answers_401() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let bucket = change_bucket(&api, &ada).await;
    assert_eq!(link(&api, &ada, &grant()).await.status, 200);
    let password = password_id(&api, &ada).await;

    let (status, body) = users::after_its_session_ended(
        &api,
        &ada,
        &bucket,
        (http::Method::DELETE, &format!("{IDENTITIES}/{password}")),
        None,
    )
    .await;

    assert_eq!(status, 401);
    assert_eq!(body["code"], "request.unauthenticated");
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM identities").await,
        ["1"]
    );
}

#[tokio::test]
async fn auth_069_a_removal_of_an_identity_deletes_the_challenges_that_its_sign_ins_issued() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    assert_eq!(link(&api, &ada, &grant()).await.status, 200);
    let app = second_factors::add_app(&api, &ada).await;
    let with_password = second_factors::challenge(&api, "ada@example.com").await;
    let through_provider = sign_in(&api, &grant()).await;
    assert_eq!(through_provider.status, 200, "{}", through_provider.json());
    let through_provider = through_provider.json()["challenge"]
        .as_str()
        .unwrap()
        .to_owned();

    let removed = remove(&api, &ada, &password_id(&api, &ada).await).await;

    assert_eq!(removed.status, 204);
    let code = second_factors::code(&api, &app.secret).await;
    let gone = second_factors::complete(&api, &with_password, json!({ "code": code })).await;
    assert_eq!(gone.status, 401);
    assert_eq!(gone.json()["code"], "auth.invalid_challenge");
    let kept = second_factors::complete(&api, &through_provider, json!({ "code": code })).await;
    assert_eq!(kept.status, 201, "{}", kept.json());
}

#[tokio::test]
async fn data_20_a_removal_of_the_password_and_a_sign_in_with_it_never_deadlock() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    assert_eq!(link(&api, &ada, &grant()).await.status, 200);
    let password = password_id(&api, &ada).await;
    let db = api.database();
    // A sign-in with the password: it locks the password identity, then starts its session.
    let mut signing_in = db
        .holding(&format!(
            "SELECT 1 FROM identities WHERE user_id = '{}' FOR UPDATE",
            ada.user_id
        ))
        .await;

    let removal = api.spawn(
        http::Method::DELETE,
        &format!("{IDENTITIES}/{password}"),
        &ada.access,
        None,
    );
    queries_wait_on_locks(db, 1).await;
    // The removal holds her row and waits for the identity. The session's foreign key reads her
    // row for a key share, which the removal's lock lets through, so the insert waits for
    // nothing.
    execute_on(
        &mut signing_in,
        &format!(
            "INSERT INTO sessions (user_id, access_token_hash, refresh_token_hash, \
             access_token_expires_at, refresh_token_expires_at, expires_at) \
             VALUES ('{}', '\\x01', '\\x02', now() + interval '1 hour', \
             now() + interval '30 days', now() + interval '90 days')",
            ada.user_id
        ),
    )
    .await;
    execute_on(&mut signing_in, "COMMIT").await;
    let (status, _) = removal.await.unwrap();

    assert_eq!(status, 204);
    // The removal ended the session that the sign-in started before it.
    assert_eq!(
        reads(&api, "SELECT id::text FROM sessions").await,
        [ada.session_id.as_str()]
    );
}

#[tokio::test]
async fn auth_089_a_link_and_a_removal_reach_the_user_s_streams() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let mut stream = streams::opened(&api, &ada).await;

    let linked = link(&api, &ada, &grant()).await;
    let added = stream.event().await;
    let id = linked.json()["providers"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let removed = remove(&api, &ada, &id).await;
    let left = stream.event().await;

    assert_eq!(removed.status, 204);
    assert_eq!(added.event.as_deref(), Some("identity.added"));
    assert_eq!(added.payload(), &json!({ "identity_id": id }));
    assert_eq!(left.event.as_deref(), Some("identity.removed"));
    assert_eq!(left.payload(), &json!({ "identity_id": id }));
}

#[tokio::test]
async fn auth_089_each_change_s_event_names_its_user_and_the_identity() {
    let (upstream, api) = standing().await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let ada = signed_in(&api).await;
    // Another user, whom no event of hers names.
    users::register(&api, "grace@example.com").await;
    let provider = provider_id(&api, &ada).await;

    let added = add_password(&api, &ada, PASSWORD).await;
    let removed = remove(&api, &ada, &provider).await;

    assert_eq!(added.status, 200);
    assert_eq!(removed.status, 204);
    let password = password_id(&api, &ada).await;
    assert_eq!(
        reads(
            &api,
            "SELECT name || ' ' || user_id || ' ' || (payload->>'identity_id') FROM events \
             WHERE name LIKE 'identity.%' ORDER BY 1"
        )
        .await,
        [
            format!("identity.added {} {password}", ada.user_id),
            format!("identity.removed {} {provider}", ada.user_id),
        ]
    );
}

#[tokio::test]
async fn auth_090_the_changes_of_one_s_identities_count_in_one_bucket_of_the_user_s() {
    let (upstream, api) = standing().await;
    let ada = users::register(&api, "ada@example.com").await;
    let grace = users::register(&api, "grace@example.com").await;
    gives(&upstream, identity("subject-1", "ada@example.com", true));
    let other = "lantern-orchard-58-brisk";
    for _ in 0..10 {
        assert_eq!(add_password(&api, &ada, other).await.status, 409);
    }
    let password = password_id(&api, &ada).await;

    let limited = [
        link(&api, &ada, &grant()).await,
        remove(&api, &ada, &password).await,
        add_password(&api, &ada, other).await,
    ];

    for response in &limited {
        assert_eq!(response.status, 429);
        assert_eq!(response.json()["code"], "request.rate_limited");
        assert!(
            response
                .header("ratelimit-policy")
                .starts_with("\"identity_change\";q=10;w=86400"),
            "{}",
            response.header("ratelimit-policy")
        );
    }
    assert!(
        upstream.received().is_empty(),
        "a limited link exchanges nothing"
    );
    assert_eq!(
        reads(&api, "SELECT count(*)::text FROM provider_identities").await,
        ["0"]
    );
    // Another user's changes count in a bucket of their own.
    assert_eq!(add_password(&api, &grace, other).await.status, 409);
}

#[tokio::test]
async fn auth_091_each_change_tells_the_address_what_happened() {
    let (upstream, api) = standing().await;
    let address = mailbox::address("ada");
    gives(&upstream, identity("subject-1", &address, true));
    let ada = signed_in(&api).await;
    let retired = retired_identity(&api, &ada).await;

    assert_eq!(add_password(&api, &ada, PASSWORD).await.status, 200);
    let provider = provider_id(&api, &ada).await;
    assert_eq!(remove(&api, &ada, &provider).await.status, 204);
    assert_eq!(link(&api, &ada, &grant()).await.status, 200);
    let password = password_id(&api, &ada).await;
    assert_eq!(remove(&api, &ada, &password).await.status, 204);
    assert_eq!(remove(&api, &ada, &retired).await.status, 204);
    api.run_jobs().await;

    let emails = mailbox::to(&address).await;
    let subjects: Vec<&str> = emails.iter().map(|email| email.subject.as_str()).collect();
    assert_eq!(
        subjects,
        [
            // A provider that the deployment doesn't configure, by its key.
            "retired signs in to your AntWorker account no more",
            "The password of your AntWorker account was removed",
            "Stand-in now signs in to your AntWorker account",
            "Stand-in signs in to your AntWorker account no more",
            "A password now signs in to your AntWorker account",
        ]
    );
    for email in &emails {
        assert!(email.text.contains(&address), "{}", email.text);
        assert!(email.text.contains("If you didn't"), "{}", email.text);
        assert!(!email.text.contains("http"), "{}", email.text);
        assert!(!email.html.contains("href"), "{}", email.html);
    }
}

#[tokio::test]
async fn auth_091_with_email_off_no_change_of_the_identities_enqueues_an_email() {
    let upstream = Upstream::start().await;
    let api = Api::with(providers::stand_in(&upstream).set("EMAIL", "off")).await;
    gives(&upstream, identity("subject-1", "ada@example.com", false));
    let ada = signed_in(&api).await;

    assert_eq!(add_password(&api, &ada, PASSWORD).await.status, 200);
    let provider = provider_id(&api, &ada).await;
    assert_eq!(remove(&api, &ada, &provider).await.status, 204);
    assert_eq!(link(&api, &ada, &grant()).await.status, 200);

    assert!(
        reads(
            &api,
            "SELECT id::text FROM jobs WHERE kind = 'platform.send_email'"
        )
        .await
        .is_empty()
    );
}
