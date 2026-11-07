//! Files from the deployed image (the files spec): a file stored in the deployment's files bucket,
//! a provider's over HTTPS, whose certificate an authority of the test's own signed, which the
//! deployment trusts as certs/files-ca.crt.

use std::{thread, time::Duration};

use serde_json::{Value, json};

use crate::{
    deployment::{Deployment, post, post_as, streaming_client},
    docker::lossy,
};

/// How long the tests wait for each answer.
const ANSWER: Duration = Duration::from_secs(30);

#[tokio::test]
async fn files_001_the_deployed_api_stores_a_file_in_its_bucket_and_serves_it_back() {
    let deployment = Deployment::managed(&[]);
    let applied = deployment.apply(&deployment.api, &[]).await;
    assert!(applied.status.success(), "{}", lossy(&applied));
    let root = deployment.server.caddy_root();
    let (registered, session) = post(
        root.clone(),
        deployment.url("https", 443, "/v1/users"),
        json!({ "email": "ada@example.com", "password": "plum-tractor-41-quiet", "name": "Ada" })
            .to_string(),
    );
    assert_eq!(registered, 201, "{session}");
    let token = serde_json::from_str::<Value>(&session).unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let (created, workspace) = post_as(
        root.clone(),
        deployment.url("https", 443, "/v1/workspaces"),
        Some(token.clone()),
        json!({ "name": "Acme" }).to_string(),
    );
    assert_eq!(created, 201, "{workspace}");
    let workspace = serde_json::from_str::<Value>(&workspace).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let files = deployment.url("https", 443, &format!("/v1/workspaces/{workspace}/files"));

    let (uploaded, file) = {
        let (root, files, token) = (root.clone(), files.clone(), token.clone());
        thread::spawn(move || {
            let response = streaming_client(&root, ANSWER)
                .post(&files)
                .bearer_auth(token)
                .header("content-type", "application/octet-stream")
                .header("content-disposition", "attachment; filename=\"notes.txt\"")
                .body("stored in the bucket")
                .send()
                .expect("an answer");
            (
                response.status().as_u16(),
                response.text().expect("the body"),
            )
        })
        .join()
        .expect("the upload")
    };
    assert_eq!(uploaded, 201, "{file}");
    let id = serde_json::from_str::<Value>(&file).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (downloaded, bytes) = thread::spawn(move || {
        let response = streaming_client(&root, ANSWER)
            .get(format!("{files}/{id}/content"))
            .bearer_auth(token)
            .send()
            .expect("an answer");
        (
            response.status().as_u16(),
            response.text().expect("the body"),
        )
    })
    .join()
    .expect("the download");

    assert_eq!(downloaded, 200, "{bytes}");
    assert_eq!(bytes, "stored in the bucket");
}
