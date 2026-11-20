//! The modules' settings, which `serve` reads before its async runtime starts and hands to the
//! composition root (OPS-01).

use std::{ffi::OsString, io, path::Path, time::Duration};

use platform::{
    config::{ConfigError, Settings},
    rate_limit::Bucket,
};
// part: client_telemetry
use platform::telemetry::OtlpEndpoint;
// end part: client_telemetry

use crate::modules::{audit, auth};
// part: client_telemetry
use crate::modules::client_telemetry;
// end part: client_telemetry
// part: files
use crate::modules::files;
// end part: files
// part: invitations
use crate::modules::invitations;
// end part: invitations

/// The settings of the modules, which `serve` reads before its async runtime starts (OPS-01).
pub struct Config {
    pub(super) audit: audit::Settings,
    pub(super) auth: auth::Settings,
    /// The OAuth providers that the deployment configures (AUTH-081).
    pub(super) providers: auth::Providers,
    // part: client_telemetry
    pub(super) client_telemetry: TelemetryProxy,
    // end part: client_telemetry
    // part: files
    pub(super) files: files::Settings,
    // end part: files
    // part: invitations
    pub(super) invitations: invitations::Settings,
    // end part: invitations
    pub(super) heartbeat: Duration,
}

/// The name of the bucket of the sign-in limit of an email from one client address (AUTH-006).
const SIGN_IN_BUCKET: &str = "sign_in";

/// The name of the bucket of the sign-in limit of an email from every address (AUTH-006).
const SIGN_IN_EMAIL_BUCKET: &str = "sign_in_email";

/// The name of the bucket of the sign-in limit of a device token (AUTH-006).
const SIGN_IN_DEVICE_BUCKET: &str = "sign_in_device";

// part: client_telemetry
/// The telemetry proxy's settings: what the composition root builds the proxy from.
pub(super) struct TelemetryProxy {
    /// The collector's receiver of clients' batches, to which the relay forwards them, or `None`
    /// while the deployment forwards none (CLIENT_TELEMETRY-010).
    pub(super) endpoint: Option<OtlpEndpoint>,
    /// The two budgets of what the proxy forwards.
    pub(super) budgets: client_telemetry::Settings,
}

/// The setting that names the collector's receiver of clients' batches, and what it must be
/// (CLIENT_TELEMETRY-010).
const TELEMETRY_ENDPOINT: &str = "CLIENT_TELEMETRY_ENDPOINT";
const TELEMETRY_ENDPOINT_EXPECTED: &str =
    "an http or https URL with a host, and no user, password, query or fragment";

/// The name of the budget of the batches of telemetry that one user's clients send
/// (CLIENT_TELEMETRY-007).
const TELEMETRY_USER_BUDGET: &str = "telemetry_user";

/// The name of the budget of the batches of telemetry that one replica forwards
/// (CLIENT_TELEMETRY-008).
const TELEMETRY_REPLICA_BUDGET: &str = "telemetry_replica";

/// The window of both budgets.
const TELEMETRY_WINDOW: Duration = Duration::from_secs(60);
// end part: client_telemetry

// part: invitations
/// The name of the per-member limit's bucket on inviting and sending again (INVITATIONS-009).
const INVITATION_BUCKET: &str = "invitation";
// end part: invitations

// part: client_telemetry
/// The collector's receiver that the telemetry proxy forwards to, from `settings`, if the
/// deployment names one (CLIENT_TELEMETRY-010).
fn client_telemetry_endpoint<E, F>(
    settings: &Settings<E, F>,
) -> Result<Option<OtlpEndpoint>, ConfigError>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> io::Result<String>,
{
    settings
        .text_setting(TELEMETRY_ENDPOINT)?
        .map(|endpoint| {
            endpoint
                .parse()
                .map_err(|()| ConfigError::invalid(TELEMETRY_ENDPOINT, TELEMETRY_ENDPOINT_EXPECTED))
        })
        .transpose()
}

/// The telemetry proxy's two budgets, from `settings`, each at least the bytes of the largest
/// batch, so that a window holds any batch (CLIENT_TELEMETRY-007, CLIENT_TELEMETRY-008). A
/// user's is the largest batch unless a setting says otherwise, and a replica's four times that,
/// so that no one user's batches spend it (the client_telemetry spec's "The budgets").
fn client_telemetry_budgets<E, F>(
    settings: &Settings<E, F>,
) -> Result<client_telemetry::Settings, ConfigError>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> io::Result<String>,
{
    let least = client_telemetry::MAX_BATCH_BYTES;
    let budget = |name: &'static str, setting: &str, default: u32| {
        Ok(Bucket {
            name,
            limit: settings.number(setting, default, least..=1 << 30)?,
            window: TELEMETRY_WINDOW,
        })
    };
    Ok(client_telemetry::Settings {
        user_budget: budget(
            TELEMETRY_USER_BUDGET,
            "CLIENT_TELEMETRY_USER_BYTES_PER_MINUTE",
            least,
        )?,
        replica_budget: budget(
            TELEMETRY_REPLICA_BUDGET,
            "CLIENT_TELEMETRY_REPLICA_BYTES_PER_MINUTE",
            4 * least,
        )?,
    })
}
// end part: client_telemetry

impl Config {
    /// The settings that `seed` builds the API with: these, but with `SECOND_FACTOR_REQUIRED`
    /// off, whatever the deployment's, so its accounts, which have no authenticator app, create
    /// their workspaces through the routes (SYSTEM-007, D138).
    #[must_use]
    pub fn for_seed(mut self) -> Self {
        self.auth.second_factor_required = false;
        self
    }

    /// The modules' settings from `settings`.
    pub fn read<E, F>(settings: &Settings<E, F>) -> Result<Self, ConfigError>
    where
        E: Fn(&str) -> Option<OsString>,
        F: Fn(&Path) -> io::Result<String>,
    {
        let day: u32 = 86_400;
        let sign_in_attempts = settings.number("SIGN_IN_ATTEMPTS", 5, 1..=1_000)?;
        let sign_in_window = settings.duration("SIGN_IN_WINDOW_SECONDS", 900, 1..=day)?;
        let refresh = settings.duration("REFRESH_TOKEN_SECONDS", 30 * day, 3_600..=365 * day)?;
        Ok(Self {
            audit: audit::Settings {
                retention_days: settings.number(
                    "AUDIT_RETENTION_DAYS",
                    365,
                    audit::RETENTION_FLOOR_DAYS..=3_650,
                )?,
            },
            providers: auth::Providers::from_settings(settings)?,
            auth: auth::Settings {
                lifetimes: auth::Lifetimes {
                    // No access token outlives its refresh token (AUTH-007).
                    access: settings.duration_at_most(
                        "ACCESS_TOKEN_SECONDS",
                        3_600,
                        60..=day,
                        ("REFRESH_TOKEN_SECONDS", refresh),
                    )?,
                    refresh,
                    absolute: settings.duration(
                        "SESSION_LIFETIME_SECONDS",
                        90 * day,
                        3_600..=365 * day,
                    )?,
                },
                sign_in: Bucket {
                    name: SIGN_IN_BUCKET,
                    limit: sign_in_attempts,
                    window: sign_in_window,
                },
                sign_in_email: Bucket {
                    name: SIGN_IN_EMAIL_BUCKET,
                    limit: settings.number("SIGN_IN_EMAIL_ATTEMPTS", 50, 1..=100_000)?,
                    window: sign_in_window,
                },
                sign_in_device: Bucket {
                    name: SIGN_IN_DEVICE_BUCKET,
                    limit: sign_in_attempts,
                    window: sign_in_window,
                },
                device_tokens: settings.duration(
                    "DEVICE_TOKEN_SECONDS",
                    365 * day,
                    day..=5 * 365 * day,
                )?,
                second_factor_required: settings.flag("SECOND_FACTOR_REQUIRED", false)?,
            },
            // part: client_telemetry
            client_telemetry: TelemetryProxy {
                endpoint: client_telemetry_endpoint(settings)?,
                budgets: client_telemetry_budgets(settings)?,
            },
            // end part: client_telemetry
            // part: files
            files: files::Settings {
                max_bytes: usize::try_from(settings.number(
                    "FILE_MAX_BYTES",
                    10_485_760,
                    1..=files::LARGEST_UPLOAD_BYTES,
                )?)
                .unwrap_or(usize::MAX),
                deletion_delay_days: settings.number("FILE_DELETION_DELAY_DAYS", 35, 1..=3_657)?,
            },
            // end part: files
            // part: invitations
            invitations: invitations::Settings {
                expiry_days: settings.number("INVITATION_EXPIRY_DAYS", 7, 1..=90)?,
                limit: Bucket {
                    name: INVITATION_BUCKET,
                    limit: settings.number("INVITATION_LIMIT", 100, 1..=100_000)?,
                    window: settings.duration("INVITATION_WINDOW_SECONDS", 3_600, 1..=day)?,
                },
            },
            // end part: invitations
            heartbeat: settings.duration("STREAM_HEARTBEAT_SECONDS", 15, 5..=300)?,
        })
    }
}
