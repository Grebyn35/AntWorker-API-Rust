//! Email from the deployed image (the mail spec), through a mail server on the tests' own server.

use serde_json::{Value, json};

use crate::{
    deployment::{Deployment, post, post_as},
    docker::lossy,
    support::users,
};

#[tokio::test]
async fn mail_001_the_deployed_api_sends_an_invitations_email_through_its_mail_server() {
    let deployment = Deployment::managed(&[]);
    let mail = deployment.server.start_mail_server();
    let mail: Vec<(&str, &str)> = mail
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    deployment.set(&mail);
    let applied = deployment.apply(&deployment.api, &[]).await;
    assert!(applied.status.success(), "{}", lossy(&applied));
    let root = deployment.server.caddy_root();
    let url = |path: &str| deployment.url("https", 443, path);
    let (status, body) = post(
        root.clone(),
        url("/v1/users"),
        users::registration("ada@example.com").to_string(),
    );
    assert_eq!(status, 201, "{body}");
    let token = serde_json::from_str::<Value>(&body).unwrap()["access_token"]
        .as_str()
        .unwrap()
        .to_owned();
    let (status, body) = post_as(
        root.clone(),
        url("/v1/workspaces"),
        Some(token.clone()),
        json!({ "name": "Acme" }).to_string(),
    );
    assert_eq!(status, 201, "{body}");
    let workspace = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    let (status, body) = post_as(
        root,
        url(&format!("/v1/workspaces/{workspace}/invitations")),
        Some(token),
        json!({ "email": "bob@example.com", "role": "member" }).to_string(),
    );

    assert_eq!(status, 201, "{body}");
    // The email reaches the mail server once a replica's job sends it, within about a second.
    deployment.server.shell_ok(
        "timeout 60 sh -c 'until wget -q -O - \
         \"http://127.0.0.1:8025/api/v1/search?query=to%3Abob%40example.com\" 2> /dev/null \
         | grep -q \"Ada Lovelace invited you to Acme on AntWorker\"; do sleep 1; done'",
    );
}
