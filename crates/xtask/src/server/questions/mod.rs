//! The questions of `server-setup`: each setting, secret and certificate that the settings'
//! choices need, and the registry's login, asked on the developer's machine before anything on
//! the server changes, and what the answers send the server.

mod files;

use files::{CERTS, SECRETS};

use super::{
    base64::{decode, encode},
    settings::{Block, alertmanager_secrets, blocks, get, set, settings_file, settings_of},
};

/// The files of the release that the questions follow.
pub(super) struct Release<'a> {
    /// deploy/settings.env.example.
    pub(super) settings: &'a str,
    /// Alertmanager's configuration, whose integrations name their credentials.
    pub(super) alertmanager: &'a str,
}

/// What the server holds, as deploy/server-setup.sh's `state` writes it.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct State {
    /// The user that runs the deployment.
    pub(super) user: String,
    /// Its settings file, if it has one.
    settings: Option<String>,
    /// The secret files it holds.
    secrets: Vec<String>,
    /// The certificates it holds.
    certs: Vec<String>,
}

impl State {
    pub(super) fn parse(text: &str) -> Result<Self, String> {
        let mut state = Self::default();
        for line in text.lines() {
            let (kind, value) = line.split_once(' ').unwrap_or((line, ""));
            match kind {
                "user" => value.clone_into(&mut state.user),
                "settings" => {
                    let bytes = decode(value)?;
                    state.settings =
                        Some(String::from_utf8(bytes).map_err(|_| "settings.env isn't UTF-8")?);
                }
                "secret" => state.secrets.push(value.to_owned()),
                "cert" => state.certs.push(value.to_owned()),
                _ => return Err(format!("server-setup.sh wrote an unknown line: {line}")),
            }
        }
        if state.user.is_empty() {
            return Err("server-setup.sh named no user".to_owned());
        }
        Ok(state)
    }
}

/// Where the questions go and the answers come from.
pub(super) trait Prompt {
    /// Shows `text`.
    fn show(&mut self, text: &str);
    /// Asks `question`, and returns the answer, trimmed.
    fn ask(&mut self, question: &str) -> Result<String, String>;
    /// Asks `question` for a secret, showing nothing of what is typed, and returns the answer,
    /// trimmed.
    fn ask_secret(&mut self, question: &str) -> Result<String, String>;
}

/// A login to the registry that holds the release's images.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Login {
    registry: String,
    user: String,
    token: String,
}

/// What the answers set up: the settings file, and the secrets, certificates and login to write.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Setup {
    settings: String,
    secrets: Vec<(String, String)>,
    certs: Vec<(String, Vec<u8>)>,
    login: Option<Login>,
}

impl Setup {
    /// What deploy/server-setup.sh's `apply` reads, as its comment describes it.
    pub(super) fn payload(&self) -> String {
        let mut payload = format!("settings {}\n", encode(self.settings.as_bytes()));
        for (name, value) in &self.secrets {
            payload.push_str(&format!("secret {name} {}\n", encode(value.as_bytes())));
        }
        for (name, contents) in &self.certs {
            payload.push_str(&format!("cert {name} {}\n", encode(contents)));
        }
        if let Some(login) = &self.login {
            payload.push_str(&format!(
                "login {} {} {}\n",
                login.registry,
                login.user,
                encode(login.token.as_bytes())
            ));
        }
        payload
    }
}

/// The registry that holds the images of `repository`, such as ghcr.io for
/// ghcr.io/owner/name, or Docker Hub for a name without one.
fn registry_of(repository: &str) -> &str {
    match repository.split_once('/') {
        Some((first, _)) if first.contains(['.', ':']) || first == "localhost" => first,
        _ => "docker.io",
    }
}

/// The settings `IMAGE_REPOSITORY` and `SOURCE` for the GitHub repository at `remote`, the
/// address git fetches it from, such as git@github.com:Owner/Name.git.
pub(super) fn github_defaults(remote: &str) -> Option<(String, String)> {
    let path = remote
        .strip_prefix("https://github.com/")
        .or_else(|| remote.strip_prefix("git@github.com:"))
        .or_else(|| remote.strip_prefix("ssh://git@github.com/"))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    if owner.is_empty() || name.is_empty() || name.contains('/') {
        return None;
    }
    Some((
        format!("ghcr.io/{}", path.to_lowercase()),
        format!("https://github.com/{owner}/{name}"),
    ))
}

/// Asks for each setting, secret and certificate that the settings' choices need, showing what
/// the server holds, which an empty answer keeps, and for the registry's login. `defaults` are
/// the values of settings that the server doesn't hold yet, in place of the example's;
/// `read_file` reads a certificate from this machine.
pub(super) fn ask(
    release: &Release,
    state: &State,
    defaults: &[(String, String)],
    prompt: &mut impl Prompt,
    read_file: impl Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<Setup, String> {
    let example = release.settings;
    let held = state
        .settings
        .as_deref()
        .map(settings_of)
        .unwrap_or_default();
    let mut settings = settings_of(example);
    for (name, value) in defaults.iter().chain(&held) {
        set(&mut settings, name, value);
    }
    for block in blocks(example) {
        if let Some((name, value)) = block.condition()
            && get(&settings, &name) != value
        {
            continue;
        }
        prompt.show("");
        for line in &block.comment {
            prompt.show(line);
        }
        for (name, _) in &block.settings {
            let value = ask_setting(&block, name, get(&settings, name), prompt)?;
            set(&mut settings, name, &value);
        }
    }
    Ok(Setup {
        secrets: ask_secrets(&settings, release.alertmanager, state, prompt)?,
        certs: ask_certs(&settings, state, prompt, read_file)?,
        login: ask_login(get(&settings, "IMAGE_REPOSITORY"), prompt)?,
        settings: settings_file(example, &settings, &held),
    })
}

/// Asks for the setting `name` of `block`, whose value is `current`, until the answer is one the
/// block's `Type:` line allows.
fn ask_setting(
    block: &Block,
    name: &str,
    current: &str,
    prompt: &mut impl Prompt,
) -> Result<String, String> {
    loop {
        let question = if current.is_empty() {
            format!("{name}: ")
        } else {
            format!("{name} [{current}]: ")
        };
        let answer = prompt.ask(&question)?;
        let value = if answer.is_empty() {
            current.to_owned()
        } else {
            answer
        };
        if let Some(choices) = block.choices()
            && !choices.contains(&value)
        {
            prompt.show(&format!("{name} is one of: {}", choices.join(", ")));
            continue;
        }
        if value.is_empty() && block.required() {
            prompt.show(&format!("{name} is required."));
            continue;
        }
        return Ok(value);
    }
}

/// Asks for each secret that `settings` need, which an empty answer keeps when the server holds
/// it, and leaves to the server to generate when it can; and, with the self-hosted stack, for each
/// credential that Alertmanager's configuration `alertmanager` reads from secrets/alertmanager/
/// (D49).
fn ask_secrets(
    settings: &[(String, String)],
    alertmanager: &str,
    state: &State,
    prompt: &mut impl Prompt,
) -> Result<Vec<(String, String)>, String> {
    let held = |name: &str| state.secrets.iter().any(|secret| secret == name);
    let mut secrets = Vec::new();
    for secret in SECRETS.iter().filter(|secret| (secret.needed)(settings)) {
        prompt.show("");
        prompt.show(secret.what);
        secrets.extend(ask_secret(
            secret.name,
            held(secret.name),
            secret.generated,
            prompt,
        )?);
    }
    if get(settings, "TELEMETRY") == "self-hosted" {
        for file in alertmanager_secrets(alertmanager) {
            let name = format!("alertmanager/{file}");
            prompt.show("");
            prompt.show(&format!(
                "A credential of an alert integration, which Alertmanager reads as \
                 /etc/alertmanager-secrets/{file}."
            ));
            secrets.extend(ask_secret(&name, held(&name), false, prompt)?);
        }
    }
    Ok(secrets)
}

/// Asks for the secret `name`, unseen, until an answer gives it, or until an empty one keeps the
/// server's, when it `held` one, or leaves it to the server, when it is `generated`.
fn ask_secret(
    name: &str,
    held: bool,
    generated: bool,
    prompt: &mut impl Prompt,
) -> Result<Option<(String, String)>, String> {
    loop {
        let question = if held {
            format!("{name} (Enter keeps the server's): ")
        } else if generated {
            format!("{name} (Enter generates one): ")
        } else {
            format!("{name}: ")
        };
        let answer = prompt.ask_secret(&question)?;
        if !answer.is_empty() {
            return Ok(Some((name.to_owned(), answer)));
        }
        if held || generated {
            return Ok(None);
        }
        prompt.show(&format!("{name} is required."));
    }
}

/// Asks for each certificate of a private authority that `settings` may need, as a file on this
/// machine, which `read_file` reads.
fn ask_certs(
    settings: &[(String, String)],
    state: &State,
    prompt: &mut impl Prompt,
    read_file: impl Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<Vec<(String, Vec<u8>)>, String> {
    let mut certs = Vec::new();
    for cert in CERTS.iter().filter(|cert| (cert.needed)(settings)) {
        let held = state.certs.iter().any(|name| name == cert.name);
        prompt.show("");
        prompt.show(cert.what);
        let keeps = if held {
            "keeps the server's"
        } else {
            "for none"
        };
        let path = prompt.ask(&format!(
            "{}, as a file on this machine (Enter {keeps}): ",
            cert.name
        ))?;
        if !path.is_empty() {
            certs.push((cert.name.to_owned(), read_file(&path)?));
        }
    }
    Ok(certs)
}

/// Asks for the server's login to the registry that holds the images of `repository`: a token,
/// whose empty answer keeps the server's login, and its user.
fn ask_login(repository: &str, prompt: &mut impl Prompt) -> Result<Option<Login>, String> {
    let registry = registry_of(repository).to_owned();
    prompt.show("");
    prompt.show(&format!(
        "The server's login to {registry}, through which it reads the release's images: a \
         token that can read the repository's packages."
    ));
    let token = prompt.ask_secret(
        "The token (Enter keeps the server's login, or makes none, for public images): ",
    )?;
    if token.is_empty() {
        return Ok(None);
    }
    let owner = repository.split('/').nth(1).unwrap_or_default();
    loop {
        let answer = prompt.ask(&format!("Its user [{owner}]: "))?;
        let user = if answer.is_empty() {
            owner.to_owned()
        } else {
            answer
        };
        if !user.is_empty() && !user.contains(char::is_whitespace) {
            return Ok(Some(Login {
                registry,
                user,
                token,
            }));
        }
        prompt.show("The user is one word.");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::example;

    /// The questions it showed and asked, and the answers it gives, in order.
    struct Script {
        answers: Vec<&'static str>,
        asked: Vec<String>,
        shown: Vec<String>,
    }

    impl Script {
        fn new(answers: &[&'static str]) -> Self {
            Self {
                answers: answers.iter().rev().copied().collect(),
                asked: Vec::new(),
                shown: Vec::new(),
            }
        }
    }

    impl Prompt for Script {
        fn show(&mut self, text: &str) {
            self.shown.push(text.to_owned());
        }

        fn ask(&mut self, question: &str) -> Result<String, String> {
            self.asked.push(question.to_owned());
            self.answers
                .pop()
                .map(str::to_owned)
                .ok_or_else(|| format!("no answer left for {question}"))
        }

        fn ask_secret(&mut self, question: &str) -> Result<String, String> {
            self.ask(&format!("(unseen) {question}"))
        }
    }

    /// The release's files, with the settings `settings`, and an Alertmanager that names no
    /// integration, as the template's.
    fn release(settings: &str) -> Release<'_> {
        Release {
            settings,
            alertmanager: "receivers:\n  - name: page\n",
        }
    }

    /// The number of setting questions that settings holding `held` ask.
    fn setting_questions(held: &str) -> usize {
        blocks(&example())
            .iter()
            .filter(|block| {
                block
                    .condition()
                    .is_none_or(|(name, value)| get(&settings_of(held), &name) == value)
            })
            .map(|block| block.settings.len())
            .sum()
    }

    fn no_file(path: &str) -> Result<Vec<u8>, String> {
        Err(format!("no file {path}"))
    }

    /// The answers of a new server's setup with the bundled database and the self-hosted stack.
    /// The setup asks each setting of deploy/settings.env.example that the answers before it
    /// call for, in the file's order, so a setting added there adds an answer here.
    const NEW_SERVER: &[&str] = &[
        // IMAGE_REPOSITORY, SOURCE, API_ADDRESS, then Enter for the rest of the settings,
        // until Grafana's address: TRUSTED_PROXIES, CORS_ALLOWED_ORIGINS, the IP and user limits,
        "",
        "",
        "api.example.com",
        "",
        "",
        "",
        "",
        // part: files
        // the public downloads' limit,
        "",
        // end part: files
        // the replicas', DATABASE, POSTGRES_MAX_CONNECTIONS and TELEMETRY.
        "",
        "",
        "",
        "",
        "",
        "",
        "grafana.example.com",
        // No email, so no question about a mail server.
        "off",
        // part: files
        // The files bucket, then Enter for the uploads' limit, the scanner's CPUs and the
        // deletion's delay.
        "files.example.com",
        "",
        "eu-1",
        "files",
        "",
        "",
        "",
        "",
        // end part: files
        "s3.example.com",
        "",
        "eu-1",
        "backups",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        // Enter generates the key ring.
        "",
        // part: files
        // The files bucket's key pair.
        "files-key",
        "files-secret",
        // end part: files
        // The backup bucket's key pair, then Enter leaves the passphrase and the superuser's
        // password to be generated.
        "key",
        "secret",
        "",
        "",
        // No private authority for the bucket, then the registry's token and its user.
        "",
        // part: files
        // Nor for the files bucket.
        "",
        // end part: files
        "ghp_token",
        "",
    ];

    #[test]
    fn ops_19_a_new_server_is_asked_for_what_the_bundled_database_and_its_stack_need() {
        let state = State {
            user: "deploy".to_owned(),
            ..State::default()
        };
        let defaults = github_defaults("git@github.com:Owner/Name.git")
            .map(|(repository, source)| {
                vec![
                    ("IMAGE_REPOSITORY".to_owned(), repository),
                    ("SOURCE".to_owned(), source),
                ]
            })
            .expect("GitHub's defaults");
        let mut script = Script::new(NEW_SERVER);

        let setup = ask(
            &release(&example()),
            &state,
            &defaults,
            &mut script,
            no_file,
        )
        .unwrap();

        let settings = settings_of(&setup.settings);
        assert_eq!(get(&settings, "IMAGE_REPOSITORY"), "ghcr.io/owner/name");
        assert_eq!(get(&settings, "SOURCE"), "https://github.com/Owner/Name");
        assert_eq!(get(&settings, "API_ADDRESS"), "api.example.com");
        assert_eq!(get(&settings, "GRAFANA_ADDRESS"), "grafana.example.com");
        // part: files
        assert_eq!(get(&settings, "FILES_S3_ENDPOINT"), "files.example.com");
        assert_eq!(get(&settings, "FILES_S3_BUCKET"), "files");
        // end part: files
        assert_eq!(get(&settings, "BACKUP_S3_ENDPOINT"), "s3.example.com");
        assert_eq!(get(&settings, "BACKUP_S3_PORT"), "443");
        assert_eq!(get(&settings, "BACKUP_S3_BUCKET"), "backups");
        assert_eq!(get(&settings, "DATABASE"), "bundled");
        assert!(
            !script
                .asked
                .iter()
                .any(|question| question.starts_with("MANAGED_")),
            "{:?}",
            script.asked
        );
        assert_eq!(
            setup.secrets,
            vec![
                // part: files
                ("files_s3_key".to_owned(), "files-key".to_owned()),
                ("files_s3_key_secret".to_owned(), "files-secret".to_owned()),
                // end part: files
                ("backup_s3_key".to_owned(), "key".to_owned()),
                ("backup_s3_key_secret".to_owned(), "secret".to_owned()),
            ]
        );
        assert_eq!(
            setup.login,
            Some(Login {
                registry: "ghcr.io".to_owned(),
                user: "owner".to_owned(),
                token: "ghp_token".to_owned(),
            })
        );
        assert!(
            script
                .asked
                .iter()
                .any(|question| question.starts_with("(unseen) backup_s3_key_secret")),
            "{:?}",
            script.asked
        );
        assert!(script.answers.is_empty(), "{:?}", script.answers);
    }

    #[test]
    fn ops_19_a_server_that_was_set_up_keeps_what_each_empty_answer_leaves() {
        let held = concat!(
            "IMAGE_REPOSITORY=ghcr.io/owner/name\nSOURCE=https://github.com/owner/name\n",
            "API_ADDRESS=api.example.com\nDATABASE=managed\nTELEMETRY=off\nEMAIL=off\n",
            // part: files
            "FILES_S3_ENDPOINT=s3.example.com\nFILES_S3_REGION=eu-1\nFILES_S3_BUCKET=files\n",
            // end part: files
            "FORK_SETTING=kept\n",
        );
        let state = State {
            user: "deploy".to_owned(),
            settings: Some(held.to_owned()),
            secrets: vec![
                "database_admin_url".to_owned(),
                "database_owner_url".to_owned(),
                "database_url".to_owned(),
                // part: files
                "files_s3_key".to_owned(),
                "files_s3_key_secret".to_owned(),
                // end part: files
            ],
            certs: Vec::new(),
        };
        // An empty answer to every setting, every secret, the key ring among them, and the
        // registry's token.
        let asked = setting_questions(held) + 1 + 3 + 1;
        // part: files
        // The files bucket's key pair and its authority.
        let asked = asked + 2 + 1;
        // end part: files
        let answers = vec![""; asked];
        let mut script = Script::new(&answers);

        let setup = ask(&release(&example()), &state, &[], &mut script, no_file).unwrap();

        let settings = settings_of(&setup.settings);
        assert_eq!(get(&settings, "API_ADDRESS"), "api.example.com");
        assert_eq!(get(&settings, "DATABASE"), "managed");
        assert_eq!(get(&settings, "FORK_SETTING"), "kept");
        assert!(setup.secrets.is_empty(), "{:?}", setup.secrets);
        assert_eq!(setup.login, None);
        assert!(
            script
                .asked
                .iter()
                .any(|question| question == "API_ADDRESS [api.example.com]: "),
            "{:?}",
            script.asked
        );
        assert!(
            !script
                .asked
                .iter()
                .any(|question| question.starts_with("BACKUP_") || question.contains("backup_")),
            "{:?}",
            script.asked
        );
    }

    #[test]
    fn ops_19_email_through_a_mail_server_with_a_user_name_is_asked_for_its_password() {
        let held = concat!(
            "IMAGE_REPOSITORY=ghcr.io/owner/name\nSOURCE=https://github.com/owner/name\n",
            "API_ADDRESS=api.example.com\nDATABASE=managed\nTELEMETRY=off\nEMAIL=smtp\n",
            "SMTP_HOST=smtp.example.com\nSMTP_USERNAME=api\n",
            "EMAIL_FROM=AntWorker <noreply@example.com>\n",
            // part: files
            "FILES_S3_ENDPOINT=s3.example.com\nFILES_S3_REGION=eu-1\nFILES_S3_BUCKET=files\n",
            // end part: files
        );
        let state = State {
            user: "deploy".to_owned(),
            settings: Some(held.to_owned()),
            secrets: vec![
                "database_admin_url".to_owned(),
                "database_owner_url".to_owned(),
                "database_url".to_owned(),
                // part: files
                "files_s3_key".to_owned(),
                "files_s3_key_secret".to_owned(),
                // end part: files
            ],
            certs: Vec::new(),
        };
        // Enter for every setting, the key ring and the database's URLs, the mail server's
        // password, then Enter for the registry's token.
        let asked = setting_questions(held) + 1 + 3;
        // part: files
        // The files bucket's key pair.
        let asked = asked + 2;
        // end part: files
        let mut answers = vec![""; asked];
        answers.push("the password");
        // part: files
        // Enter for the files bucket's authority.
        answers.push("");
        // end part: files
        answers.push("");
        let mut script = Script::new(&answers);

        let setup = ask(&release(&example()), &state, &[], &mut script, no_file).unwrap();

        assert!(
            script
                .asked
                .iter()
                .any(|question| question == "SMTP_HOST [smtp.example.com]: "),
            "{:?}",
            script.asked
        );
        assert_eq!(
            setup.secrets,
            vec![("smtp_password".to_owned(), "the password".to_owned())]
        );
        assert!(script.answers.is_empty(), "{:?}", script.answers);
    }

    #[test]
    fn ops_19_the_self_hosted_stack_is_asked_for_each_credential_its_alert_integrations_read() {
        let held = concat!(
            "IMAGE_REPOSITORY=ghcr.io/owner/name\nSOURCE=https://github.com/owner/name\n",
            "API_ADDRESS=api.example.com\nGRAFANA_ADDRESS=grafana.example.com\n",
            "DATABASE=managed\nTELEMETRY=self-hosted\nEMAIL=off\n",
            // part: files
            "FILES_S3_ENDPOINT=s3.example.com\nFILES_S3_REGION=eu-1\nFILES_S3_BUCKET=files\n",
            // end part: files
        );
        let state = State {
            user: "deploy".to_owned(),
            settings: Some(held.to_owned()),
            secrets: vec![
                "database_admin_url".to_owned(),
                "database_owner_url".to_owned(),
                "database_url".to_owned(),
                // part: files
                "files_s3_key".to_owned(),
                "files_s3_key_secret".to_owned(),
                // end part: files
                "alertmanager/held_url".to_owned(),
            ],
            certs: Vec::new(),
        };
        let alerting = Release {
            settings: &example(),
            alertmanager: "receivers:\n  - name: page\n    webhook_configs:\n\
                           \x20     - url_file: /etc/alertmanager-secrets/new_url\n\
                           \x20     - url_file: /etc/alertmanager-secrets/held_url\n",
        };
        // Enter for every setting, the key ring and the database's URLs, a new credential and Enter
        // for the held one, then Enter for the registry's token.
        let asked = setting_questions(held) + 1 + 3;
        // part: files
        // The files bucket's key pair.
        let asked = asked + 2;
        // end part: files
        let mut answers = vec![""; asked];
        answers.extend(["https://hooks.example.com/page", ""]);
        // part: files
        // Enter for the files bucket's authority.
        answers.push("");
        // end part: files
        answers.push("");
        let mut script = Script::new(&answers);

        let setup = ask(&alerting, &state, &[], &mut script, no_file).unwrap();

        assert_eq!(
            setup.secrets,
            vec![(
                "alertmanager/new_url".to_owned(),
                "https://hooks.example.com/page".to_owned()
            )]
        );
        assert!(
            script.asked.contains(
                &"(unseen) alertmanager/held_url (Enter keeps the server's): ".to_owned()
            ),
            "{:?}",
            script.asked
        );
        assert!(script.answers.is_empty(), "{:?}", script.answers);
    }

    #[test]
    fn ops_19_a_choice_that_isn_t_listed_and_an_empty_required_setting_are_asked_again() {
        let state = State {
            user: "deploy".to_owned(),
            ..State::default()
        };
        // IMAGE_REPOSITORY and SOURCE, an empty API_ADDRESS, then API_ADDRESS again.
        let mut answers = vec!["", "", "", "api.example.com"];
        // TRUSTED_PROXIES, CORS_ALLOWED_ORIGINS, and the IP and user limits.
        answers.extend(["", "", "", ""]);
        // part: files
        // The public downloads' limit.
        answers.push("");
        // end part: files
        // The replicas', then a database that isn't a choice.
        answers.extend(["", "", "", "postgres"]);
        let mut script = Script::new(&answers);

        let asked = ask(&release(&example()), &state, &[], &mut script, no_file);

        assert!(asked.is_err(), "the answers ran out");
        assert!(
            script
                .shown
                .contains(&"API_ADDRESS is required.".to_owned()),
            "{:?}",
            script.shown
        );
        assert!(
            script
                .shown
                .contains(&"DATABASE is one of: bundled, managed".to_owned()),
            "{:?}",
            script.shown
        );
    }

    #[test]
    fn ops_19_the_server_is_sent_each_value_in_base64_on_a_line_of_its_own() {
        let setup = Setup {
            settings: "API_ADDRESS=api.example.com\n".to_owned(),
            secrets: vec![("backup_s3_key".to_owned(), "a b\nc".to_owned())],
            certs: vec![("backup-ca.crt".to_owned(), b"-----BEGIN\n".to_vec())],
            login: Some(Login {
                registry: "ghcr.io".to_owned(),
                user: "owner".to_owned(),
                token: "ghp_token".to_owned(),
            }),
        };

        let payload = setup.payload();

        let lines: Vec<&str> = payload.lines().collect();
        assert_eq!(lines.len(), 4, "{payload}");
        assert_eq!(
            decode(lines[0].strip_prefix("settings ").unwrap()).unwrap(),
            b"API_ADDRESS=api.example.com\n"
        );
        assert_eq!(
            decode(lines[1].strip_prefix("secret backup_s3_key ").unwrap()).unwrap(),
            b"a b\nc"
        );
        assert!(lines[2].starts_with("cert backup-ca.crt "), "{payload}");
        assert!(lines[3].starts_with("login ghcr.io owner "), "{payload}");
    }

    #[test]
    fn ops_19_the_registry_and_the_defaults_come_from_the_repository_s_names() {
        assert_eq!(registry_of("ghcr.io/owner/name"), "ghcr.io");
        assert_eq!(registry_of("registry:5000/image"), "registry:5000");
        assert_eq!(registry_of("owner/name"), "docker.io");
        assert_eq!(
            github_defaults("https://github.com/Owner/Name.git"),
            Some((
                "ghcr.io/owner/name".to_owned(),
                "https://github.com/Owner/Name".to_owned()
            ))
        );
        assert_eq!(github_defaults("https://gitlab.com/owner/name"), None);
    }
}
