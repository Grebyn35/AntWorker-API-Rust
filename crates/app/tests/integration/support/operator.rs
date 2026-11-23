//! What a subcommand of the API binary that an operator runs wrote, and how it ended (OPS-03),
//! with the settings of a deployment whose email reaches the tests' Mailpit.

use std::process::Output;

use super::Api;

/// The settings of a deployment that sends email through the tests' Mailpit, as the in-process
/// API's own settings send it.
pub const SENDS_EMAIL: [(&str, &str); 6] = [
    ("ENVIRONMENT", "test"),
    ("EMAIL", "smtp"),
    ("SMTP_HOST", "127.0.0.1"),
    ("SMTP_PORT", "1025"),
    ("SMTP_SECURITY", "none"),
    ("EMAIL_FROM", "AntWorker <noreply@example.com>"),
];

/// The settings of a deployment with email off (PLATFORM-024).
pub const EMAIL_OFF: [(&str, &str); 2] = [("ENVIRONMENT", "test"), ("EMAIL", "off")];

/// Runs the binary's `subcommand` with `arguments` on `api`'s database, in a deployment with the
/// settings `env`.
pub async fn run(api: &Api, subcommand: &str, env: &[(&str, &str)], arguments: &[&str]) -> Output {
    let arguments: Vec<&str> = [subcommand]
        .into_iter()
        .chain(arguments.iter().copied())
        .collect();
    api.database().run_api(&arguments, env).await
}

/// What `output` wrote, its logs among it, to stdout and stderr together.
pub fn printed(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Asserts that `output` exited 0, and that what it wrote holds `subcommand finished`, `area` and
/// `outcome`, each anywhere in it, as the line that says what the subcommand `area` did holds them,
/// and none of `absent`, since the logs hold no personal data (OPS-03, OBS-04).
pub fn finished(output: &Output, area: &str, outcome: &str, absent: &[&str]) {
    let written = printed(output);
    assert_eq!(output.status.code(), Some(0), "{written}");
    assert!(written.contains("subcommand finished"), "{written}");
    assert!(written.contains(area), "{written}");
    assert!(written.contains(outcome), "{written}");
    for text in absent {
        assert!(!written.contains(text), "{text}: {written}");
    }
}

/// Asserts that `output` exited 1, and that what it wrote holds `startup failed`, `area` and
/// `message`, each anywhere in it, as the failure that OPS-03 gives the subcommand `area` holds
/// them, and none of `absent`, since the logs hold no personal data (OBS-04).
pub fn refused(output: &Output, area: &str, message: &str, absent: &[&str]) {
    let written = printed(output);
    assert_eq!(output.status.code(), Some(1), "{written}");
    assert!(written.contains("startup failed"), "{written}");
    assert!(written.contains(area), "{written}");
    assert!(written.contains(message), "{written}");
    for text in absent {
        assert!(!written.contains(text), "{text}: {written}");
    }
}
