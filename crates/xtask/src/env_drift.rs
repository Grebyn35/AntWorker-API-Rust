//! `env-drift`, which `just setup` and `just dev` run: it warns of each setting that
//! `.env.example` has and `.env` lacks, and of each that `.env` still sets to a value that
//! `.env.example` once had and has since changed, since `just setup` writes `.env` only once, and
//! later commits add settings and change development values (OPS-20). A value that `.env.example`
//! never had is one the developer chose, and draws no warning; git's history of `.env.example`
//! tells the two apart. The generated passwords and key, whose example holds a placeholder, such
//! as `__APP_PASSWORD__`, differ by design: one draws a warning only when `.env` lacks it. A
//! warning names the setting, with the example's value, which the repository holds anyway, unless
//! it is a placeholder, and never the value in `.env`, which may be a secret. It warns and never
//! fails.

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

/// A warning for each setting that `example` has and `env` lacks, and for each that `env` sets to
/// a value other than the example's that `history`, the lines that commits removed from or added
/// to the example, shows the example once had; a generated secret, whose example holds a
/// placeholder, draws one only when `env` lacks it.
pub fn drift(env: &str, example: &str, history: &str) -> Vec<String> {
    let ours = settings(env);
    let earlier = settings(history);
    settings(example)
        .into_iter()
        .filter_map(
            |(name, value)| match ours.iter().find(|(setting, _)| *setting == name) {
                None if has_placeholder(value) => Some(format!(
                    "warning: .env lacks {name}, a secret that `just setup` generates for a new \
                     .env: give it a random value of your own"
                )),
                Some(_) if has_placeholder(value) => None,
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
/// without their `-` and `+`: none where git or the history isn't there. Colors are turned off,
/// whatever git's configuration asks, so no escape code comes before a line.
fn history(root: &Path) -> String {
    Command::new("git")
        .args([
            "log",
            "--no-color",
            "--format=",
            "--unified=0",
            "--patch",
            "--",
            EXAMPLE,
        ])
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
    fn ops_20_a_env_written_from_the_example_draws_no_warning() {
        let env = EXAMPLE
            .replace("__HMAC_KEY__", "c2VjcmV0")
            .replace("__APP_PASSWORD__", "0f3a");

        assert_eq!(drift(&env, EXAMPLE, HISTORY), Vec::<String>::new());
    }

    #[test]
    fn ops_20_a_missing_setting_and_an_earlier_default_draw_warnings_without_the_env_s_value() {
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
    fn ops_20_a_warning_never_holds_the_env_s_value() {
        let example = "LOG_LEVEL=info\nHMAC_KEY=__HMAC_KEY__\n";
        let history = "LOG_LEVEL=trace\nLOG_LEVEL=info\n";
        let env = "LOG_LEVEL=trace\n";

        let warnings = drift(env, example, history);

        assert_eq!(warnings.len(), 2, "{warnings:#?}");
        assert!(warnings[0].starts_with("warning: .env sets LOG_LEVEL to an earlier value"));
        assert!(warnings[1].starts_with("warning: .env lacks HMAC_KEY, a secret"));
        assert!(warnings.iter().all(|warning| !warning.contains("trace")));
    }

    #[test]
    #[expect(
        clippy::disallowed_methods,
        reason = "the test writes a repository of its own, outside any async runtime"
    )]
    fn ops_20_git_s_history_of_the_example_holds_its_earlier_values() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/env-drift")
            .join(std::process::id().to_string());
        if root.exists() {
            std::fs::remove_dir_all(&root).unwrap();
        }
        std::fs::create_dir_all(&root).unwrap();
        let git = |args: &[&str]| {
            let status = Command::new("git")
                .args([
                    "-c",
                    "user.name=env-drift",
                    "-c",
                    "user.email=env-drift@localhost",
                ])
                .args(args)
                .current_dir(&root)
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "--quiet"]);
        for value in ["", "http://127.0.0.1:8080"] {
            std::fs::write(
                root.join(super::EXAMPLE),
                format!("# The origins.\nCORS_ALLOWED_ORIGINS={value}\n"),
            )
            .unwrap();
            git(&["add", super::EXAMPLE]);
            git(&[
                "commit",
                "--quiet",
                "--no-gpg-sign",
                "--message=The example",
            ]);
        }

        let history = history(&root);
        std::fs::remove_dir_all(&root).unwrap();

        assert_eq!(
            settings(&history),
            [
                ("CORS_ALLOWED_ORIGINS", ""),
                ("CORS_ALLOWED_ORIGINS", "http://127.0.0.1:8080"),
                ("CORS_ALLOWED_ORIGINS", ""),
            ]
        );
    }

    #[test]
    fn ops_20_a_value_the_developer_chose_draws_no_warning() {
        let env = "ENVIRONMENT=development\n\
                   CORS_ALLOWED_ORIGINS=http://localhost:3000\n\
                   STREAM_HEARTBEAT_SECONDS=16\n\
                   LOG_LEVEL=debug\n\
                   HMAC_KEY=c2VjcmV0\n\
                   DATABASE_URL=postgres://app:0f3a@127.0.0.1:5432/api\n";

        assert_eq!(drift(env, EXAMPLE, HISTORY), Vec::<String>::new());
    }
}
