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
        ("S3_ENDPOINT", "https://bucket.invalid"),
        ("S3_REGION", "us-east-1"),
        ("S3_BUCKET", "files"),
        ("S3_ACCESS_KEY_ID", "key"),
        ("S3_SECRET_ACCESS_KEY", "secret"),
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

/// The setting of `.env.example` that Compose reads, and the API doesn't.
const COMPOSE_SETTING: &str = "POSTGRES_PASSWORD";

/// The settings of `.env.example`, with its values, and the generated ones that `just setup`
/// writes in place of its placeholders.
#[expect(
    clippy::disallowed_methods,
    reason = "a test may block (CODE-05), and reads the settings' documentation"
)]
fn example_settings() -> std::collections::BTreeMap<String, OsString> {
    let example =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../.env.example"))
            .expect("read .env.example");
    dotenvy::from_read_iter(example.as_bytes())
        .map(|item| {
            let (name, value) = item.expect("a line of .env.example");
            let value = value
                .replace("__ADMIN_PASSWORD__", "secret")
                .replace("__OWNER_PASSWORD__", "secret")
                .replace("__APP_PASSWORD__", "secret")
                .replace(
                    "__HMAC_KEY__",
                    "BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=",
                );
            (name, OsString::from(value))
        })
        .collect()
}

/// A lookup of variables that records each one it is asked for.
type Lookup<'a> = Box<dyn Fn(&str) -> Option<OsString> + 'a>;

/// Settings that record each variable they are asked for.
type Recording<'a> =
    platform::config::Settings<Lookup<'a>, fn(&std::path::Path) -> std::io::Result<String>>;

/// The settings that `read` asks for, given `.env.example`'s values: each name it asks for
/// whose `<NAME>_FILE` it asks for too, as every setting's read does (SEC-01). The variables that
/// must stay unset, such as OpenTelemetry's and libpq's, are asked for alone.
fn settings_read<R>(read: R) -> std::collections::BTreeSet<String>
where
    R: for<'a> Fn(&Recording<'a>) -> Result<(), platform::config::ConfigError>,
{
    let values = example_settings();
    let asked = std::cell::RefCell::new(std::collections::BTreeSet::new());
    let env: Lookup<'_> = Box::new(|name: &str| {
        asked.borrow_mut().insert(name.to_owned());
        values.get(name).cloned()
    });
    let no_files: fn(&std::path::Path) -> std::io::Result<String> =
        |_| Err(std::io::Error::from(std::io::ErrorKind::NotFound));
    let settings = platform::config::Settings::new(env, no_files);
    read(&settings).expect("the settings of .env.example are valid");
    drop(settings);
    let asked = asked.into_inner();
    asked
        .iter()
        .filter(|name| asked.contains(&format!("{name}_FILE")))
        .cloned()
        .collect()
}

/// The settings each subcommand that a container of the reference deployment runs reads, as
/// `main` reads them, and `None` for any other subcommand.
fn subcommand_settings(subcommand: &str) -> Option<std::collections::BTreeSet<String>> {
    Some(match subcommand {
        "serve" => settings_read(|settings| {
            settings.telemetry()?;
            settings.server()?;
            app::Config::read(settings)?;
            Ok(())
        }),
        "bootstrap" => settings_read(|settings| {
            settings.telemetry()?;
            settings.bootstrap()?;
            Ok(())
        }),
        "migrate" => settings_read(|settings| {
            settings.telemetry()?;
            settings.migrate()?;
            Ok(())
        }),
        _ => return None,
    })
}

/// The settings a service of `deploy/compose.yaml` passes the API, each `<NAME>_FILE` as
/// `<NAME>`.
fn container_settings(service: &serde_json::Value) -> std::collections::BTreeSet<String> {
    service["environment"]
        .as_object()
        .expect("an environment")
        .keys()
        .map(|name| name.strip_suffix("_FILE").unwrap_or(name).to_owned())
        .collect()
}

#[test]
#[expect(
    clippy::disallowed_methods,
    reason = "a test may block (CODE-05), and reads the deployment's Compose file"
)]
fn ops_02_each_setting_the_api_reads_is_documented_and_each_container_gets_only_its_own() {
    let read: std::collections::BTreeSet<String> = ["serve", "bootstrap", "migrate"]
        .into_iter()
        .flat_map(|subcommand| subcommand_settings(subcommand).unwrap_or_default())
        .chain(settings_read(|settings| {
            settings.health()?;
            Ok(())
        }))
        .collect();
    let documented: std::collections::BTreeSet<String> = example_settings()
        .into_keys()
        .filter(|name| name != COMPOSE_SETTING)
        .collect();
    assert_eq!(
        read.difference(&documented).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "settings the API reads that .env.example lacks"
    );
    assert_eq!(
        documented.difference(&read).collect::<Vec<_>>(),
        Vec::<&String>::new(),
        "settings of .env.example that the API doesn't read"
    );

    let compose: serde_json::Value = serde_saphyr::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../deploy/compose.yaml"
        ))
        .expect("read deploy/compose.yaml"),
    )
    .expect("deploy/compose.yaml is YAML");
    let mut subcommands = std::collections::BTreeSet::new();
    for (name, service) in compose["services"].as_object().expect("services") {
        if !service["image"]
            .as_str()
            .is_some_and(|image| image.contains("API_IMAGE"))
        {
            continue;
        }
        let subcommand = service["command"][0].as_str().expect("a command");
        let passed = container_settings(service);
        let read = subcommand_settings(subcommand)
            .expect("a subcommand that a service of the reference deployment runs");
        assert_eq!(
            passed.difference(&read).collect::<Vec<_>>(),
            Vec::<&String>::new(),
            "{name} passes settings that {subcommand} doesn't read"
        );
        assert_eq!(
            read.difference(&passed).collect::<Vec<_>>(),
            Vec::<&String>::new(),
            "{name} doesn't pass settings that {subcommand} reads"
        );
        subcommands.insert(subcommand);
    }
    assert_eq!(
        subcommands,
        ["bootstrap", "migrate", "serve"].into(),
        "the services that run the API's image"
    );
}
