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

#[tokio::test]
async fn files_001_the_deployed_api_stores_a_file_in_its_bucket_and_serves_it_back() {
    let deployment = Deployment::managed(&[]);
    let applied = deployment.apply(&deployment.api, &[]).await;
    assert!(applied.status.success(), "{}", lossy(&applied));
    let workspace = deployment.workspace();

    let (uploaded, file) = upload(&workspace, "notes.txt", b"stored in the bucket".to_vec());

    assert_eq!(uploaded, 201, "{file}");
    let id = serde_json::from_str::<Value>(&file).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let Workspace { root, files, token } = workspace;
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
