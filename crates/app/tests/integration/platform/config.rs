use std::ffi::OsString;

use crate::support::{TestDatabase, not_unicode};

#[tokio::test]
async fn ops_01_a_setting_that_isnt_unicode_fails_startup() {
    let db = TestDatabase::migrated().await;

    for setting in ["LOG_LEVEL", "ENVIRONMENT", "DATABASE_OWNER_URL_FILE"] {
        let mut env = vec![("ENVIRONMENT", OsString::from("test"))];
        env.retain(|(name, _)| *name != setting);
        env.push((setting, not_unicode()));

        let output = db.run_api(&["migrate"], &env).await;

        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{setting}: {errors}");
        assert!(
            errors.contains(&format!("{setting} must be valid Unicode")),
            "{setting}: {errors}"
        );
    }
}

/// Each setting of `.env.example` whose type is a whole number in a range, with its range: the
/// `Type:` line of its comment, then the setting it describes.
#[expect(
    clippy::disallowed_methods,
    reason = "a test may block (CODE-05), and reads the settings' documentation"
)]
fn ranged_settings() -> Vec<(String, i64, i64)> {
    let example =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env.example"))
            .expect("read .env.example");
    let typed = example
        .lines()
        .filter(|line| line.starts_with("# Type: whole number"))
        .count();
    let mut found = Vec::new();
    let mut range = None;
    for line in example.lines() {
        if let Some(kind) = line.strip_prefix("# Type: whole number") {
            let bounds = kind
                .split_once(" from ")
                .and_then(|(_, rest)| rest.split_once(" to "))
                .map(|(low, rest)| {
                    let high: String = rest.chars().take_while(char::is_ascii_digit).collect();
                    (low.parse::<i64>().unwrap(), high.parse::<i64>().unwrap())
                });
            range = bounds;
        } else if let Some((name, _)) = line.split_once('=')
            && !line.starts_with('#')
            && let Some((low, high)) = range.take()
        {
            found.push((name.to_owned(), low, high));
        }
    }
    assert_eq!(
        found.len(),
        typed,
        "a whole number's range wasn't read: {found:?}"
    );
    found
}

#[test]
fn ops_01_each_number_setting_is_refused_just_outside_its_range() {
    let ranged = ranged_settings();
    let base = [
        ("ENVIRONMENT", "test"),
        ("LISTEN_ADDR", "0.0.0.0:8080"),
        ("DATABASE_URL", "postgres://app:secret@localhost:5432/api"),
        ("HMAC_KEY", "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc="),
        ("EMAIL", "smtp"),
        ("SMTP_HOST", "127.0.0.1"),
        ("SMTP_SECURITY", "none"),
        ("EMAIL_FROM", "AntWorker <noreply@example.com>"),
    ];

    for (name, low, high) in &ranged {
        for value in [low - 1, high + 1] {
            let mut vars: std::collections::HashMap<String, OsString> = base
                .iter()
                .map(|(key, value)| ((*key).to_owned(), OsString::from(*value)))
                .collect();
            vars.insert(name.clone(), OsString::from(value.to_string()));
            let settings = platform::config::Settings::new(
                move |key: &str| vars.get(key).cloned(),
                |_: &std::path::Path| Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
            );

            let error = settings
                .server()
                .err()
                .or_else(|| app::Config::read(&settings).err());

            assert_eq!(
                error.map(|error| error.to_string()),
                Some(format!(
                    "{name} must be a whole number from {low} to {high}"
                )),
                "{name}={value}"
            );
        }
    }
}
