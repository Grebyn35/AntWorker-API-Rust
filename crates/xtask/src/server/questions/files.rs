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

fn managed_grafana(settings: &[(String, String)]) -> bool {
    managed_telemetry(settings) && !get(settings, "MANAGED_GRAFANA_URL").is_empty()
}

fn smtp_credentials(settings: &[(String, String)]) -> bool {
    get(settings, "EMAIL") == "smtp" && !get(settings, "SMTP_USERNAME").is_empty()
}

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
        name: "smtp_password",
        what: "The password the mail server signs the API in with, as SMTP_USERNAME.",
        generated: false,
        needed: smtp_credentials,
    },
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
