//! The links to files' bytes (FILES-021), and the signed URLs that private files' downloads take
//! in place of a credential (FILES-022, PLATFORM-080, AUTH-020).

use platform::{
    hashing::HmacKey,
    signed_urls::SignedUrls,
    types::{SessionId, Timestamp},
};
use serde_json::Value;

use super::{acme, delete, own::upload_own, upload};
use crate::support::{
    Api,
    api::Options,
    users::{self, SignedIn},
};

/// The signer of the tests' `HMAC_KEY`: 32 bytes of 7.
fn signer() -> SignedUrls {
    SignedUrls::new(HmacKey::new(&[7; 32]).unwrap())
}

/// A text file that `ada` uploaded to `workspace`, as the upload answered it.
async fn uploaded(api: &Api, ada: &SignedIn, workspace: &str, query: &str) -> Value {
    let path = format!("/v1/workspaces/{workspace}/files{query}");
    api.request(http::Method::POST, &path)
        .header("content-type", "application/octet-stream")
        .header("content-disposition", "attachment; filename=\"a.txt\"")
        .body(b"hello".to_vec())
        .bearer(&ada.access)
        .send()
        .await
        .json()
}

/// The value of the query parameter `name` in `url`.
fn parameter(url: &str, name: &str) -> String {
    let (_, query) = url.split_once('?').unwrap();
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(parameter, _)| parameter == name)
        .map(|(_, value)| value.into_owned())
        .unwrap()
}

#[tokio::test]
async fn files_021_a_private_file_s_link_is_its_download_signed_for_the_caller_s_session() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;

    let file = uploaded(&api, &ada, &workspace, "").await;

    let id = file["id"].as_str().unwrap();
    let url = file["content_url"].as_str().unwrap();
    assert!(
        url.starts_with(&format!("/v1/workspaces/{workspace}/files/{id}/content?")),
        "{url}"
    );
    assert_eq!(parameter(url, "session"), ada.session_id);
    // It lasts an hour, by the database's clock, to the second.
    let expires = file["content_url_expires_at"].as_str().unwrap();
    let lasting = api
        .database()
        .owner_reads(&format!(
            "SELECT (abs(extract(epoch FROM '{expires}'::timestamptz - now())) \
             BETWEEN 3540 AND 3600)::text"
        ))
        .await;
    assert_eq!(lasting, ["true"]);
    let seconds = api
        .database()
        .owner_reads(&format!(
            "SELECT extract(epoch FROM '{expires}'::timestamptz)::bigint::text"
        ))
        .await;
    assert_eq!([parameter(url, "expires")], seconds.as_slice());
}

#[tokio::test]
async fn files_021_a_public_file_s_link_is_its_public_download_which_never_expires() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;

    let file = uploaded(&api, &ada, &workspace, "?visibility=public").await;
    let own = upload_own(&api, &ada, "a.txt", b"hello", "?visibility=public", None)
        .await
        .json();

    assert_eq!(
        file["content_url"],
        format!(
            "/v1/public/workspace-files/{}",
            file["id"].as_str().unwrap()
        )
    );
    assert_eq!(file["content_url_expires_at"], Value::Null);
    assert_eq!(
        own["content_url"],
        format!("/v1/public/user-files/{}", own["id"].as_str().unwrap())
    );
    assert_eq!(own["content_url_expires_at"], Value::Null);
    let listed = api
        .get(&format!("/v1/workspaces/{workspace}/files"))
        .bearer(&ada.access)
        .send()
        .await
        .json();
    assert_eq!(listed["items"][0]["content_url"], file["content_url"]);
}

#[tokio::test]
async fn files_021_a_replayed_upload_answers_links_made_for_its_retry() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let other = users::new_session(&api, "ada@example.com").await;

    let first = upload(&api, &ada, &workspace, "a.txt", b"hello", Some("\"k1\"")).await;
    let again = upload(&api, &other, &workspace, "a.txt", b"hello", Some("\"k1\"")).await;

    assert_eq!(first.json()["id"], again.json()["id"]);
    let url = again.json()["content_url"].as_str().unwrap().to_owned();
    assert_eq!(parameter(&url, "session"), other.session_id);
    let own = upload_own(&api, &ada, "b.txt", b"hello", "", Some("\"k2\"")).await;
    let own_again = upload_own(&api, &other, "b.txt", b"hello", "", Some("\"k2\"")).await;
    assert_eq!(own.json()["id"], own_again.json()["id"]);
    let url = own_again.json()["content_url"].as_str().unwrap().to_owned();
    assert_eq!(parameter(&url, "session"), other.session_id);
}

#[tokio::test]
async fn files_022_a_signed_url_downloads_a_private_file_without_a_credential() {
    let api = Api::new().await;
    let (workspace, ada, grace) = acme(&api, "member").await;
    let file = uploaded(&api, &ada, &workspace, "").await;
    let own = upload_own(&api, &ada, "notes.txt", b"mine", "", None)
        .await
        .json();
    // A member's own read of the file links it for their session.
    let read = api
        .get(&format!(
            "/v1/workspaces/{workspace}/files/{}",
            file["id"].as_str().unwrap()
        ))
        .bearer(&grace.access)
        .send()
        .await
        .json();

    for (url, bytes) in [
        (&file["content_url"], &b"hello"[..]),
        (&read["content_url"], &b"hello"[..]),
        (&own["content_url"], &b"mine"[..]),
    ] {
        let response = api.get(url.as_str().unwrap()).send().await;
        assert_eq!(response.status, 200, "{url}");
        assert_eq!(response.body, bytes, "{url}");
        assert_eq!(response.header("cache-control"), "no-store");
        assert!(response.header("content-disposition").contains("filename"));
    }
}

#[tokio::test]
async fn files_022_a_signed_url_stops_working_once_its_session_ends() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let file = uploaded(&api, &ada, &workspace, "").await;

    api.request(http::Method::DELETE, "/v1/users/me/sessions/current")
        .bearer(&ada.access)
        .send()
        .await;
    let response = api.get(file["content_url"].as_str().unwrap()).send().await;

    assert_eq!(response.status, 401);
    assert_eq!(response.json()["code"], "request.unauthenticated");
}

#[tokio::test]
async fn files_022_a_signed_url_stops_working_once_its_user_loses_access_or_the_file_goes() {
    let api = Api::new().await;
    let (workspace, ada, grace) = acme(&api, "member").await;
    let file = uploaded(&api, &ada, &workspace, "").await;
    let id = file["id"].as_str().unwrap();
    let path = format!("/v1/workspaces/{workspace}/files/{id}");
    let graces = api.get(&path).bearer(&grace.access).send().await.json();

    api.database()
        .owner_executes(&format!(
            "DELETE FROM workspace_memberships WHERE user_id = '{}'",
            grace.user_id
        ))
        .await;
    let removed = api
        .get(graces["content_url"].as_str().unwrap())
        .send()
        .await;
    delete(&api, &ada, &workspace, id).await;
    let deleted = api.get(file["content_url"].as_str().unwrap()).send().await;

    // A removed member is no member of the workspace (ACCESS-003).
    assert_eq!(removed.status, 404);
    assert_eq!(removed.json()["code"], "request.not_found");
    assert_eq!(deleted.status, 404);
    assert_eq!(deleted.json()["code"], "files.not_found");
}

#[tokio::test]
async fn platform_080_a_signature_that_doesn_t_match_its_path_session_and_time_answers_401() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let file = uploaded(&api, &ada, &workspace, "").await;
    let other = uploaded(&api, &ada, &workspace, "").await;
    let url = file["content_url"].as_str().unwrap();
    let (_, query) = url.split_once('?').unwrap();
    let other_path = format!(
        "/v1/workspaces/{workspace}/files/{}/content",
        other["id"].as_str().unwrap()
    );
    let signature = parameter(url, "signature");
    let flipped = if signature.starts_with('A') { "B" } else { "A" };

    for refused in [
        // Another file's path, with this file's query.
        format!("{other_path}?{query}"),
        url.replace(&signature, &format!("{flipped}{}", &signature[1..])),
        url.replace(
            &format!("session={}", ada.session_id),
            &format!("session={}", uuid::Uuid::nil()),
        ),
        // Some of the three alone.
        format!("{other_path}?signature={signature}"),
    ] {
        let response = api.get(&refused).send().await;
        assert_eq!(response.status, 401, "{refused}");
        assert_eq!(
            response.json()["code"],
            "request.unauthenticated",
            "{refused}"
        );
    }
}

#[tokio::test]
async fn platform_080_a_signed_url_past_its_time_answers_401_signed_url_expired() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let file = uploaded(&api, &ada, &workspace, "").await;
    let path = format!(
        "/v1/workspaces/{workspace}/files/{}/content",
        file["id"].as_str().unwrap()
    );
    let session = SessionId::from_uuid(uuid::Uuid::parse_str(&ada.session_id).unwrap());
    // A time long past by the database's clock.
    let past =
        Timestamp::from_database(time::OffsetDateTime::from_unix_timestamp(1_577_836_800).unwrap());

    let expired = signer().sign(&path, session, past);
    let response = api.get(&expired.url).send().await;

    assert_eq!(response.status, 401);
    assert_eq!(response.json()["code"], "request.signed_url_expired");
}

#[tokio::test]
async fn platform_080_a_signed_url_counts_in_its_user_s_bucket() {
    let api = Api::with(Options::default().set("RATE_LIMIT_USER_REQUESTS", 50)).await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let file = uploaded(&api, &ada, &workspace, "").await;

    let response = api.get(file["content_url"].as_str().unwrap()).send().await;

    assert_eq!(response.status, 200);
    assert_eq!(response.header("ratelimit-policy"), "\"user\";q=50;w=60");
}

#[tokio::test]
async fn platform_080_a_route_that_takes_no_signed_url_refuses_its_parameters() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let file = uploaded(&api, &ada, &workspace, "").await;
    let (_, query) = file["content_url"]
        .as_str()
        .unwrap()
        .split_once('?')
        .unwrap();
    let path = format!(
        "/v1/workspaces/{workspace}/files/{}?{query}",
        file["id"].as_str().unwrap()
    );

    let response = api.get(&path).bearer(&ada.access).send().await;

    assert_eq!(response.status, 400);
    assert_eq!(response.json()["code"], "request.invalid_query");
}

#[tokio::test]
async fn auth_020_a_signed_url_s_session_proves_its_user_while_it_lasts() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let file = uploaded(&api, &ada, &workspace, "").await;
    let url = file["content_url"].as_str().unwrap();
    let db = api.database();

    // An access token that has expired leaves a session that lasts.
    db.owner_executes(&format!(
        "UPDATE sessions SET access_token_expires_at = now() - interval '1 second' \
         WHERE id = '{}'",
        ada.session_id
    ))
    .await;
    let lasting = api.get(url).send().await;
    // A refresh token that has expired, or an absolute lifetime that has, leaves none.
    db.owner_executes(&format!(
        "UPDATE sessions SET refresh_token_expires_at = now() - interval '1 second' \
         WHERE id = '{}'",
        ada.session_id
    ))
    .await;
    let unrefreshable = api.get(url).send().await;
    db.owner_executes(&format!(
        "UPDATE sessions SET refresh_token_expires_at = now() + interval '1 day', \
         expires_at = now() - interval '1 second' WHERE id = '{}'",
        ada.session_id
    ))
    .await;
    let ended = api.get(url).send().await;

    assert_eq!(lasting.status, 200);
    for response in [&unrefreshable, &ended] {
        assert_eq!(response.status, 401);
        assert_eq!(response.json()["code"], "request.unauthenticated");
    }
}

#[tokio::test]
async fn platform_080_a_signed_query_authenticates_alone_whatever_credential_comes_with_it() {
    let api = Api::new().await;
    let (workspace, ada, grace) = acme(&api, "member").await;
    let file = uploaded(&api, &ada, &workspace, "").await;
    let url = file["content_url"].as_str().unwrap();
    let own = upload_own(&api, &ada, "mine.txt", b"mine", "", None)
        .await
        .json();
    let own_url = own["content_url"].as_str().unwrap();
    let signature = parameter(url, "signature");
    let flipped = if signature.starts_with('A') { "B" } else { "A" };

    // A query that doesn't sign the path refuses the request, though its Bearer header would
    // authenticate it.
    let partial = api
        .get(&url.replace(&format!("&signature={signature}"), ""))
        .bearer(&ada.access)
        .send()
        .await;
    let tampered = api
        .get(&url.replace(&signature, &format!("{flipped}{}", &signature[1..])))
        .bearer(&ada.access)
        .send()
        .await;
    // A signed URL acts for its session's user, whose own file it serves to whoever sends it,
    // another user's Bearer header with it.
    let other = api.get(own_url).bearer(&grace.access).send().await;

    for refused in [&partial, &tampered] {
        assert_eq!(refused.status, 401);
        assert_eq!(refused.json()["code"], "request.unauthenticated");
    }
    assert_eq!(other.status, 200);
    assert_eq!(other.body, b"mine");
}

#[tokio::test]
async fn auth_020_a_session_that_doesn_t_exist_proves_none() {
    let api = Api::new().await;
    let ada = users::register(&api, "ada@example.com").await;
    let own = upload_own(&api, &ada, "mine.txt", b"mine", "", None)
        .await
        .json();
    let path = format!("/v1/users/me/files/{}/content", own["id"].as_str().unwrap());
    // A time long to come by the database's clock, signed for a session that no row holds.
    let later =
        Timestamp::from_database(time::OffsetDateTime::from_unix_timestamp(4_102_444_800).unwrap());
    let nobody = SessionId::from_uuid(uuid::Uuid::from_u128(7));

    let response = api
        .get(&signer().sign(&path, nobody, later).url)
        .send()
        .await;

    assert_eq!(response.status, 401);
    assert_eq!(response.json()["code"], "request.unauthenticated");
}

#[tokio::test]
async fn files_021_a_list_links_each_private_file_for_the_caller_s_session() {
    let api = Api::new().await;
    let (workspace, ada, grace) = acme(&api, "member").await;
    uploaded(&api, &ada, &workspace, "").await;
    upload_own(&api, &grace, "mine.txt", b"mine", "", None).await;

    let listed = api
        .get(&format!("/v1/workspaces/{workspace}/files"))
        .bearer(&grace.access)
        .send()
        .await
        .json();
    let own = api
        .get("/v1/users/me/files")
        .bearer(&grace.access)
        .send()
        .await
        .json();

    for item in [&listed["items"][0], &own["items"][0]] {
        assert_eq!(item["visibility"], "private");
        let url = item["content_url"].as_str().unwrap();
        assert_eq!(parameter(url, "session"), grace.session_id);
        assert!(item["content_url_expires_at"].is_string(), "{item}");
        let response = api.get(url).send().await;
        assert_eq!(response.status, 200, "{url}");
    }
}
