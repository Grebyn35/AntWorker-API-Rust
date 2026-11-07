//! Files from the deployed image (the files spec): a file stored in the deployment's files bucket,
//! a provider's over HTTPS, whose certificate an authority of the test's own signed, which the
//! deployment trusts as certs/files-ca.crt; and the bundled files bucket, whose copy is in the
//! stand-in's bucket `files-copy`, encrypted, and comes back onto a new server (OPS-15).

use std::{thread, time::Duration};

use serde_json::{Value, json};

use crate::{
    deployment::{Deployment, post, post_as, streaming_client},
    docker::lossy,
    server::Server,
};

/// How long the tests wait for each answer.
const ANSWER: Duration = Duration::from_secs(30);

/// A user's session and workspace on a deployment that serves.
pub struct Account {
    /// The authority of Caddy's certificates, which the tests' clients trust.
    pub root: String,
    pub token: String,
    pub workspace: String,
}

impl Account {
    /// Registers a user on `deployment`, and creates a workspace that they own.
    pub fn register(deployment: &Deployment) -> Self {
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
        Self {
            root,
            token,
            workspace,
        }
    }

    /// The URL of the workspace's files on `deployment`, with `path` after it.
    fn files(&self, deployment: &Deployment, path: &str) -> String {
        deployment.url(
            "https",
            443,
            &format!("/v1/workspaces/{}/files{path}", self.workspace),
        )
    }

    /// Uploads `body` as the text file `name` to the workspace on `deployment`, and returns the
    /// file's ID.
    pub fn upload(&self, deployment: &Deployment, name: &str, body: &str) -> String {
        let (root, url, token) = (
            self.root.clone(),
            self.files(deployment, ""),
            self.token.clone(),
        );
        let (name, body) = (name.to_owned(), body.to_owned());
        let (uploaded, file) = thread::spawn(move || {
            let response = streaming_client(&root, ANSWER)
                .post(&url)
                .bearer_auth(token)
                .header("content-type", "application/octet-stream")
                .header(
                    "content-disposition",
                    format!("attachment; filename=\"{name}\""),
                )
                .body(body)
                .send()
                .expect("an answer");
            (
                response.status().as_u16(),
                response.text().expect("the body"),
            )
        })
        .join()
        .expect("the upload");
        assert_eq!(uploaded, 201, "{file}");
        serde_json::from_str::<Value>(&file).unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Downloads the file `id` of the workspace on `deployment`, and returns the answer's status
    /// and body.
    pub fn download(&self, deployment: &Deployment, id: &str) -> (u16, String) {
        let (root, url, token) = (
            self.root.clone(),
            self.files(deployment, &format!("/{id}/content")),
            self.token.clone(),
        );
        thread::spawn(move || {
            let response = streaming_client(&root, ANSWER)
                .get(url)
                .bearer_auth(token)
                .send()
                .expect("an answer");
            (
                response.status().as_u16(),
                response.text().expect("the body"),
            )
        })
        .join()
        .expect("the download")
    }

    /// Downloads the file `id` on `deployment` until its bytes are back, for at most 2 minutes,
    /// while each answer is 503 `files.restoring` (FILES-014), and returns the last answer.
    pub fn download_once_back(&self, deployment: &Deployment, id: &str) -> (u16, String) {
        for _ in 0..60 {
            let answer = self.download(deployment, id);
            if answer.0 != 503 || !answer.1.contains("files.restoring") {
                return answer;
            }
            // The server waits, so the test's own thread holds no clock of its own (CODE-06).
            deployment.server.shell_ok("sleep 2");
        }
        self.download(deployment, id)
    }

    /// The names of the workspace's files on `deployment`, the latest uploaded first.
    pub fn names(&self, deployment: &Deployment) -> Vec<String> {
        let (root, url, token) = (
            self.root.clone(),
            self.files(deployment, ""),
            self.token.clone(),
        );
        let listed = thread::spawn(move || {
            streaming_client(&root, ANSWER)
                .get(url)
                .bearer_auth(token)
                .send()
                .expect("an answer")
                .text()
                .expect("the body")
        })
        .join()
        .expect("the list");
        serde_json::from_str::<Value>(&listed).unwrap()["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["name"].as_str().unwrap().to_owned())
            .collect()
    }
}

/// The key of the object of the file `id`, read from `deployment`'s database.
pub fn object_key(deployment: &Deployment, id: &str) -> String {
    deployment.sql(&format!(
        "SELECT object_key FROM workspace_files WHERE id = '{id}'"
    ))
}

/// Whether `stored`, an object's bytes as a bucket stores them, is rclone crypt's encryption of
/// `plain`: its header, and none of the plain bytes (D101).
fn encrypted(stored: &[u8], plain: &str) -> bool {
    stored.starts_with(b"RCLONE\0\0")
        && !stored
            .windows(plain.len())
            .any(|window| window == plain.as_bytes())
}

#[tokio::test]
async fn files_001_the_deployed_api_stores_a_file_in_its_bucket_and_serves_it_back() {
    let deployment = Deployment::managed(&[]);
    let applied = deployment.apply(&deployment.api, &[]).await;
    assert!(applied.status.success(), "{}", lossy(&applied));
    let account = Account::register(&deployment);

    let id = account.upload(&deployment, "notes.txt", "stored in the bucket");
    let downloaded = account.download(&deployment, &id);

    assert_eq!(downloaded, (200, "stored in the bucket".to_owned()));
}

#[tokio::test]
async fn ops_15_the_bundled_files_bucket_s_copy_holds_each_upload_encrypted_under_its_key() {
    let deployment = Deployment::managed(&[]).with_bundled_files(&[]);
    let applied = deployment.apply(&deployment.api, &[]).await;
    assert!(applied.status.success(), "{}", lossy(&applied));
    let account = Account::register(&deployment);

    let id = account.upload(&deployment, "notes.txt", "copied off the server");
    let key = object_key(&deployment, &id);
    let copied = deployment
        .server
        .wait_for_stand_in_object("files-copy", &key);
    let downloaded = account.download(&deployment, &id);
    let queue = deployment.server.shell_ok(
        "echo 'fs.ls /buckets/files/copy-queue/workspaces' | docker exec --interactive \
         deployment-files-1 weed shell -master=localhost:9333 -filer=localhost:8888 2> /dev/null",
    );
    let status = deployment.server.deploy(&["status"]);

    assert!(encrypted(&copied, "copied off the server"), "{copied:?}");
    assert_eq!(downloaded, (200, "copied off the server".to_owned()));
    // The bucket's queue holds nothing once the copy holds what it queued.
    assert!(
        !queue.contains(key.trim_start_matches("workspaces/")),
        "{queue}"
    );
    // The API's own bucket is the bundled one, which the replicas reach on the server.
    assert!(deployment.server.stand_in_object("files", &key).is_none());
    assert!(status.status.success(), "{}", lossy(&status));
    let status = lossy(&status);
    assert!(status.contains("the files' copy:"), "{status}");
    assert!(
        !status.contains("last copied                never"),
        "{status}"
    );
}

#[tokio::test]
async fn ops_15_a_new_server_with_a_managed_database_gets_the_bundled_files_back_as_it_deploys() {
    // Both servers' shares at once, so no test holds one while it waits for another.
    let (lost_server, new_server) = Server::start_two();
    let lost = Deployment::managed_on(lost_server, &[]).with_bundled_files(&[]);
    let applied = lost.apply(&lost.api, &[]).await;
    assert!(applied.status.success(), "{}", lossy(&applied));
    let account = Account::register(&lost);
    let id = account.upload(&lost, "notes.txt", "kept off the server");
    lost.server
        .wait_for_stand_in_object("files-copy", &object_key(&lost, &id));
    let new = lost.on_a_new_server(new_server);
    // The server is lost, while its database's server and the copy's bucket stay.
    lost.server.shell_ok(
        "docker ps --quiet --filter label=com.docker.compose.project=deployment | xargs docker rm --force",
    );

    let deployed = new.apply(&new.api, &[]).await;
    let account = Account {
        root: new.server.caddy_root(),
        ..account
    };
    let downloaded = account.download_once_back(&new, &id);

    assert!(deployed.status.success(), "{}", lossy(&deployed));
    assert!(
        lossy(&deployed).contains("The files bucket lacks objects that its copy holds"),
        "{}",
        lossy(&deployed)
    );
    assert_eq!(downloaded, (200, "kept off the server".to_owned()));
    assert_eq!(account.names(&new), ["notes.txt"]);
}
