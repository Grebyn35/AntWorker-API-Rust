use std::ffi::OsString;

use crate::support::{Options, TestDatabase, api, not_unicode};

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

/// Each setting of `.env.example` whose type is a whole number in a range, with its range.
fn ranged_settings() -> Vec<(String, i64, i64)> {
    ranged(".env.example")
}

/// Each setting of the example `example`, a path from the repository's root, whose type is a
/// whole number in a range, with its range: the `Type:` line of its comment, then the setting it
/// describes.
#[expect(
    clippy::disallowed_methods,
    reason = "a test may block (CODE-05), and reads the settings' documentation"
)]
fn ranged(example: &str) -> Vec<(String, i64, i64)> {
    let example =
        std::fs::read_to_string(format!("{}/../../{example}", env!("CARGO_MANIFEST_DIR")))
            .expect("read the example");
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

// part: files
#[test]
fn ops_01_the_deletion_delay_that_a_deploy_derives_is_one_that_the_api_takes() {
    let api = ranged(".env.example");
    let deployment = ranged("deploy/settings.env.example");
    let highest = |settings: &[(String, i64, i64)], name: &str| {
        settings
            .iter()
            .find(|(each, _, _)| each == name)
            .map(|(_, _, high)| *high)
            .unwrap()
    };

    let delay = highest(&api, "FILE_DELETION_DELAY_DAYS");
    // Left empty, the delay is BACKUP_RETENTION_DAYS plus 7 (deploy/tasks/apply.sh).
    assert!(highest(&deployment, "BACKUP_RETENTION_DAYS") + 7 <= delay);
    assert_eq!(highest(&deployment, "FILE_DELETION_DELAY_DAYS"), delay);
}
// end part: files

#[test]
fn auth_007_startup_refuses_an_access_token_that_would_outlive_its_refresh_token() {
    let longer = Options::default()
        .set("ACCESS_TOKEN_SECONDS", 7_200)
        .set("REFRESH_TOKEN_SECONDS", 3_600);
    let equal = Options::default()
        .set("ACCESS_TOKEN_SECONDS", 3_600)
        .set("REFRESH_TOKEN_SECONDS", 3_600);

    assert_eq!(
        api::refusal(&longer),
        "ACCESS_TOKEN_SECONDS must be at most REFRESH_TOKEN_SECONDS"
    );
    // An access token that lasts as long as its refresh token is one startup takes.
    api::configuration(&equal);
}

#[test]
fn ops_01_each_number_setting_is_refused_just_outside_its_range() {
    for (name, low, high) in &ranged_settings() {
        for value in [low - 1, high + 1] {
            let refusal = api::refusal(&Options::default().set(name.clone(), value));

            assert_eq!(
                refusal,
                format!("{name} must be a whole number from {low} to {high}"),
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
                )
                .replace(
                    "__ENCRYPTION_KEYS__",
                    "1:BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=",
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
        "remove-second-factor" => settings_read(|settings| {
            settings.telemetry()?;
            settings.operator()?;
            Ok(())
        }),
        // part: workspace_setup
        "setup-workspace" => settings_read(|settings| {
            settings.telemetry()?;
            settings.operator()?;
            Ok(())
        }),
        // end part: workspace_setup
        _ => return None,
    })
}

/// Whether only the test environment reads the setting `name`: the configured client's origin,
/// and the stand-in provider's settings (SEC-10).
fn is_only_for_tests(name: &str) -> bool {
    name == "OUTBOUND_TEST_ORIGIN" || name.starts_with("STAND_IN_")
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
    let read: std::collections::BTreeSet<String> =
        ["serve", "bootstrap", "migrate", "remove-second-factor"]
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
    // The settings that only the test environment reads: the configured client's origin, and the
    // stand-in provider's, which only the tests' build compiles (SEC-10, the auth spec's "Signing
    // in through a provider").
    let read: std::collections::BTreeSet<String> = read
        .into_iter()
        .filter(|name| !is_only_for_tests(name))
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
        let read: std::collections::BTreeSet<String> = subcommand_settings(subcommand)
            .expect("a subcommand that a service of the reference deployment runs")
            .into_iter()
            .filter(|name| !is_only_for_tests(name))
            .collect();
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
    let expected =
        std::collections::BTreeSet::from(["bootstrap", "migrate", "remove-second-factor", "serve"]);
    // part: workspace_setup
    let expected: std::collections::BTreeSet<&str> =
        expected.into_iter().chain(["setup-workspace"]).collect();
    // end part: workspace_setup
    assert_eq!(
        subcommands, expected,
        "the services that run the API's image"
    );
}
