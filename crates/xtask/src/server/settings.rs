//! The deployment's settings, as deploy/settings.env.example describes them and a settings file
//! holds them: the groups of settings that the questions follow, and the file the server gets.

/// A group of settings that deploy/settings.env.example describes together: its comment, without
/// the `# `, and the settings with their example values.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Block {
    pub(super) comment: Vec<String>,
    pub(super) settings: Vec<(String, String)>,
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
    pub(super) fn condition(&self) -> Option<(String, String)> {
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
    pub(super) fn choices(&self) -> Option<Vec<String>> {
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
    pub(super) fn required(&self) -> bool {
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
pub(super) fn blocks(example: &str) -> Vec<Block> {
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
pub(super) fn settings_of(text: &str) -> Vec<(String, String)> {
    let mut settings: Vec<(String, String)> = Vec::new();
    for (name, value) in text.lines().filter_map(setting_line) {
        set(&mut settings, name, value);
    }
    settings
}

pub(super) fn get<'a>(settings: &'a [(String, String)], name: &str) -> &'a str {
    settings
        .iter()
        .find(|(existing, _)| existing == name)
        .map_or("", |(_, value)| value)
}

pub(super) fn set(settings: &mut Vec<(String, String)>, name: &str, value: &str) {
    match settings.iter_mut().find(|(existing, _)| existing == name) {
        Some(setting) => value.clone_into(&mut setting.1),
        None => settings.push((name.to_owned(), value.to_owned())),
    }
}

/// The settings file that the deployment's server holds: deploy/settings.env.example with the
/// values of `settings`, then each setting the server's file held that the example doesn't list.
pub(super) fn settings_file(
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

/// Where Alertmanager sees the deployment's secrets/alertmanager/ (deploy/compose.yaml).
const ALERTMANAGER_SECRETS: &str = "/etc/alertmanager-secrets/";

/// The files of secrets/alertmanager/ that Alertmanager's configuration `config` reads, through
/// an integration's option for a file, such as `api_url_file` of `slack_configs` (D49), in its
/// order, each once. A comment names none.
pub(super) fn alertmanager_secrets(config: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for line in config.lines() {
        let line = line.trim_start();
        if line.starts_with('#') {
            continue;
        }
        let Some((option, value)) = line.trim_start_matches("- ").split_once(':') else {
            continue;
        };
        let Some(name) = value.trim().strip_prefix(ALERTMANAGER_SECRETS) else {
            continue;
        };
        if option.ends_with("_file") && !name.is_empty() && !names.iter().any(|known| known == name)
        {
            names.push(name.to_owned());
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        deploy::root,
        server::{example, read},
    };

    /// The block of `blocks` that holds the setting `name`.
    fn block_of<'a>(blocks: &'a [Block], name: &str) -> &'a Block {
        blocks
            .iter()
            .find(|block| block.settings.iter().any(|(setting, _)| setting == name))
            .expect("a block for the setting")
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

    #[test]
    fn ops_22_the_alertmanager_secrets_are_the_files_its_integrations_read() {
        let config = "# - api_url_file: /etc/alertmanager-secrets/commented\n\
                      receivers:\n  - name: page\n    slack_configs:\n\
                      \x20     - api_url_file: /etc/alertmanager-secrets/slack_api_url\n\
                      \x20   webhook_configs:\n      - url_file: /etc/alertmanager-secrets/page_url\n\
                      \x20 - name: ticket\n    webhook_configs:\n\
                      \x20     - url_file: /etc/alertmanager-secrets/page_url\n";

        assert_eq!(
            alertmanager_secrets(config),
            vec!["slack_api_url".to_owned(), "page_url".to_owned()]
        );
        assert!(
            alertmanager_secrets(
                &String::from_utf8(
                    read(&root().join("deploy/observability/alertmanager/alertmanager.yaml"))
                        .expect("the file")
                )
                .expect("UTF-8")
            )
            .is_empty(),
            "the template names no integration"
        );
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
}
