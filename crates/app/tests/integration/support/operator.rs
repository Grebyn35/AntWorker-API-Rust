//! What a subcommand of the API binary that an operator runs wrote, and how it ended (OPS-03),
//! with the settings of a deployment whose email reaches the tests' Mailpit.

use std::process::Output;

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

/// What `output` wrote, its logs among it, to stdout and stderr together.
pub fn printed(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// Asserts that `output` exited 0 with the one line that says what the subcommand `area` did,
/// which holds `outcome`, and that it wrote none of `absent`, since the logs hold no personal
/// data (OPS-03, OBS-04).
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

/// Asserts that `output` exited 1 with the one failure that OPS-03 gives the subcommand `area`,
/// whose message is `message`, and that it wrote none of `absent`, since the logs hold no
/// personal data (OBS-04).
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
