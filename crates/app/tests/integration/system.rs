//! The `system` module ([spec](../../../../docs/spec/system.md)).

use http::StatusCode;
use serde_json::{Value, json};

use crate::support::{
    Api, Options, TestDatabase,
    users::{self, SignedIn},
};

#[tokio::test]
async fn system_001_the_capabilities_list_each_feature_a_deployment_can_turn_off() {
    let api = Api::new().await;

    let response = api.get("/v1/capabilities").send().await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.header("content-type"), "application/json");
    // Email is the one feature a deployment can turn off, beside the optional parts the fork
    // keeps, and no version is reported; a deployment that configures no provider lists none
    // (AUTH-081).
    assert_eq!(
        response.json(),
        json!({"features": features(true), "sign_in_providers": []})
    );
}

#[tokio::test]
async fn system_001_a_feature_turned_off_is_false() {
    let api = Api::without_email().await;

    let response = api.get("/v1/capabilities").send().await;

    assert_eq!(
        response.json(),
        json!({"features": features(false), "sign_in_providers": []})
    );
}

/// The features of a deployment whose email is `email`: each optional part that changes what a
/// client offers is `true` while the fork keeps it (D118).
fn features(email: bool) -> Value {
    let mut features = serde_json::Map::new();
    features.insert("email".to_owned(), json!(email));
    // part: workspace_creation
    features.insert("workspace_creation".to_owned(), json!(true));
    // end part: workspace_creation
    // part: invitations
    features.insert("invitations".to_owned(), json!(true));
    // end part: invitations
    // part: files
    features.insert("files".to_owned(), json!(true));
    // end part: files
    Value::Object(features)
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

    // Production logs JSON lines, and the tests' environment readable text (PLATFORM-044).
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

/// Signs in as `email` with `password`, which must succeed, and returns the access token.
async fn sign_in(api: &Api, email: &str, password: &str) -> String {
    let response = users::sign_in(api, email, password).await;
    assert_eq!(response.status, StatusCode::CREATED, "{}", response.json());
    SignedIn::from(&response).access
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

/// Runs the binary's `seed` in development on `database`, which must succeed (TEST-03), and
/// returns the password it printed.
async fn seed(database: &TestDatabase) -> String {
    let output = database
        .run_api(&["seed"], &[("ENVIRONMENT", "development")])
        .await;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    stdout
        .lines()
        .find_map(|line| line.split_once("with the password "))
        .map(|(_, password)| password.trim_end_matches(':').to_owned())
        .unwrap()
}

#[tokio::test]
async fn system_003_seed_registers_its_accounts_through_the_routes() {
    let database = TestDatabase::migrated().await;

    let password = seed(&database).await;

    let api = Api::on(database, Options::default()).await;
    for email in [
        "owner@example.com",
        "admin@example.com",
        "member@example.com",
    ] {
        sign_in(&api, email, &password).await;
    }
    let names = api
        .database()
        .owner_reads(
            "SELECT name FROM users WHERE email IN \
             ('owner@example.com', 'admin@example.com', 'member@example.com') ORDER BY email",
        )
        .await;
    assert_eq!(names, ["Adam Admin", "Mia Member", "Olivia Owner"]);
    // Each request went through the pipeline as a client's does, from 127.0.0.1, and recorded
    // and published what a client's does: each registration its entry, and its session's start.
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT ((SELECT count(*) FROM users) = count(*))::text \
                 || ' ' || host(client.client_ip) || ' ' || client.user_agent \
                 FROM audit_entries entry \
                 JOIN audit_entry_clients client ON client.entry_id = entry.id \
                 WHERE entry.action = 'user.registered' \
                 GROUP BY client.client_ip, client.user_agent"
            )
            .await,
        ["true 127.0.0.1 seed"]
    );
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT ((SELECT count(DISTINCT user_id) FROM events \
                 WHERE name = 'session.created') = (SELECT count(*) FROM users))::text"
            )
            .await,
        ["true"]
    );
}

// part: workspace_creation
#[tokio::test]
async fn system_008_seed_has_olivia_and_mia_each_create_a_workspace_through_the_route() {
    let database = TestDatabase::migrated().await;

    let password = seed(&database).await;

    let api = Api::on(database, Options::default()).await;
    let olivia = sign_in(&api, "owner@example.com", &password).await;
    let mia = sign_in(&api, "member@example.com", &password).await;
    assert!(
        workspaces(&api, &olivia)
            .await
            .contains(&("Acme".to_owned(), "owner".to_owned()))
    );
    assert!(
        workspaces(&api, &mia)
            .await
            .contains(&("Side Project".to_owned(), "owner".to_owned()))
    );
    // Olivia's is the first workspace that the seed created.
    assert_eq!(
        api.database()
            .owner_reads("SELECT name FROM workspaces ORDER BY created_at LIMIT 1")
            .await,
        ["Acme"]
    );
    // Each creation went through the pipeline as a client's does: its entry names its creator
    // and its client, and its event its creator's session.
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT workspace.name || ' ' || actor.email || ' ' || host(client.client_ip) \
                 || ' ' || client.user_agent \
                 FROM audit_entries entry \
                 JOIN workspaces workspace ON workspace.id = entry.workspace_id \
                 JOIN users actor ON actor.id = entry.actor_user_id \
                 JOIN audit_entry_clients client ON client.entry_id = entry.id \
                 WHERE entry.action = 'workspace.created' ORDER BY workspace.name"
            )
            .await,
        [
            "Acme owner@example.com 127.0.0.1 seed",
            "Side Project member@example.com 127.0.0.1 seed",
        ]
    );
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT workspace.name FROM events event \
                 JOIN workspaces workspace \
                 ON workspace.id::text = event.payload->>'workspace_id' \
                 JOIN workspace_memberships membership \
                 ON membership.workspace_id = workspace.id AND membership.role = 'owner' \
                 WHERE event.name = 'membership.created' AND event.user_id = membership.user_id \
                 ORDER BY workspace.name"
            )
            .await,
        ["Acme", "Side Project"]
    );
}
// end part: workspace_creation

// part: workspace_setup
#[tokio::test]
async fn system_009_seed_sets_a_workspace_up_for_olivia_as_an_operator_s_run_does() {
    let database = TestDatabase::migrated().await;

    let password = seed(&database).await;

    let api = Api::on(database, Options::default()).await;
    let olivia = sign_in(&api, "owner@example.com", &password).await;
    assert!(
        workspaces(&api, &olivia)
            .await
            .contains(&("Acme Labs".to_owned(), "owner".to_owned()))
    );
    // The setup recorded what an operator's run records: the name against a later run, and the
    // workspace's first entry with no actor, no client, and its owner as the target.
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT setup.name || ' ' || (entry.actor_user_id IS NULL) || ' ' \
                 || target.email || ' ' || (SELECT count(*) FROM audit_entry_clients client \
                 WHERE client.entry_id = entry.id) \
                 FROM workspace_setups setup \
                 JOIN audit_entries entry ON entry.workspace_id = setup.workspace_id \
                 JOIN users target ON target.id = entry.target_user_id \
                 WHERE entry.action = 'workspace.created'"
            )
            .await,
        ["Acme Labs true owner@example.com 0"]
    );
}

#[tokio::test]
async fn system_009_with_email_on_the_setup_takes_olivia_s_proved_address_and_enqueues_its_email() {
    let database = TestDatabase::migrated().await;

    let output = database
        .run_api(
            &["seed"],
            &[("ENVIRONMENT", "development"), ("EMAIL", "smtp")],
        )
        .await;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        database
            .owner_reads(
                "SELECT (SELECT count(*) FROM workspace_setups)::text || ' ' \
                 || (SELECT count(*) FROM jobs job JOIN users owner ON owner.id = job.user_id \
                 WHERE job.kind = 'platform.send_email' AND owner.email = 'owner@example.com' \
                 AND job.payload::text LIKE '%was set up for you%')"
            )
            .await,
        ["1 1"]
    );
}
// end part: workspace_setup

#[tokio::test]
async fn system_003_the_subcommand_prints_the_accounts_their_password_and_the_workspaces() {
    let database = TestDatabase::migrated().await;

    let output = database
        .run_api(&["seed"], &[("ENVIRONMENT", "development")])
        .await;

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    assert!(
        stdout.contains(
            "Seeded the database. Sign in as any of these, with the password orchard-lantern-42-violet:"
        ),
        "{stdout}"
    );
    let printed = |name: &str, value: &str| {
        stdout.lines().any(|line| {
            let mut words = line.split_whitespace();
            words.next() == Some(name) && line.trim_end().ends_with(value)
        })
    };
    // Every account the seed registered, with or without the parts it keeps.
    let accounts = database
        .owner_reads("SELECT email || '|' || name FROM users")
        .await;
    assert!(accounts.len() >= 3, "{accounts:?}");
    for account in accounts {
        let (email, name) = account.split_once('|').unwrap();
        assert!(printed(email, name), "{email} in {stdout}");
    }
    for workspace in database
        .owner_reads("SELECT name || '|' || id::text FROM workspaces")
        .await
    {
        let (name, id) = workspace.split_once('|').unwrap();
        let first = name.split_whitespace().next().unwrap();
        assert!(printed(first, id), "{name} in {stdout}");
    }
}

#[tokio::test]
async fn system_006_with_email_on_seed_proves_each_account_s_address() {
    let database = TestDatabase::migrated().await;

    let output = database
        .run_api(
            &["seed"],
            &[("ENVIRONMENT", "development"), ("EMAIL", "smtp")],
        )
        .await;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    let unproven = database
        .owner_reads("SELECT email FROM users WHERE NOT email_verified")
        .await;
    assert!(unproven.is_empty(), "{unproven:?}");
    // Each account confirmed its code through the route, as a client does (AUTH-031).
    assert_eq!(
        database
            .owner_reads(
                "SELECT (SELECT count(*) FROM users)::text || ' ' \
                 || (SELECT count(*) FROM audit_entries WHERE action = 'email.verified')"
            )
            .await,
        database
            .owner_reads(
                "SELECT (SELECT count(*) FROM users)::text || ' ' || (SELECT count(*) FROM users)"
            )
            .await
    );
}

#[tokio::test]
async fn system_006_with_email_off_seed_confirms_nothing() {
    let database = TestDatabase::migrated().await;

    let output = database
        .run_api(
            &["seed"],
            &[("ENVIRONMENT", "development"), ("EMAIL", "off")],
        )
        .await;

    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert_eq!(
        database
            .owner_reads(
                "SELECT (SELECT count(*) FROM users WHERE email_verified)::text || ' ' \
                 || (SELECT count(*) FROM jobs WHERE kind = 'auth.send_verification_code')"
            )
            .await,
        ["0 0"]
    );
}

#[tokio::test]
async fn system_004_seed_stops_at_an_account_that_exists_and_changes_nothing_more() {
    let database = TestDatabase::migrated().await;
    seed(&database).await;
    let rows = "SELECT (SELECT count(*) FROM users) || ' ' || (SELECT count(*) FROM workspaces) \
                || ' ' || (SELECT count(*) FROM audit_entries)";
    let before = database.owner_reads(rows).await;

    let again = database
        .run_api(&["seed"], &[("ENVIRONMENT", "development")])
        .await;

    assert_eq!(again.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&again.stdout).contains(
        "owner@example.com is registered already, so this database holds the seed's data"
    ));
    assert_eq!(database.owner_reads(rows).await, before);
}

// part: invitations
#[tokio::test]
async fn system_005_seed_has_olivia_invite_adam_and_mia_who_accept_and_ian_whose_invitation_waits()
{
    let database = TestDatabase::migrated().await;

    let password = seed(&database).await;

    let api = Api::on(database, Options::default()).await;
    let olivia = sign_in(&api, "owner@example.com", &password).await;
    let adam = sign_in(&api, "admin@example.com", &password).await;
    let mia = sign_in(&api, "member@example.com", &password).await;
    let ian = sign_in(&api, "invitee@example.com", &password).await;
    // The first workspace the seed created: the one that Olivia joined first.
    let listed = api
        .get("/v1/workspaces")
        .bearer(&olivia)
        .send()
        .await
        .json();
    let first = listed["items"].as_array().unwrap().last().unwrap();
    let name = first["name"].as_str().unwrap().to_owned();
    let shared = first["id"].as_str().unwrap().to_owned();
    assert_eq!(
        workspaces(&api, &adam).await,
        [(name.clone(), "admin".to_owned())]
    );
    assert!(
        workspaces(&api, &mia)
            .await
            .contains(&(name, "member".to_owned()))
    );
    let members = api
        .get(&format!("/v1/workspaces/{shared}/members"))
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
    assert_eq!(pending["items"][0]["workspace_id"], shared.as_str());
    assert_eq!(pending["items"][0]["role"], "member");
    assert_eq!(pending["items"][0]["inviter_name"], "Olivia Owner");
    // Olivia invited each of them, at the role each holds.
    assert_eq!(
        api.database()
            .owner_reads(
                "SELECT actor.email || ' ' || target.email || ' ' || (entry.details->>'role') \
                 FROM audit_entries entry \
                 JOIN users actor ON actor.id = entry.actor_user_id \
                 JOIN users target ON target.id = entry.target_user_id \
                 WHERE entry.action = 'invitation.created' ORDER BY target.email"
            )
            .await,
        [
            "owner@example.com admin@example.com admin",
            "owner@example.com invitee@example.com member",
            "owner@example.com member@example.com member",
        ]
    );
    assert!(workspaces(&api, &ian).await.is_empty());
    assert_eq!(
        api.database()
            .owner_reads("SELECT name FROM users WHERE email = 'invitee@example.com'")
            .await,
        ["Ian Invitee"]
    );
}
// end part: invitations

#[tokio::test]
async fn system_007_seed_builds_its_data_while_the_deployment_requires_a_second_factor() {
    let database = TestDatabase::migrated().await;

    let output = database
        .run_api(
            &["seed"],
            &[
                ("ENVIRONMENT", "development"),
                ("SECOND_FACTOR_REQUIRED", "true"),
            ],
        )
        .await;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert_eq!(output.status.code(), Some(0), "{stdout}");
    let password = stdout
        .lines()
        .find_map(|line| line.split_once("with the password "))
        .map(|(_, password)| password.trim_end_matches(':').to_owned())
        .unwrap();
    // Its accounts were served what it sent as them.
    assert_eq!(
        database
            .owner_reads("SELECT (count(*) > 0)::text FROM workspace_memberships")
            .await,
        ["true"]
    );
    // Each account sets an app up at its first sign-in, as one that registered before the
    // deployment turned the requirement on does (AUTH-070).
    let api = Api::on(
        database,
        Options::default().set("SECOND_FACTOR_REQUIRED", true),
    )
    .await;
    let signed_in = users::sign_in(&api, "owner@example.com", &password).await;
    assert_eq!(signed_in.status, 201, "{}", signed_in.json());
    assert_eq!(signed_in.json()["second_factor_setup_required"], true);
}
