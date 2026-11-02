//! The `system` module ([spec](../../../../docs/spec/system.md)).

use http::StatusCode;
use serde_json::{Value, json};

use crate::support::{Api, Options, TestDatabase};

#[tokio::test]
async fn system_001_the_capabilities_list_each_feature_a_deployment_can_turn_off() {
    let api = Api::new().await;

    let response = api.get("/v1/capabilities").send().await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("content-type"), "application/json");
    // Email is the one feature a deployment can turn off, and no version is reported.
    assert_eq!(response.json(), json!({"features": {"email": true}}));
}

#[tokio::test]
async fn system_001_a_feature_turned_off_is_false() {
    let api = Api::with(Options::default().set("EMAIL", "off")).await;

    let response = api.get("/v1/capabilities").send().await;

    assert_eq!(response.json(), json!({"features": {"email": false}}));
}

#[tokio::test]
async fn system_002_seed_refuses_outside_development_before_it_connects() {
    let database = TestDatabase::migrated().await;

    let output = database
        .run_api(
            &["seed"],
            &[
                ("ENVIRONMENT", "production"),
                // A database that no connection reaches: the refusal comes first.
                ("DATABASE_URL", "postgres://app:unused@127.0.0.1:1/none"),
            ],
        )
        .await;
    let test = database
        .run_api(&["seed"], &[("ENVIRONMENT", "test")])
        .await;

    // Production logs JSON lines, and the tests' environment readable text (PLATFORM-034).
    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let failure = stdout
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|line| line["message"] == "startup failed");
    assert!(failure.is_some(), "no startup failure in {stdout}");
    let failure = failure.unwrap();
    assert_eq!(failure["startup.area"], "seed");
    assert!(
        failure["exception.message"]
            .as_str()
            .unwrap()
            .starts_with("seed runs only when ENVIRONMENT is development")
    );
    assert_eq!(test.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&test.stdout);
    assert!(
        stdout.contains("seed runs only when ENVIRONMENT is development"),
        "{stdout}"
    );
    assert_eq!(
        database
            .owner_reads("SELECT count(*)::text FROM users")
            .await,
        ["0"]
    );
}

/// Signs in as `email` with `password`, and returns the access token.
async fn sign_in(api: &Api, email: &str, password: &str) -> String {
    let response = api
        .post(
            "/v1/sessions",
            &json!({ "email": email, "password": password }),
        )
        .send()
        .await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.json());
    response.json()["access_token"].as_str().unwrap().to_owned()
}

/// The name and the caller's role of each workspace that `token`'s caller belongs to.
async fn workspaces(api: &Api, token: &str) -> Vec<(String, String)> {
    let response = api.get("/v1/workspaces").bearer(token).send().await;
    let mut workspaces: Vec<(String, String)> = response.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| {
            (
                item["name"].as_str().unwrap().to_owned(),
                item["role"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    workspaces.sort();
    workspaces
}

#[tokio::test]
async fn system_003_seed_creates_its_accounts_and_workspaces_through_the_routes() {
    let api = Api::new().await;

    let report = app::seed(api.router()).await.unwrap();

    assert_eq!(
        &report.accounts[..3],
        [
            ("owner@example.com", "Olivia Owner"),
            ("admin@example.com", "Adam Admin"),
            ("member@example.com", "Mia Member"),
        ]
    );
    let olivia = sign_in(&api, "owner@example.com", report.password).await;
    let adam = sign_in(&api, "admin@example.com", report.password).await;
    let mia = sign_in(&api, "member@example.com", report.password).await;
    assert_eq!(
        workspaces(&api, &olivia).await,
        [("Acme".to_owned(), "owner".to_owned())]
    );
    assert!(
        workspaces(&api, &mia)
            .await
            .contains(&("Side Project".to_owned(), "owner".to_owned()))
    );
    assert!(
        !workspaces(&api, &adam)
            .await
            .iter()
            .any(|(_, role)| role == "owner")
    );
    // Each request went through the pipeline as a client's does, from 127.0.0.1.
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT DISTINCT host(client_ip) || ' ' || user_agent FROM audit_entries \
                 WHERE action IN ('user.registered', 'workspace.created')"
            )
            .await,
        ["127.0.0.1 seed"]
    );
    assert_eq!(
        api.database()
            .owner_reads("SELECT count(*)::text FROM events WHERE type = 'membership.created'")
            .await,
        [workspaces(&api, &olivia).await.len()
            + workspaces(&api, &adam).await.len()
            + workspaces(&api, &mia).await.len()]
        .map(|count| count.to_string())
    );
}

#[tokio::test]
async fn system_004_seed_stops_at_an_account_that_exists_and_changes_nothing_more() {
    let api = Api::new().await;
    app::seed(api.router()).await.unwrap();
    let users = || {
        api.database()
            .owner_reads("SELECT count(*)::text FROM users")
    };
    let before = users().await;

    let again = app::seed(api.router()).await;

    let error = again.err().expect("a second seed fails").to_string();
    assert!(
        error.starts_with(
            "owner@example.com is registered already, so this database holds the seed's data"
        ),
        "{error}"
    );
    assert_eq!(users().await, before);
}

// part: invitations
#[tokio::test]
async fn system_005_seed_has_olivia_invite_adam_and_mia_who_accept_and_ian_whose_invitation_waits()
{
    let api = Api::new().await;

    let report = app::seed(api.router()).await.unwrap();

    assert_eq!(report.accounts[3], ("invitee@example.com", "Ian Invitee"));
    let olivia = sign_in(&api, "owner@example.com", report.password).await;
    let adam = sign_in(&api, "admin@example.com", report.password).await;
    let mia = sign_in(&api, "member@example.com", report.password).await;
    let ian = sign_in(&api, "invitee@example.com", report.password).await;
    assert_eq!(
        workspaces(&api, &adam).await,
        [("Acme".to_owned(), "admin".to_owned())]
    );
    assert_eq!(
        workspaces(&api, &mia).await,
        [
            ("Acme".to_owned(), "member".to_owned()),
            ("Side Project".to_owned(), "owner".to_owned())
        ]
    );
    let acme = api
        .get("/v1/workspaces")
        .bearer(&olivia)
        .send()
        .await
        .json()["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let members = api
        .get(&format!("/v1/workspaces/{acme}/members"))
        .bearer(&olivia)
        .send()
        .await
        .json();
    let mut roles: Vec<&str> = members["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|member| member["role"].as_str().unwrap())
        .collect();
    roles.sort_unstable();
    assert_eq!(roles, ["admin", "member", "owner"]);
    let pending = api
        .get("/v1/users/me/invitations")
        .bearer(&ian)
        .send()
        .await
        .json();
    assert_eq!(pending["items"].as_array().unwrap().len(), 1);
    assert_eq!(pending["items"][0]["workspace_id"], acme.as_str());
    assert_eq!(pending["items"][0]["role"], "member");
    assert!(workspaces(&api, &ian).await.is_empty());
}
// end part: invitations
