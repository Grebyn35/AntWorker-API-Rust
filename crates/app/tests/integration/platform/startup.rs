//! Startup failures of the real binary (OPS-03).

use serde_json::Value;

use crate::support::{TestDatabase, run_binary};

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
