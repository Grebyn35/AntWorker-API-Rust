//! Startup failures of the real binary (OPS-03).

use serde_json::Value;

use crate::support::{Api, TestDatabase, operator::EMAIL_OFF, run_binary, second_factors, users};

#[tokio::test]
async fn ops_03_a_failure_before_logging_is_one_json_line_on_stderr_and_exit_code_1() {
    let output = run_binary(&["serve"], &[]).await;

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8(output.stderr).unwrap();
    let lines: Vec<&str> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    let line: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(line["level"], "ERROR");
    assert_eq!(line["message"], "startup failed");
    assert_eq!(line["startup.area"], "config");
    assert_eq!(line["exception.message"], "ENVIRONMENT is required");
    assert!(
        line.get("deployment.environment.name").is_none(),
        "the environment isn't known"
    );
}

#[tokio::test]
async fn ops_03_a_logged_startup_failure_is_one_error_line_and_exit_code_1() {
    let database = TestDatabase::migrated().await;
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = taken.local_addr().unwrap().to_string();

    let output = database
        .run_api(
            &["serve"],
            &[("ENVIRONMENT", "production"), ("LISTEN_ADDR", &address)],
        )
        .await;

    assert_eq!(output.status.code(), Some(1));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let errors: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line["level"] == "ERROR")
        .collect();
    assert_eq!(errors.len(), 1, "{stdout}");
    assert_eq!(errors[0]["message"], "startup failed");
    assert_eq!(errors[0]["startup.area"], "listen");
    assert_eq!(errors[0]["deployment.environment.name"], "production");
    drop(taken);
}

#[tokio::test]
async fn ops_03_a_subcommand_that_an_operator_runs_logs_what_it_did_as_one_info_line() {
    let api = Api::without_email().await;
    let ada = users::register(&api, "ada@example.com").await;
    second_factors::add_app(&api, &ada).await;
    let settings: Vec<(&str, &str)> = EMAIL_OFF
        .into_iter()
        .chain([("ENVIRONMENT", "production")])
        .collect();

    let output = api
        .database()
        .run_api(&["remove-second-factor", "ada@example.com"], &settings)
        .await;

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8(output.stdout).unwrap();
    let finished: Vec<Value> = stdout
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|line| line["message"] == "subcommand finished")
        .collect();
    assert_eq!(finished.len(), 1, "{stdout}");
    assert_eq!(finished[0]["level"], "INFO");
    assert_eq!(finished[0]["startup.area"], "remove-second-factor");
    assert!(
        finished[0]["startup.outcome"]
            .as_str()
            .unwrap()
            .starts_with("removed the account's authenticator app"),
        "{stdout}"
    );
    assert!(!stdout.contains("ada@example.com"), "{stdout}");
}
