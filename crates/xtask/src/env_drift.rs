//! `env-drift`, which `just setup` and `just dev` run: it warns of each setting that
//! `.env.example` has and `.env` lacks, and of each that `.env` still sets to a value that
//! `.env.example` once had and has since changed, since `just setup` writes `.env` only once, and
//! later commits add settings and change development values (OPS-19). A value that `.env.example`
//! never had is one the developer chose, and draws no warning; git's history of `.env.example`
//! tells the two apart. The generated passwords and key, whose example holds a placeholder, such
//! as `__APP_PASSWORD__`, are left out. A warning names the setting and the example's value, which
//! the repository holds anyway, and never the value in `.env`, which may be a secret. It warns and
//! never fails.

use std::{path::Path, process::Command, process::ExitCode};

/// The file of settings that `just setup` copies into `.env`.
const EXAMPLE: &str = ".env.example";

/// Warns of how `.env` lags behind `.env.example`, at the repository's root.
pub fn run() -> ExitCode {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let (Ok(env), Ok(example)) = (read(&root.join(".env")), read(&root.join(EXAMPLE))) else {
        // Without a `.env`, `just setup` writes one from the example, which matches it.
        return ExitCode::SUCCESS;
    };
    for warning in drift(&env, &example, &history(&root)) {
        warn(&warning);
    }
    ExitCode::SUCCESS
}

/// A warning for each setting that `example` has and `env` lacks, or that `env` sets to a value
/// that `history`, the lines that commits removed from or added to the example, shows it once
/// had, but for the settings whose example holds a placeholder.
pub fn drift(env: &str, example: &str, history: &str) -> Vec<String> {
    let ours = settings(env);
    let earlier = settings(history);
    settings(example)
        .into_iter()
        .filter(|(_, value)| !has_placeholder(value))
        .filter_map(
            |(name, value)| match ours.iter().find(|(setting, _)| *setting == name) {
                None => Some(format!(
                    "warning: .env lacks {name}, which .env.example sets to `{value}`: copy its \
                     line from .env.example"
                )),
                Some((_, mine)) if *mine != value && earlier.contains(&(name, *mine)) => {
                    Some(format!(
                        "warning: .env sets {name} to an earlier value of .env.example's, which \
                         now sets it to `{value}`: copy the example's line, unless you chose the \
                         earlier value"
                    ))
                }
                Some(_) => None,
            },
        )
        .collect()
}

/// The lines that the commits of the repository at `root` removed from and added to the example,
/// with their `-` and `+`: none where git or the history isn't there.
fn history(root: &Path) -> String {
    Command::new("git")
        .args(["log", "--format=", "--unified=0", "--patch", "--", EXAMPLE])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| !line.starts_with("---") && !line.starts_with("+++"))
                .filter_map(|line| line.strip_prefix(['-', '+']))
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
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
                           LOG_LEVEL=info\n\
                           HMAC_KEY=__HMAC_KEY__\n\
                           DATABASE_URL=postgres://app:__APP_PASSWORD__@127.0.0.1:5432/api\n";

    /// What the commits did to the example: `CORS_ALLOWED_ORIGINS` was empty, and
    /// `STREAM_HEARTBEAT_SECONDS` came later.
    const HISTORY: &str = "CORS_ALLOWED_ORIGINS=\n\
                           CORS_ALLOWED_ORIGINS=http://127.0.0.1:8080\n\
                           STREAM_HEARTBEAT_SECONDS=15\n\
                           LOG_LEVEL=info\n";

    #[test]
    fn ops_19_a_env_written_from_the_example_draws_no_warning() {
        let env = EXAMPLE
            .replace("__HMAC_KEY__", "c2VjcmV0")
            .replace("__APP_PASSWORD__", "0f3a");

        assert_eq!(drift(&env, EXAMPLE, HISTORY), Vec::<String>::new());
    }

    #[test]
    fn ops_19_a_missing_setting_and_an_earlier_default_draw_warnings_without_the_env_s_value() {
        let env = "ENVIRONMENT=development\n\
                   CORS_ALLOWED_ORIGINS=\n\
                   LOG_LEVEL=info\n\
                   HMAC_KEY=c2VjcmV0\n\
                   DATABASE_URL=postgres://app:0f3a@127.0.0.1:5432/api\n";

        let warnings = drift(env, EXAMPLE, HISTORY);

        assert_eq!(warnings.len(), 2, "{warnings:#?}");
        assert!(warnings[0].starts_with(
            "warning: .env sets CORS_ALLOWED_ORIGINS to an earlier value of .env.example's, \
             which now sets it to `http://127.0.0.1:8080`"
        ));
        assert!(warnings[1].starts_with(
            "warning: .env lacks STREAM_HEARTBEAT_SECONDS, which .env.example sets to `15`"
        ));
    }

    #[test]
    fn ops_19_a_value_the_developer_chose_draws_no_warning() {
        let env = "ENVIRONMENT=development\n\
                   CORS_ALLOWED_ORIGINS=http://localhost:3000\n\
                   STREAM_HEARTBEAT_SECONDS=16\n\
                   LOG_LEVEL=debug\n\
                   HMAC_KEY=c2VjcmV0\n\
                   DATABASE_URL=postgres://app:0f3a@127.0.0.1:5432/api\n";

        assert_eq!(drift(env, EXAMPLE, HISTORY), Vec::<String>::new());
    }
}
