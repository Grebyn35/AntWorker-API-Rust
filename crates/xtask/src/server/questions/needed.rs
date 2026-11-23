//! The secret files and the certificates that `server-setup` asks for, each with the choices of
//! the settings that need it.

use super::get;

/// A file of secrets/ or certs/ that the settings' choices may need.
pub(super) struct File {
    pub(super) name: &'static str,
    /// What it holds, as the question shows it.
    pub(super) what: &'static str,
    /// Whether the server generates it when the operator gives none.
    pub(super) generated: bool,
    /// Whether the settings need it.
    pub(super) needed: fn(&[(String, String)]) -> bool,
}

fn every(_: &[(String, String)]) -> bool {
    true
}

fn bundled(settings: &[(String, String)]) -> bool {
    matches!(get(settings, "DATABASE"), "bundled" | "")
}

fn managed_database(settings: &[(String, String)]) -> bool {
    get(settings, "DATABASE") == "managed"
}

// part: files
fn always(_: &[(String, String)]) -> bool {
    true
}
// end part: files

fn managed_telemetry(settings: &[(String, String)]) -> bool {
    get(settings, "TELEMETRY") == "managed"
}

fn self_hosted(settings: &[(String, String)]) -> bool {
    matches!(get(settings, "TELEMETRY"), "self-hosted" | "")
}

fn managed_grafana(settings: &[(String, String)]) -> bool {
    managed_telemetry(settings) && !get(settings, "MANAGED_GRAFANA_URL").is_empty()
}

fn smtp_credentials(settings: &[(String, String)]) -> bool {
    get(settings, "EMAIL") == "smtp" && !get(settings, "SMTP_USERNAME").is_empty()
}

// part: github
fn github_client(settings: &[(String, String)]) -> bool {
    !get(settings, "GITHUB_CLIENT_ID").is_empty()
}
// end part: github

// part: google
fn google_client(settings: &[(String, String)]) -> bool {
    !get(settings, "GOOGLE_CLIENT_ID").is_empty()
}

fn google_desktop_client(settings: &[(String, String)]) -> bool {
    !get(settings, "GOOGLE_DESKTOP_CLIENT_ID").is_empty()
}
// end part: google

// part: openid_connect
fn openid_connect_client(settings: &[(String, String)]) -> bool {
    !get(settings, "OPENID_CONNECT_CLIENT_ID").is_empty()
}
// end part: openid_connect

/// The secrets that the operator holds, as docs/operations/deploy.md lists them; the server
/// generates those it can, for the operator to keep, unless the operator gives a lost server's.
pub(super) const SECRETS: &[File] = &[
    File {
        name: "encryption_keys",
        what: "The key ring that encrypts the secrets the database holds, such as authenticator \
               apps'. Enter generates one on the server, the first time, which the setup shows \
               once; type a lost server's to restore its backups, or a ring with a new key to \
               replace one (docs/operations/deploy.md).",
        generated: true,
        needed: every,
    },
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
    // part: files
    File {
        name: "files_s3_key",
        what: "The access key of the files bucket's key pair, which reaches only that bucket.",
        generated: false,
        needed: always,
    },
    File {
        name: "files_s3_key_secret",
        what: "The secret key of the files bucket's key pair.",
        generated: false,
        needed: always,
    },
    // end part: files
    File {
        name: "backup_s3_key",
        what: "The access key of the backup bucket's key pair, which reaches only that bucket.",
        generated: false,
        needed: bundled,
    },
    File {
        name: "backup_s3_key_secret",
        what: "The secret key of the backup bucket's key pair.",
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
        what: "The bundled database's superuser password. Enter generates one on the server, \
               the first time, which the setup shows once; type a lost server's to restore its \
               backups, whose database keeps it (docs/operations/backups.md).",
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
    File {
        name: "grafana_admin_password",
        what: "The password that Grafana's administrator signs in with, as GRAFANA_ADMIN_USER: \
               15 characters or more, which nothing else uses. Grafana reads it the first time \
               it starts, and never again.",
        generated: false,
        needed: self_hosted,
    },
    File {
        name: "smtp_password",
        what: "The password the mail server signs the API in with, as SMTP_USERNAME.",
        generated: false,
        needed: smtp_credentials,
    },
    // part: github
    File {
        name: "github_client_secret",
        what: "The client secret of the GitHub OAuth app whose ID is GITHUB_CLIENT_ID.",
        generated: false,
        needed: github_client,
    },
    // end part: github
    // part: google
    File {
        name: "google_client_secret",
        what: "The client secret of Google's client for browsers, whose ID is GOOGLE_CLIENT_ID.",
        generated: false,
        needed: google_client,
    },
    File {
        name: "google_desktop_client_secret",
        what: "The client secret of Google's client for desktop apps, whose ID is \
               GOOGLE_DESKTOP_CLIENT_ID.",
        generated: false,
        needed: google_desktop_client,
    },
    // end part: google
    // part: openid_connect
    File {
        name: "openid_connect_client_secret",
        what: "The client secret of the client, at the OpenID Connect provider that the \
               settings name, whose ID is OPENID_CONNECT_CLIENT_ID.",
        generated: false,
        needed: openid_connect_client,
    },
    // end part: openid_connect
];

/// The certificates of private authorities that a deployment may trust.
pub(super) const CERTS: &[File] = &[
    // part: files
    File {
        name: "files-ca.crt",
        what: "The certificate of the private authority that signed the files bucket's \
               certificate, for a bucket of your own; none for a provider's.",
        generated: false,
        needed: always,
    },
    // end part: files
    File {
        name: "database-ca.crt",
        what: "The certificate of the private authority that signed the managed database's \
               certificate, if one did; the database's URLs then name it, as \
               sslrootcert=/etc/deployment-certs/database-ca.crt.",
        generated: false,
        needed: managed_database,
    },
    File {
        name: "backup-ca.crt",
        what: "The certificate of the private authority that signed the backup bucket's \
               certificate, for a bucket of your own; none for a provider's.",
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

#[cfg(test)]
mod tests {
    use super::{CERTS, SECRETS};
    use crate::{deploy::root, server::read};

    /// The names that deploy/server-setup.sh's `write` takes for an item of the kind `kind`,
    /// `secret` or `cert`. It reads the script's text from the line `write() {`: the line
    /// `<kind>)` after it, then the `case $first in` on the next line, up to the `esac` that ends
    /// it; of that case, each pattern of an arm on one line that ends with `) ;;`, an arm that
    /// takes the name, split at each `|`. A nested case and its arms are skipped, and a pattern is
    /// compared as the text it is, so a name that only a wildcard takes is none of these.
    fn taken(kind: &str) -> Vec<String> {
        let script = String::from_utf8(
            read(&root().join("deploy/server-setup.sh")).expect("deploy/server-setup.sh"),
        )
        .expect("UTF-8");
        let mut lines = script
            .lines()
            .skip_while(|line| *line != "write() {")
            .map(str::trim)
            .skip_while(|line| line.strip_suffix(')') != Some(kind));
        assert!(lines.next().is_some(), "no {kind}) in write()");
        assert_eq!(lines.next(), Some("case $first in"), "the {kind}) arm");
        let mut names = Vec::new();
        let mut depth = 1;
        for line in lines {
            if line.starts_with("case ") {
                depth += 1;
            } else if line == "esac" {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            } else if depth == 1
                && let Some(patterns) = line.strip_suffix(") ;;")
            {
                names.extend(patterns.split('|').map(|name| name.trim().to_owned()));
            }
        }
        assert_eq!(depth, 0, "the {kind}) arm's case has no esac");
        assert!(!names.is_empty(), "the {kind}) arm takes no name");
        names
    }

    /// Each secret and certificate that the questions can ask for is one that the server's
    /// script writes, as `taken` reads the script.
    #[test]
    fn ops_19_the_server_s_script_writes_each_secret_and_certificate_the_setup_asks_for() {
        let secrets = taken("secret");
        let certs = taken("cert");

        for file in SECRETS {
            assert!(
                secrets.iter().any(|name| name == file.name),
                "server-setup.sh's write takes no secret named {}: {secrets:?}",
                file.name
            );
        }
        for file in CERTS {
            assert!(
                certs.iter().any(|name| name == file.name),
                "server-setup.sh's write takes no certificate named {}: {certs:?}",
                file.name
            );
        }
    }
}
