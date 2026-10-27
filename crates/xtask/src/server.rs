//! `server-setup` and `server-check`, which the `just` recipes of the same names run on a
//! developer's machine (OPS-22). Both send deploy/ from the checkout's HEAD to a temporary
//! directory on the server over SSH. `server-setup` asks for the settings and the secrets that
//! the settings' choices need, then runs deploy/server-setup.sh, which installs Docker, prepares
//! the deployment's user, and writes what was asked. `server-check` runs deploy.sh's check of a
//! release on the server, then checks from here what only the outside sees: where each of the
//! deployment's addresses points, and whether the server's ports 80 and 443 can be reached.
//! docs/operations/deploy.md describes both.
//!
//! The questions, the settings file they make, and what the server is sent are pure functions of
//! deploy/settings.env.example, the server's state and the answers, so tests cover them without a
//! server.

use std::{
    io::{self, BufRead, IsTerminal, Write},
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    path::Path,
    process::{Command, ExitCode, Stdio},
    time::Duration,
};

use crate::{
    deploy::{archive, quoted, refuse, root},
    release,
};

/// The deployment's directory on the server, in the home of the user SSH signs in as.
const DEPLOYMENT: &str = "$HOME/deployment";

/// How long a connection to one of the server's ports may take before the check counts the port
/// as unreachable.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// A group of settings that deploy/settings.env.example describes together: its comment, without
/// the `# `, and the settings with their example values.
#[derive(Debug, PartialEq, Eq)]
struct Block {
    comment: Vec<String>,
    settings: Vec<(String, String)>,
}

impl Block {
    /// The text of the comment's `Type:` sentence and what follows it, on one line.
    fn kind(&self) -> Option<String> {
        let start = self
            .comment
            .iter()
            .position(|line| line.starts_with("Type: "))?;
        Some(self.comment[start..].join(" "))
    }

    /// The setting and the value with which the block's settings apply, which its `Type:` line
    /// names as `with NAME=value`, such as `Required with TELEMETRY=managed`.
    fn condition(&self) -> Option<(String, String)> {
        let kind = self.kind()?;
        kind.split("with ").skip(1).find_map(|rest| {
            let word: String = rest
                .chars()
                .take_while(|c| !matches!(c, ',' | '.' | ' '))
                .collect();
            let (name, value) = word.split_once('=')?;
            (!name.is_empty() && name.chars().all(|c| c.is_ascii_uppercase() || c == '_'))
                .then(|| (name.to_owned(), value.to_owned()))
        })
    }

    /// The values that the block's setting may take, when its `Type:` line lists them, such as
    /// `Type: bundled or managed.`
    fn choices(&self) -> Option<Vec<String>> {
        let kind = self.kind()?;
        let listed = kind.strip_prefix("Type: ")?.split('.').next()?;
        let (rest, last) = listed.rsplit_once(" or ")?;
        let choices: Vec<String> = rest.split(", ").chain([last]).map(str::to_owned).collect();
        choices
            .iter()
            .all(|choice| {
                !choice.is_empty() && choice.chars().all(|c| c.is_ascii_lowercase() || c == '-')
            })
            .then_some(choices)
    }

    /// Whether its `Type:` line says its settings are required.
    fn required(&self) -> bool {
        self.kind().is_some_and(|kind| kind.contains("Required"))
    }
}

/// The setting that `line` of a settings file sets, and its value.
fn setting_line(line: &str) -> Option<(&str, &str)> {
    let (name, value) = line.split_once('=')?;
    (!name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
    .then_some((name, value))
}

/// The groups of settings of deploy/settings.env.example, in its order. A comment that no
/// setting follows, such as the file's own, starts no group.
fn blocks(example: &str) -> Vec<Block> {
    let mut blocks = Vec::new();
    let mut comment = Vec::new();
    let mut settings = Vec::new();
    for line in example.lines() {
        if let Some((name, value)) = setting_line(line) {
            settings.push((name.to_owned(), value.to_owned()));
            continue;
        }
        if !settings.is_empty() {
            blocks.push(Block {
                comment: std::mem::take(&mut comment),
                settings: std::mem::take(&mut settings),
            });
        }
        match line.strip_prefix('#') {
            Some(text) => comment.push(text.strip_prefix(' ').unwrap_or(text).to_owned()),
            None => comment.clear(),
        }
    }
    if !settings.is_empty() {
        blocks.push(Block { comment, settings });
    }
    blocks
}

/// The settings that the file `text` sets, in its order; a later line sets a setting again, as
/// deploy.sh reads it.
fn settings_of(text: &str) -> Vec<(String, String)> {
    let mut settings: Vec<(String, String)> = Vec::new();
    for (name, value) in text.lines().filter_map(setting_line) {
        set(&mut settings, name, value);
    }
    settings
}

fn get<'a>(settings: &'a [(String, String)], name: &str) -> &'a str {
    settings
        .iter()
        .find(|(existing, _)| existing == name)
        .map_or("", |(_, value)| value)
}

fn set(settings: &mut Vec<(String, String)>, name: &str, value: &str) {
    match settings.iter_mut().find(|(existing, _)| existing == name) {
        Some(setting) => value.clone_into(&mut setting.1),
        None => settings.push((name.to_owned(), value.to_owned())),
    }
}

/// The settings file that the deployment's server holds: deploy/settings.env.example with the
/// values of `settings`, then each setting the server's file held that the example doesn't list.
fn settings_file(
    example: &str,
    settings: &[(String, String)],
    held: &[(String, String)],
) -> String {
    let mut text = String::new();
    for line in example.lines() {
        match setting_line(line) {
            Some((name, _)) => text.push_str(&format!("{name}={}\n", get(settings, name))),
            None => {
                text.push_str(line);
                text.push('\n');
            }
        }
    }
    let listed = settings_of(example);
    let extra: Vec<&(String, String)> = held
        .iter()
        .filter(|(name, _)| listed.iter().all(|(listed, _)| listed != name))
        .collect();
    if !extra.is_empty() {
        text.push_str(
            "\n# Settings this deployment held that deploy/settings.env.example doesn't list.\n",
        );
        for (name, value) in extra {
            text.push_str(&format!("{name}={value}\n"));
        }
    }
    text
}

/// What the server holds, as deploy/server-setup.sh's `state` writes it.
#[derive(Debug, Default, PartialEq, Eq)]
struct State {
    /// The user that runs the deployment.
    user: String,
    /// Its settings file, if it has one.
    settings: Option<String>,
    /// The secret files it holds.
    secrets: Vec<String>,
    /// The certificates it holds.
    certs: Vec<String>,
}

impl State {
    fn parse(text: &str) -> Result<Self, String> {
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

/// A file of secrets/ or certs/ that the settings' choices may need.
struct File {
    name: &'static str,
    /// What it holds, as the question shows it.
    what: &'static str,
    /// Whether the server generates it when the operator gives none.
    generated: bool,
    /// Whether the settings need it.
    needed: fn(&[(String, String)]) -> bool,
}

fn bundled(settings: &[(String, String)]) -> bool {
    matches!(get(settings, "DATABASE"), "bundled" | "")
}

fn managed_database(settings: &[(String, String)]) -> bool {
    get(settings, "DATABASE") == "managed"
}

fn managed_telemetry(settings: &[(String, String)]) -> bool {
    get(settings, "TELEMETRY") == "managed"
}

fn managed_grafana(settings: &[(String, String)]) -> bool {
    managed_telemetry(settings) && !get(settings, "MANAGED_GRAFANA_URL").is_empty()
}

/// The secrets that the operator holds, as docs/operations/deploy.md lists them.
const SECRETS: &[File] = &[
    File {
        name: "database_admin_url",
        what: "The managed database's URL for its administrator, as .env.example describes \
               DATABASE_ADMIN_URL, with sslmode=verify-full.",
        generated: false,
        needed: managed_database,
    },
    File {
        name: "database_owner_url",
        what: "The managed database's URL for the owner of the schema, as .env.example describes \
               DATABASE_OWNER_URL, with sslmode=verify-full.",
        generated: false,
        needed: managed_database,
    },
    File {
        name: "database_url",
        what: "The managed database's URL for the API, as .env.example describes DATABASE_URL, \
               with sslmode=verify-full.",
        generated: false,
        needed: managed_database,
    },
    File {
        name: "backup_s3_key",
        what: "The access key of the bucket's key pair, which reaches only that bucket.",
        generated: false,
        needed: bundled,
    },
    File {
        name: "backup_s3_key_secret",
        what: "The secret key of the bucket's key pair.",
        generated: false,
        needed: bundled,
    },
    File {
        name: "backup_cipher_pass",
        what: "The passphrase that encrypts the backups. Enter generates one on the server, the \
               first time, which the setup shows once; type a lost server's to restore its \
               backups (docs/operations/backups.md).",
        generated: true,
        needed: bundled,
    },
    File {
        name: "postgres_password",
        what: "The bundled database's superuser password. Enter leaves it to the first deploy, \
               which generates it; type a lost server's to restore its backups, whose database \
               keeps it (docs/operations/backups.md).",
        generated: true,
        needed: bundled,
    },
    File {
        name: "managed_otlp_token",
        what: "The token that the collector signs in to the managed backend with, as \
               MANAGED_OTLP_USER.",
        generated: false,
        needed: managed_telemetry,
    },
    File {
        name: "managed_grafana_token",
        what: "The token of a service account of the managed Grafana with the Admin role, which \
               imports the dashboards and the alert rules.",
        generated: false,
        needed: managed_grafana,
    },
];

/// The certificates of private authorities that a deployment may trust.
const CERTS: &[File] = &[
    File {
        name: "backup-ca.crt",
        what: "The certificate of the private authority that signed the bucket's certificate, \
               for a bucket of your own; none for a provider's.",
        generated: false,
        needed: bundled,
    },
    File {
        name: "managed-otlp-ca.crt",
        what: "The certificate of the private authority that signed the managed backend's \
               certificate, if one did.",
        generated: false,
        needed: managed_telemetry,
    },
    File {
        name: "managed-grafana-ca.crt",
        what: "The certificate of the private authority that signed the managed Grafana's \
               certificate, if one did.",
        generated: false,
        needed: managed_grafana,
    },
];

/// Where the questions go and the answers come from.
trait Prompt {
    /// Shows `text`.
    fn show(&mut self, text: &str);
    /// Asks `question`, and returns the answer, trimmed.
    fn ask(&mut self, question: &str) -> Result<String, String>;
    /// Asks `question` for a secret, showing nothing of what is typed, and returns the answer,
    /// trimmed.
    fn ask_secret(&mut self, question: &str) -> Result<String, String>;
}

/// The terminal: questions on stdout and answers from stdin, and a secret typed unseen. Without
/// a terminal, such as when a script answers, each answer is a line of stdin.
struct Terminal;

impl Prompt for Terminal {
    #[expect(clippy::print_stdout, reason = "the questions are the task's output")]
    fn show(&mut self, text: &str) {
        println!("{text}");
    }

    #[expect(clippy::print_stdout, reason = "the questions are the task's output")]
    fn ask(&mut self, question: &str) -> Result<String, String> {
        print!("{question}");
        io::stdout().flush().map_err(|error| error.to_string())?;
        let mut answer = String::new();
        let read = io::stdin()
            .lock()
            .read_line(&mut answer)
            .map_err(|error| format!("can't read the answer: {error}"))?;
        if read == 0 {
            return Err("the answers ended before the questions".to_owned());
        }
        Ok(answer.trim().to_owned())
    }

    fn ask_secret(&mut self, question: &str) -> Result<String, String> {
        if !io::stdin().is_terminal() {
            return self.ask(question);
        }
        rpassword::prompt_password(question)
            .map(|answer| answer.trim().to_owned())
            .map_err(|error| format!("can't read the answer: {error}"))
    }
}

/// A login to the registry that holds the release's images.
#[derive(Debug, PartialEq, Eq)]
struct Login {
    registry: String,
    user: String,
    token: String,
}

/// What the answers set up: the settings file, and the secrets, certificates and login to write.
#[derive(Debug, PartialEq, Eq)]
struct Setup {
    settings: String,
    secrets: Vec<(String, String)>,
    certs: Vec<(String, Vec<u8>)>,
    login: Option<Login>,
}

impl Setup {
    /// What deploy/server-setup.sh's `apply` reads, as its comment describes it.
    fn payload(&self) -> String {
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
fn github_defaults(remote: &str) -> Option<(String, String)> {
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
fn ask(
    example: &str,
    state: &State,
    defaults: &[(String, String)],
    prompt: &mut impl Prompt,
    read_file: impl Fn(&str) -> Result<Vec<u8>, String>,
) -> Result<Setup, String> {
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
        secrets: ask_secrets(&settings, state, prompt)?,
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
/// it, and leaves to the server to generate when it can.
fn ask_secrets(
    settings: &[(String, String)],
    state: &State,
    prompt: &mut impl Prompt,
) -> Result<Vec<(String, String)>, String> {
    let mut secrets = Vec::new();
    for secret in SECRETS.iter().filter(|secret| (secret.needed)(settings)) {
        let held = state.secrets.iter().any(|name| name == secret.name);
        prompt.show("");
        prompt.show(secret.what);
        loop {
            let question = if held {
                format!("{} (Enter keeps the server's): ", secret.name)
            } else if secret.generated {
                format!("{} (Enter generates one): ", secret.name)
            } else {
                format!("{}: ", secret.name)
            };
            let answer = prompt.ask_secret(&question)?;
            if !answer.is_empty() {
                secrets.push((secret.name.to_owned(), answer));
                break;
            }
            if held || secret.generated {
                break;
            }
            prompt.show(&format!("{} is required.", secret.name));
        }
    }
    Ok(secrets)
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

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// `bytes` in base64, as `base64 -d` on the server reads it.
fn encode(bytes: &[u8]) -> String {
    let mut text = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = chunk
            .iter()
            .enumerate()
            .fold(0_u32, |group, (index, &byte)| {
                group | u32::from(byte) << (16 - 8 * index)
            });
        for index in 0..4 {
            if index <= chunk.len() {
                let sextet = (group >> (18 - 6 * index)) & 0x3f;
                text.push(char::from(BASE64[sextet as usize]));
            } else {
                text.push('=');
            }
        }
    }
    text
}

/// The bytes that the base64 `text` holds, as `base64` on the server writes it.
fn decode(text: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::with_capacity(text.len() / 4 * 3);
    let mut group = 0_u32;
    let mut count = 0;
    for c in text.trim_end_matches('=').bytes() {
        let sextet = BASE64
            .iter()
            .position(|&symbol| symbol == c)
            .ok_or_else(|| format!("{text:?} isn't base64"))?;
        group = group << 6 | u32::try_from(sextet).map_err(|error| error.to_string())?;
        count += 1;
        if count == 4 {
            bytes.extend_from_slice(&group.to_be_bytes()[1..]);
            group = 0;
            count = 0;
        }
    }
    // The bits of a last group of two or three symbols that come after its last whole byte are
    // zero.
    match count {
        0 => {}
        2 => bytes.push((group >> 4).to_be_bytes()[3]),
        3 => bytes.extend_from_slice(&(group >> 2).to_be_bytes()[2..]),
        _ => return Err(format!("{text:?} isn't base64")),
    }
    Ok(bytes)
}

/// Copies the files of deploy/ in the checkout's HEAD to a new temporary directory on `host`,
/// and returns its path there.
fn send_checkout(root: &Path, host: &str) -> Result<String, String> {
    let mut archive = archive(root, "HEAD:deploy")
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("can't run git archive: {error}"))?;
    let stdout = archive.stdout.take().ok_or("git archive has no output")?;
    let unpacked = Command::new("ssh")
        .arg(host)
        .arg(
            r#"set -e; directory=$(mktemp -d); tar -x -C "$directory"; printf '%s\n' "$directory""#,
        )
        .stdin(Stdio::from(stdout))
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("can't run ssh: {error}"))?;
    let archived = archive
        .wait()
        .map_err(|error| format!("git archive failed: {error}"))?;
    if !archived.success() {
        return Err("git archive failed".to_owned());
    }
    if !unpacked.status.success() {
        return Err(format!(
            "unpacking them on the server failed ({})",
            unpacked.status
        ));
    }
    let directory = String::from_utf8_lossy(&unpacked.stdout).trim().to_owned();
    if !directory.starts_with('/') {
        return Err(format!("the server named no directory, but {directory:?}"));
    }
    Ok(directory)
}

/// Runs `command` in the shell of `host`, and returns what it wrote on stdout; its stderr goes to
/// this process's.
fn on_server_output(host: &str, command: &str) -> Result<String, String> {
    let output = Command::new("ssh")
        .arg(host)
        .arg(command)
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| format!("can't run ssh: {error}"))?;
    if !output.status.success() {
        return Err(format!("{command} failed on {host} ({})", output.status));
    }
    String::from_utf8(output.stdout).map_err(|_| format!("{command} wrote what isn't UTF-8"))
}

/// The settings `IMAGE_REPOSITORY` and `SOURCE` for the repository that the checkout fetches from,
/// on GitHub, or none.
fn defaults(root: &Path) -> Vec<(String, String)> {
    let remote = Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(root)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned());
    match remote.as_deref().and_then(github_defaults) {
        Some((repository, source)) => vec![
            ("IMAGE_REPOSITORY".to_owned(), repository),
            ("SOURCE".to_owned(), source),
        ],
        None => Vec::new(),
    }
}

/// Sets up the server that SSH reaches at `host`, as root or a user that may run sudo without a
/// password, or changes its deployment's settings and secrets.
pub fn setup(host: &str) -> ExitCode {
    let root = root();
    let example = match Command::new("git")
        .args(["show", "HEAD:deploy/settings.env.example"])
        .current_dir(&root)
        .output()
    {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).into_owned()
        }
        _ => return refuse("Can't read deploy/settings.env.example at HEAD."),
    };
    let state = send_checkout(&root, host)
        .and_then(|directory| {
            on_server_output(
                host,
                &format!("sh {}/server-setup.sh state", quoted(&directory)),
            )
        })
        .and_then(|text| State::parse(&text));
    let state = match state {
        Ok(state) => state,
        Err(error) => return refuse(&format!("Can't read what {host} holds: {error}")),
    };
    let mut terminal = Terminal;
    terminal.show(&format!(
        "Setting up {host}, whose deployment runs as {}. Enter keeps the value in brackets.",
        state.user
    ));
    let setup = match ask(&example, &state, &defaults(&root), &mut terminal, |path| {
        read(Path::new(path)).map_err(|error| format!("Can't read {path}: {error}"))
    }) {
        Ok(setup) => setup,
        Err(error) => return refuse(&error),
    };
    let directory = match send_checkout(&root, host) {
        Ok(directory) => directory,
        Err(error) => return refuse(&format!("Can't copy deploy/ to {host}: {error}")),
    };
    let mut ssh = match Command::new("ssh")
        .arg(host)
        .arg(format!("sh {}/server-setup.sh apply", quoted(&directory)))
        .stdin(Stdio::piped())
        .spawn()
    {
        Ok(ssh) => ssh,
        Err(error) => return refuse(&format!("Can't run ssh: {error}")),
    };
    let sent = ssh
        .stdin
        .take()
        .map(|mut input| input.write_all(setup.payload().as_bytes()));
    match (sent, ssh.wait()) {
        (Some(Ok(())), Ok(status)) if status.success() => ExitCode::SUCCESS,
        (_, Ok(status)) if !status.success() => ExitCode::FAILURE,
        _ => refuse(&format!("Can't send the setup to {host}.")),
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "the task runs as a synchronous program, with no async runtime to stall"
)]
fn read(path: &Path) -> io::Result<Vec<u8>> {
    std::fs::read(path)
}

/// A line of the check's report, in the form deploy.sh's check writes.
#[derive(Debug, PartialEq, Eq)]
enum Finding {
    Passed(String),
    Failed(String),
}

impl Finding {
    fn line(&self) -> String {
        match self {
            Self::Passed(text) => format!("check: ok    {text}"),
            Self::Failed(text) => format!("check: FAIL  {text}"),
        }
    }
}

/// Whether the deployment's address `name`, which DNS resolves to `resolved`, points to the
/// server, which SSH reaches at `server`.
fn address_finding(name: &str, resolved: &[IpAddr], server: &[IpAddr]) -> Finding {
    let shown = |addresses: &[IpAddr]| {
        addresses
            .iter()
            .map(IpAddr::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    if resolved.is_empty() {
        return Finding::Failed(format!(
            "{name} resolves to no address; its DNS records must point to the server, {}, before \
             Let's Encrypt can issue its certificate",
            shown(server)
        ));
    }
    if resolved.iter().any(|address| server.contains(address)) {
        Finding::Passed(format!("{name} points to the server, {}", shown(resolved)))
    } else {
        Finding::Failed(format!(
            "{name} points to {}, not to the server, {}; Let's Encrypt can't issue its \
             certificate unless the address reaches the server",
            shown(resolved),
            shown(server)
        ))
    }
}

/// Whether the server's port `port` answered at `address`: a connection, or its refusal, comes
/// from the server, while no answer at all means a firewall drops the port, or the server is
/// down.
fn port_finding(address: SocketAddr, connected: &io::Result<TcpStream>) -> Finding {
    match connected {
        Ok(_) => Finding::Passed(format!("port {} of the server answers", address.port())),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => Finding::Passed(format!(
            "port {} of the server can be reached; nothing listens there until the first deploy \
             starts Caddy",
            address.port()
        )),
        Err(error) => Finding::Failed(format!(
            "port {} of the server, at {address}, doesn't answer ({error}): a firewall must \
             admit ports 80 and 443 from anywhere (docs/operations/deploy.md#what-a-server-needs)",
            address.port()
        )),
    }
}

/// The addresses that `name` resolves to.
fn resolve(name: &str) -> Vec<IpAddr> {
    let mut addresses: Vec<IpAddr> = (name, 443)
        .to_socket_addrs()
        .map(|resolved| resolved.map(|address| address.ip()).collect())
        .unwrap_or_default();
    addresses.sort();
    addresses.dedup();
    addresses
}

/// The address that SSH reaches `host` at, as `ssh -G` names it.
fn server_name(host: &str) -> Result<String, String> {
    let output = Command::new("ssh")
        .args(["-G", host])
        .output()
        .map_err(|error| format!("can't run ssh: {error}"))?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("hostname ").map(str::to_owned))
        .ok_or_else(|| format!("ssh -G names no address for {host}"))
}

/// Checks what a deploy of the release `version`, or of the latest release, to the server that
/// SSH reaches at `host` needs, and changes nothing.
#[expect(clippy::print_stdout, reason = "the findings are the task's output")]
pub fn check(host: &str, version: Option<&str>) -> ExitCode {
    let root = root();
    let version = match version {
        Some(version) => match version.parse::<release::Version>() {
            Ok(version) => version,
            Err(_) => return refuse(&format!("{version} isn't a version, X.Y.Z or X.Y.Z-rc.N.")),
        },
        None => match release::latest_release(&root) {
            Ok(Some(version)) => version,
            Ok(None) => {
                return refuse(
                    "No release exists yet, and the check checks what a deploy of a release \
                     needs. `just release` makes one: docs/operations/release.md says how.",
                );
            }
            Err(error) => return refuse(&error),
        },
    };
    println!("Checking what a deploy of {version} to {host} needs.");
    let directory = match send_checkout(&root, host) {
        Ok(directory) => directory,
        Err(error) => return refuse(&format!("Can't copy deploy/ to {host}: {error}")),
    };
    let directory = quoted(&directory);
    let on_server = Command::new("ssh")
        .arg(host)
        .arg(format!(
            "DEPLOYMENT={DEPLOYMENT} sh {directory}/deploy.sh check {}; status=$?; rm -rf {directory}; exit $status",
            quoted(&version.to_string())
        ))
        .status();
    let mut failed = !matches!(on_server, Ok(status) if status.success());

    let settings = on_server_output(host, &format!("cat {DEPLOYMENT}/settings.env 2> /dev/null"))
        .map(|text| settings_of(&text))
        .unwrap_or_default();
    let server = match server_name(host) {
        Ok(name) => resolve(&name),
        Err(error) => {
            println!("{}", Finding::Failed(error).line());
            return ExitCode::FAILURE;
        }
    };
    let mut findings = Vec::new();
    let mut addresses = vec![get(&settings, "API_ADDRESS")];
    if matches!(get(&settings, "TELEMETRY"), "self-hosted" | "") {
        addresses.push(get(&settings, "GRAFANA_ADDRESS"));
    }
    for name in addresses.into_iter().filter(|name| !name.is_empty()) {
        findings.push(address_finding(name, &resolve(name), &server));
    }
    if let Some(&ip) = server.first() {
        for port in [80, 443] {
            let address = SocketAddr::new(ip, port);
            findings.push(port_finding(
                address,
                &TcpStream::connect_timeout(&address, CONNECT_TIMEOUT),
            ));
        }
    }
    for finding in &findings {
        println!("{}", finding.line());
        failed |= matches!(finding, Finding::Failed(_));
    }
    if failed {
        println!("Some checks failed; each says what a deploy needs.");
        ExitCode::FAILURE
    } else {
        println!("Every check passed: deploy {version} with just deploy {host} {version}.");
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn example() -> String {
        String::from_utf8(read(&root().join("deploy/settings.env.example")).expect("the example"))
            .expect("UTF-8")
    }

    /// The block of `blocks` that holds the setting `name`.
    fn block_of<'a>(blocks: &'a [Block], name: &str) -> &'a Block {
        blocks
            .iter()
            .find(|block| block.settings.iter().any(|(setting, _)| setting == name))
            .expect("a block for the setting")
    }

    fn no_file(path: &str) -> Result<Vec<u8>, String> {
        Err(format!("no file {path}"))
    }

    #[test]
    fn ops_22_the_example_s_choices_and_conditions_are_read_from_its_type_lines() {
        let blocks = blocks(&example());
        let block = |name: &str| block_of(&blocks, name);

        assert_eq!(
            block("DATABASE").choices(),
            Some(vec!["bundled".to_owned(), "managed".to_owned()])
        );
        assert_eq!(
            block("TELEMETRY").choices(),
            Some(vec![
                "self-hosted".to_owned(),
                "managed".to_owned(),
                "off".to_owned()
            ])
        );
        assert_eq!(block("API_ADDRESS").choices(), None);
        assert_eq!(
            block("GRAFANA_ADDRESS").condition(),
            Some(("TELEMETRY".to_owned(), "self-hosted".to_owned()))
        );
        assert_eq!(
            block("BACKUP_S3_BUCKET").condition(),
            Some(("DATABASE".to_owned(), "bundled".to_owned()))
        );
        assert_eq!(
            block("MANAGED_GRAFANA_URL").condition(),
            Some(("TELEMETRY".to_owned(), "managed".to_owned()))
        );
        assert!(block("API_ADDRESS").required());
        assert!(!block("API_REPLICAS").required());
    }

    #[test]
    fn ops_22_each_choice_comes_before_the_settings_that_depend_on_it() {
        let blocks = blocks(&example());
        let position = |name: &str| {
            blocks
                .iter()
                .position(|block| block.settings.iter().any(|(setting, _)| setting == name))
                .expect("a block for the setting")
        };

        for block in &blocks {
            if let Some((choice, _)) = block.condition() {
                let (first, _) = &block.settings[0];
                assert!(
                    position(&choice) < position(first),
                    "{choice} comes after {first}, which depends on it"
                );
            }
        }
    }

    /// The answers of a new server's setup with the bundled database and the self-hosted stack.
    const NEW_SERVER: &[&str] = &[
        // IMAGE_REPOSITORY, SOURCE, API_ADDRESS, then Enter for the rest of the settings,
        // until the bucket.
        "",
        "",
        "api.example.com",
        "",
        "",
        "",
        "",
        "",
        "",
        "",
        "grafana.example.com",
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
        // The bucket's key pair, then Enter leaves the passphrase and the superuser's password
        // to be generated.
        "key",
        "secret",
        "",
        "",
        // No private authority, then the registry's token and its user.
        "",
        "ghp_token",
        "",
    ];

    #[test]
    fn ops_22_a_new_server_is_asked_for_what_the_bundled_database_and_its_stack_need() {
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

        let setup = ask(&example(), &state, &defaults, &mut script, no_file).unwrap();

        let settings = settings_of(&setup.settings);
        assert_eq!(get(&settings, "IMAGE_REPOSITORY"), "ghcr.io/owner/name");
        assert_eq!(get(&settings, "SOURCE"), "https://github.com/Owner/Name");
        assert_eq!(get(&settings, "API_ADDRESS"), "api.example.com");
        assert_eq!(get(&settings, "GRAFANA_ADDRESS"), "grafana.example.com");
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
    fn ops_22_a_server_that_was_set_up_keeps_what_each_empty_answer_leaves() {
        let held = "IMAGE_REPOSITORY=ghcr.io/owner/name\nSOURCE=https://github.com/owner/name\n\
                    API_ADDRESS=api.example.com\nDATABASE=managed\nTELEMETRY=off\n\
                    FORK_SETTING=kept\n";
        let state = State {
            user: "deploy".to_owned(),
            settings: Some(held.to_owned()),
            secrets: vec![
                "database_admin_url".to_owned(),
                "database_owner_url".to_owned(),
                "database_url".to_owned(),
            ],
            certs: Vec::new(),
        };
        let questions = blocks(&example())
            .iter()
            .filter(|block| {
                block
                    .condition()
                    .is_none_or(|(name, value)| get(&settings_of(held), &name) == value)
            })
            .map(|block| block.settings.len())
            .sum::<usize>();
        // An empty answer to every setting, every secret, and the registry's token.
        let answers = vec![""; questions + 3 + 1];
        let mut script = Script::new(&answers);

        let setup = ask(&example(), &state, &[], &mut script, no_file).unwrap();

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
    fn ops_22_a_choice_that_isn_t_listed_and_an_empty_required_setting_are_asked_again() {
        let state = State {
            user: "deploy".to_owned(),
            ..State::default()
        };
        let mut script = Script::new(&["", "", "", "api.example.com", "", "", "", "", "postgres"]);

        let asked = ask(&example(), &state, &[], &mut script, no_file);

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
    fn ops_22_the_server_is_sent_each_value_in_base64_on_a_line_of_its_own() {
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
    fn ops_22_base64_matches_the_standard_s_examples() {
        for (bytes, text) in [
            (&b""[..], ""),
            (b"f", "Zg=="),
            (b"fo", "Zm8="),
            (b"foo", "Zm9v"),
            (b"foob", "Zm9vYg=="),
            (b"fooba", "Zm9vYmE="),
            (b"foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(encode(bytes), text);
            assert_eq!(decode(text).unwrap(), bytes);
        }
        assert!(decode("Zm9v!").is_err());
    }

    #[test]
    fn ops_22_the_server_s_settings_file_keeps_the_example_s_text_and_the_settings_it_lacks() {
        let example = "# The address.\nAPI_ADDRESS=\nAPI_REPLICAS=2\n";
        let settings = vec![
            ("API_ADDRESS".to_owned(), "api.example.com".to_owned()),
            ("API_REPLICAS".to_owned(), "3".to_owned()),
        ];
        let held = vec![("FORK_SETTING".to_owned(), "kept".to_owned())];

        let text = settings_file(example, &settings, &held);

        assert!(
            text.starts_with("# The address.\nAPI_ADDRESS=api.example.com\nAPI_REPLICAS=3\n"),
            "{text}"
        );
        assert!(text.ends_with("FORK_SETTING=kept\n"), "{text}");
    }

    #[test]
    fn ops_22_the_registry_and_the_defaults_come_from_the_repository_s_names() {
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

    #[test]
    fn ops_22_an_address_that_points_elsewhere_or_nowhere_fails_the_check() {
        let server: Vec<IpAddr> = vec!["203.0.113.7".parse().unwrap()];
        let elsewhere: Vec<IpAddr> = vec!["198.51.100.1".parse().unwrap()];

        assert!(matches!(
            address_finding("api.example.com", &server, &server),
            Finding::Passed(_)
        ));
        assert!(matches!(
            address_finding("api.example.com", &elsewhere, &server),
            Finding::Failed(text) if text.contains("198.51.100.1")
        ));
        assert!(matches!(
            address_finding("api.example.com", &[], &server),
            Finding::Failed(text) if text.contains("resolves to no address")
        ));
    }

    #[test]
    fn ops_22_a_port_that_answers_or_refuses_passes_and_one_that_doesn_t_answer_fails() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let open = listener.local_addr().unwrap();
        let address = SocketAddr::from(([127, 0, 0, 1], 443));
        let refused = Err(io::Error::from(io::ErrorKind::ConnectionRefused));
        let dropped = Err(io::Error::from(io::ErrorKind::TimedOut));

        assert!(matches!(
            port_finding(address, &refused),
            Finding::Passed(text) if text.contains("nothing listens")
        ));
        assert!(matches!(
            port_finding(address, &dropped),
            Finding::Failed(text) if text.contains("firewall")
        ));
        assert!(matches!(
            port_finding(open, &TcpStream::connect_timeout(&open, CONNECT_TIMEOUT)),
            Finding::Passed(_)
        ));
    }
}
