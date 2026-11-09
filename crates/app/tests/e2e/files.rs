//! Files from the deployed image (the files spec): a file stored in the deployment's files bucket,
//! a provider's over HTTPS, whose certificate an authority of the test's own signed, which the
//! deployment trusts as certs/files-ca.crt, once the deployment's scanner has scanned it.

use std::{thread, time::Duration};

use serde_json::Value;

use crate::{
    deployment::{Deployment, Workspace, streaming_client},
    docker::lossy,
    support,
};

/// How long the tests wait for each answer.
const ANSWER: Duration = Duration::from_secs(30);

/// Uploads `bytes` as the file `name` to `workspace`, and returns the answer's status and body.
fn upload(workspace: &Workspace, name: &str, bytes: Vec<u8>) -> (u16, String) {
    let (root, files, token) = (
        workspace.root.clone(),
        workspace.files.clone(),
        workspace.token.clone(),
    );
    let disposition = format!("attachment; filename=\"{name}\"");
    thread::spawn(move || {
        let response = streaming_client(&root, ANSWER)
            .post(&files)
            .bearer_auth(token)
            .header("content-type", "application/octet-stream")
            .header("content-disposition", disposition)
            .body(bytes)
            .send()
            .expect("an answer");
        (
            response.status().as_u16(),
            response.text().expect("the body"),
        )
    })
    .join()
    .expect("the upload")
}

/// The bytes of the file that the tests of a download store.
const STORED: &str = "stored in the bucket";

/// A deployment, with a workspace, in which its owner stored a text file of [`STORED`], and the
/// file, as the upload answered it.
async fn stored() -> (Deployment, Workspace, Value) {
    let deployment = Deployment::managed(&[]);
    let applied = deployment.apply(&deployment.api, &[]).await;
    assert!(applied.status.success(), "{}", lossy(&applied));
    let workspace = deployment.workspace();
    let (uploaded, file) = upload(&workspace, "notes.txt", STORED.as_bytes().to_vec());
    assert_eq!(uploaded, 201, "{file}");
    let file = serde_json::from_str::<Value>(&file).unwrap();
    (deployment, workspace, file)
}

/// Sends `GET` to `url` through the deployment's proxy, with the workspace owner's token if
/// `signed_in`, and `range` as its `Range` if given, and returns the answer's status and body.
fn get(workspace: &Workspace, url: String, signed_in: bool, range: Option<&str>) -> (u16, String) {
    let (root, token) = (workspace.root.clone(), workspace.token.clone());
    let range = range.map(str::to_owned);
    thread::spawn(move || {
        let mut request = streaming_client(&root, ANSWER).get(url);
        if signed_in {
            request = request.bearer_auth(token);
        }
        if let Some(range) = range {
            request = request.header("range", range);
        }
        let response = request.send().expect("an answer");
        (
            response.status().as_u16(),
            response.text().expect("the body"),
        )
    })
    .join()
    .expect("the download")
}

#[tokio::test]
async fn files_001_the_deployed_api_stores_a_file_in_its_bucket_and_serves_it_back() {
    let (_deployment, workspace, file) = stored().await;
    let url = format!(
        "{}/{}/content",
        workspace.files,
        file["id"].as_str().unwrap()
    );

    let (downloaded, bytes) = get(&workspace, url, true, None);

    assert_eq!(downloaded, 200, "{bytes}");
    assert_eq!(bytes, STORED);
}

#[tokio::test]
async fn files_009_the_deployed_proxy_passes_a_download_s_range() {
    let (_deployment, workspace, file) = stored().await;
    let url = format!(
        "{}/{}/content",
        workspace.files,
        file["id"].as_str().unwrap()
    );

    let (ranged, rest) = get(&workspace, url, true, Some("bytes=7-"));

    assert_eq!(ranged, 206, "{rest}");
    assert_eq!(rest, "in the bucket");
}

#[tokio::test]
async fn files_022_the_deployed_proxy_passes_a_signed_link_s_path_and_query() {
    let (_deployment, workspace, file) = stored().await;
    // A link is a path after the API's address, which the files' address begins with.
    let files = &workspace.files;
    let address = &files[..files.find("/v1/").expect("the files' path")];
    let url = format!("{address}{}", file["content_url"].as_str().unwrap());

    let (downloaded, bytes) = get(&workspace, url, false, None);

    assert_eq!(downloaded, 200, "{bytes}");
    assert_eq!(bytes, STORED);
}

#[tokio::test]
async fn files_005_the_deployed_api_refuses_a_file_in_which_its_scanner_finds_a_signature() {
    // The deployment's scanner holds the one signature of the EICAR test file, which the server
    // gave it (Server::seed_signatures).
    let deployment = Deployment::managed(&[]);
    let applied = deployment.apply(&deployment.api, &[]).await;
    assert!(applied.status.success(), "{}", lossy(&applied));
    let workspace = deployment.workspace();

    let (refused, problem) = upload(&workspace, "eicar.txt", support::scanner::eicar());
    let (stored, file) = upload(&workspace, "notes.txt", b"harmless".to_vec());

    assert_eq!(refused, 422, "{problem}");
    assert_eq!(
        serde_json::from_str::<Value>(&problem).unwrap()["code"],
        "files.malware_detected"
    );
    assert_eq!(stored, 201, "{file}");
}
