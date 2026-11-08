//! Public files: an upload's visibility, and the downloads that anyone may make (FILES-020,
//! FILES-023), counted in a bucket of their own (PLATFORM-054).

use super::{acme, own::upload_own, stored_names, upload_queried};
use crate::support::{
    Api,
    api::{Options, Response},
    files, users, workspaces,
};

/// Uploads `bytes`, a PNG's name, to `workspace` as `user`, with `query` as the request's query.
async fn upload_query(
    api: &Api,
    user: &users::SignedIn,
    workspace: &str,
    query: &str,
    bytes: &[u8],
) -> Response {
    upload_queried(api, user, workspace, query, "a.png", bytes).await
}

/// The smallest PNG's bytes: its signature, then its header chunk.
const PNG: &[u8] =
    b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";

#[tokio::test]
async fn files_020_an_upload_is_private_unless_its_visibility_is_public() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;

    let private = upload_query(&api, &ada, &workspace, "", PNG).await;
    let public = upload_query(&api, &ada, &workspace, "?visibility=public", PNG).await;
    let named_private = upload_query(&api, &ada, &workspace, "?visibility=private", PNG).await;

    assert_eq!(private.status, 201);
    assert_eq!(private.json()["visibility"], "private");
    assert_eq!(public.status, 201);
    assert_eq!(public.json()["visibility"], "public");
    assert_eq!(named_private.json()["visibility"], "private");
    let own = upload_own(&api, &ada, "a.txt", b"hello", "?visibility=public", None).await;
    assert_eq!(own.status, 201);
    assert_eq!(own.json()["visibility"], "public");
}

#[tokio::test]
async fn files_001_a_member_and_an_admin_may_upload_a_public_file() {
    let api = Api::new().await;
    let (workspace, _, grace) = acme(&api, "member").await;
    let alan = users::register(&api, "alan@example.com").await;
    workspaces::join(api.database(), &workspace, &alan.user_id, "admin").await;

    for uploader in [&grace, &alan] {
        let response = upload_query(&api, uploader, &workspace, "?visibility=public", PNG).await;

        assert_eq!(response.status, 201);
        assert_eq!(response.json()["visibility"], "public");
        assert_eq!(response.json()["uploader_id"], uploader.user_id);
    }
}

#[tokio::test]
async fn files_020_another_visibility_answers_400_with_the_body_unread() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;

    // An empty body would be refused once read: the query's refusal comes first.
    let response = upload_query(&api, &ada, &workspace, "?visibility=Public", b"").await;

    assert_eq!(response.status, 400);
    let problem = response.json();
    assert_eq!(problem["code"], "request.invalid_query");
    assert_eq!(problem["errors"][0]["field"], "/visibility");
    assert!(stored_names(&api, &workspace).await.is_empty());
}

#[tokio::test]
async fn files_023_anyone_downloads_a_public_file_under_its_id_and_its_kind_s_extension() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let uploaded = upload_query(&api, &ada, &workspace, "?visibility=public", PNG)
        .await
        .json();
    let id = uploaded["id"].as_str().unwrap();
    let own = upload_own(
        &api,
        &ada,
        "notes.txt",
        b"hello",
        "?visibility=public",
        None,
    )
    .await
    .json();
    let own_id = own["id"].as_str().unwrap();
    // A JPEG named with its kind's second extension.
    let photo = upload_queried(
        &api,
        &ada,
        &workspace,
        "?visibility=public",
        "photo.jpeg",
        b"\xff\xd8\xff\xe0 a photo",
    )
    .await
    .json();
    let photo_id = photo["id"].as_str().unwrap();

    let jpeg = api
        .get(&format!("/v1/public/workspace-files/{photo_id}"))
        .send()
        .await;
    let image = api
        .get(&format!("/v1/public/workspace-files/{id}"))
        .send()
        .await;
    let text = api
        .get(&format!("/v1/public/user-files/{own_id}"))
        .send()
        .await;

    assert_eq!(image.status, 200);
    assert_eq!(image.body, PNG);
    assert_eq!(image.header("content-type"), "image/png");
    // Each is named by its ID and its kind's first extension, so every browser saves it with
    // that extension, and never by its own name (D115).
    assert_eq!(
        image.header("content-disposition"),
        format!("inline; filename=\"{id}.png\"; filename*=UTF-8''{id}.png")
    );
    assert_eq!(jpeg.status, 200);
    assert_eq!(
        jpeg.header("content-disposition"),
        format!("inline; filename=\"{photo_id}.jpg\"; filename*=UTF-8''{photo_id}.jpg")
    );
    assert_eq!(text.status, 200);
    assert_eq!(text.body, b"hello");
    assert_eq!(
        text.header("content-disposition"),
        format!("attachment; filename=\"{own_id}.txt\"; filename*=UTF-8''{own_id}.txt")
    );
    for response in [&image, &text, &jpeg] {
        assert_eq!(response.header("cache-control"), "public, max-age=300");
        assert_eq!(response.header("x-content-type-options"), "nosniff");
        assert_eq!(
            response.header("content-security-policy"),
            "default-src 'none'; sandbox"
        );
        assert_eq!(response.header("accept-ranges"), "bytes");
        // The name is personal data, which a public download withholds (SEC-11).
        let headers = format!("{:?}", response.headers);
        assert!(
            !headers.contains("notes") && !headers.contains("photo"),
            "{headers}"
        );
    }
    assert_eq!(image.header("etag"), format!("\"{id}\""));
}

#[tokio::test]
async fn files_023_a_private_file_or_one_of_the_other_kind_answers_404() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let db = api.database();
    let private = files::arrange(db, &workspace, "Private").await;
    let public = files::arrange(db, &workspace, "Public").await;
    files::publish(db, "workspace_files", &public).await;
    let own_private = files::arrange_own(db, &ada.user_id, "Own").await;

    for path in [
        format!("/v1/public/workspace-files/{private}"),
        format!("/v1/public/user-files/{own_private}"),
        // A workspace's public file isn't a user's, and an ID that isn't a UUID names none.
        format!("/v1/public/user-files/{public}"),
        "/v1/public/workspace-files/not-a-uuid".to_owned(),
    ] {
        let response = api.get(&path).send().await;
        assert_eq!(response.status, 404, "{path}");
        assert_eq!(response.json()["code"], "files.not_found", "{path}");
    }
    assert_eq!(
        api.get(&format!("/v1/public/workspace-files/{public}"))
            .send()
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn files_023_a_member_s_download_of_a_public_file_is_no_one_else_s_to_keep() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let db = api.database();
    let public = files::arrange(db, &workspace, "Public").await;
    files::publish(db, "workspace_files", &public).await;

    // A member's download carries the name, so no shared cache keeps it (SEC-12).
    let response = api
        .get(&format!(
            "/v1/workspaces/{workspace}/files/{public}/content"
        ))
        .bearer(&ada.access)
        .send()
        .await;

    assert_eq!(response.status, 200);
    assert_eq!(response.header("cache-control"), "no-store");
    assert!(response.header("content-disposition").contains("Public"));
}

#[tokio::test]
async fn platform_054_a_request_under_the_public_prefix_counts_in_a_bucket_of_its_own() {
    let api = Api::with(
        Options::default()
            .set("RATE_LIMIT_PUBLIC_REQUESTS", 2)
            .set("RATE_LIMIT_IP_REQUESTS", 100),
    )
    .await;
    let (workspace, _, _) = acme(&api, "member").await;
    let db = api.database();
    let public = files::arrange(db, &workspace, "Public").await;
    files::publish(db, "workspace_files", &public).await;
    let path = format!("/v1/public/workspace-files/{public}");

    let first = api.get(&path).send().await;
    let second = api.get(&path).send().await;
    let third = api.get(&path).send().await;
    let elsewhere = api.get("/v1/capabilities").send().await;

    assert_eq!(first.status, 200);
    assert_eq!(first.header("ratelimit-policy"), "\"public\";q=2;w=60");
    assert_eq!(second.status, 200);
    assert_eq!(third.status, 429);
    assert_eq!(third.json()["code"], "request.rate_limited");
    assert_eq!(third.header("ratelimit-policy"), "\"public\";q=2;w=60");
    // Every other request counts in the `ip` bucket, which the public downloads left alone.
    assert_eq!(elsewhere.status, 200);
    assert_eq!(elsewhere.header("ratelimit-policy"), "\"ip\";q=100;w=60");
}

#[tokio::test]
async fn files_012_a_bucket_that_doesn_t_answer_makes_a_public_download_answer_503() {
    // Nothing listens on port 9 of the loopback address.
    let api = Api::with(Options::default().set("S3_ENDPOINT", "http://127.0.0.1:9")).await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let db = api.database();
    let public = files::arrange(db, &workspace, "Public").await;
    files::publish(db, "workspace_files", &public).await;
    let own = files::arrange_own(db, &ada.user_id, "Own").await;
    files::publish(db, "user_files", &own).await;

    for path in [
        format!("/v1/public/workspace-files/{public}"),
        format!("/v1/public/user-files/{own}"),
    ] {
        let response = api.get(&path).send().await;

        assert_eq!(response.status, 503, "{path}");
        assert_eq!(response.json()["code"], "files.storage_unavailable");
        assert_eq!(response.header("retry-after"), "1");
    }
}

#[tokio::test]
async fn files_020_a_file_s_visibility_never_changes() {
    let api = Api::new().await;
    let (workspace, ada, _) = acme(&api, "member").await;
    let db = api.database();
    files::arrange(db, &workspace, "Private").await;
    files::arrange_own(db, &ada.user_id, "Own").await;

    // The API's role may change no file's row, and no route of a file takes a change.
    for statement in [
        "UPDATE workspace_files SET visibility = 'public'",
        "UPDATE user_files SET visibility = 'public'",
    ] {
        let refused = db.try_execute(&db.app_url(), statement).await;

        assert!(
            refused.is_err_and(|error| error.contains("permission denied")),
            "{statement}"
        );
    }
    for (path, item) in api.document()["paths"].as_object().unwrap() {
        if path.contains("files") {
            assert!(
                item.get("patch").is_none() && item.get("put").is_none(),
                "{path}"
            );
        }
    }
}

#[tokio::test]
async fn files_023_a_public_download_counts_in_the_bucket_of_public_downloads() {
    let api = Api::with(Options::default().set("RATE_LIMIT_PUBLIC_REQUESTS", 7)).await;
    let (workspace, _, _) = acme(&api, "member").await;
    let db = api.database();
    let public = files::arrange(db, &workspace, "Public").await;
    files::publish(db, "workspace_files", &public).await;

    let response = api
        .get(&format!("/v1/public/workspace-files/{public}"))
        .send()
        .await;

    assert_eq!(response.status, 200);
    assert_eq!(response.header("ratelimit-policy"), "\"public\";q=7;w=60");
}

#[tokio::test]
async fn platform_056_a_request_under_the_public_prefix_names_the_public_bucket() {
    let api = Api::with(Options::default().set("RATE_LIMIT_PUBLIC_REQUESTS", 5)).await;

    // An ID that names no public file answers 404, which the bucket counted too.
    let response = api
        .get("/v1/public/user-files/00000000-0000-0000-0000-000000000000")
        .send()
        .await;

    assert_eq!(response.status, 404);
    assert_eq!(response.header("ratelimit-policy"), "\"public\";q=5;w=60");
    assert_eq!(response.header("ratelimit"), "\"public\";r=4;t=60");
}
