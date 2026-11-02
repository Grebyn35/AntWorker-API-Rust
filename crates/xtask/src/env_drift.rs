//! `env-drift`, which `just setup` and `just dev` run: it warns of each setting that
//! `.env.example` has and `.env` lacks, and of each that `.env` sets to another value, since
//! `just setup` writes `.env` only once, and later commits add settings and change development
//! values (OPS-19). The generated passwords and key differ by design, so a setting whose example
//! holds a placeholder, such as `__APP_PASSWORD__`, is left out. A warning names the setting and
//! the example's value, which the repository holds anyway, and never the value in `.env`, which
//! may be a secret. It warns and never fails: a developer may set a value of their own on purpose.

use std::{path::Path, process::ExitCode};

/// Warns of how `.env` differs from `.env.example`, at the repository's root.
pub fn run() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let (Ok(env), Ok(example)) = (read(&root.join(".env")), read(&root.join(".env.example")))
    else {
        // Without a `.env`, `just setup` writes one from the example, which matches it.
        return ExitCode::SUCCESS;
    };
    for warning in drift(&env, &example) {
        warn(&warning);
    }
    ExitCode::SUCCESS
}

/// A warning for each setting that `example` has and `env` lacks or sets to another value, but
/// for those whose example holds a placeholder.
pub fn drift(env: &str, example: &str) -> Vec<String> {
    let ours = settings(env);
    settings(example)
        .into_iter()
        .filter(|(_, value)| !has_placeholder(value))
        .filter_map(
            |(name, value)| match ours.iter().find(|(setting, _)| *setting == name) {
                None => Some(format!(
                    "warning: .env lacks {name}, which .env.example sets to `{value}`: copy its \
                     line from .env.example"
                )),
                Some((_, mine)) if *mine != value => Some(format!(
                    "warning: .env sets {name} otherwise than .env.example, which sets it to \
                     `{value}`: keep yours if you chose it, or copy the example's line"
                )),
                Some(_) => None,
            },
        )
        .collect()
}

/// Each setting of a file of settings, `NAME=value` on a line of its own, comments left out.
fn settings(text: &str) -> Vec<(&str, &str)> {
    text.lines()
        .filter_map(|line| line.split_once('='))
        .filter(|(name, _)| {
            !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        })
        .map(|(name, value)| (name, value.trim()))
        .collect()
}

/// Whether `value` holds a placeholder that `just setup` replaces with a generated secret, such
/// as `__HMAC_KEY__`.
fn has_placeholder(value: &str) -> bool {
    value.split("__").skip(1).step_by(2).any(|inside| {
        !inside.is_empty() && inside.chars().all(|c| c.is_ascii_uppercase() || c == '_')
    })
}

#[expect(
    clippy::disallowed_methods,
    reason = "the task runs as a synchronous program, with no async runtime to stall"
)]
fn read(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

#[expect(clippy::print_stderr, reason = "the warnings are the task's output")]
fn warn(message: &str) {
    eprintln!("{message}");
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = "# Where the API runs.\n\
                           ENVIRONMENT=development\n\
                           CORS_ALLOWED_ORIGINS=http://127.0.0.1:8080\n\
                           STREAM_HEARTBEAT_SECONDS=15\n\
                           HMAC_KEY=__HMAC_KEY__\n\
                           DATABASE_URL=postgres://app:__APP_PASSWORD__@127.0.0.1:5432/api\n\
                           SMTP_PASSWORD=\n";

    #[test]
    fn ops_19_a_env_written_from_the_example_draws_no_warning() {
        let env = EXAMPLE
            .replace("__HMAC_KEY__", "c2VjcmV0")
            .replace("__APP_PASSWORD__", "0f3a");

        assert_eq!(drift(&env, EXAMPLE), Vec::<String>::new());
    }

    #[test]
    fn ops_19_a_setting_the_env_lacks_or_sets_otherwise_draws_a_warning_without_its_value() {
        let env = "ENVIRONMENT=development\n\
                   CORS_ALLOWED_ORIGINS=\n\
                   HMAC_KEY=c2VjcmV0\n\
                   DATABASE_URL=postgres://app:0f3a@127.0.0.1:5432/api\n\
                   SMTP_PASSWORD=hunter2\n";

        let warnings = drift(env, EXAMPLE);

        assert_eq!(warnings.len(), 3, "{warnings:#?}");
        assert!(warnings[0].starts_with(
            "warning: .env sets CORS_ALLOWED_ORIGINS otherwise than .env.example, which sets it \
             to `http://127.0.0.1:8080`"
        ));
        assert!(warnings[1].starts_with(
            "warning: .env lacks STREAM_HEARTBEAT_SECONDS, which .env.example sets to `15`"
        ));
        assert!(warnings[2].starts_with("warning: .env sets SMTP_PASSWORD otherwise"));
        assert!(warnings.iter().all(|warning| !warning.contains("hunter2")));
    }
}
