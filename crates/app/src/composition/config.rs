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

/// The sign-in limits of an email from one client address, of an email from every address, and
/// of a device token, from `settings`, in that order. Startup refuses a `SIGN_IN_ATTEMPTS` above
/// half `SIGN_IN_EMAIL_ATTEMPTS`, so that no one address fills the count of every address
/// (AUTH-006).
fn sign_in_limits<E, F>(settings: &Settings<E, F>) -> Result<[Bucket; 3], ConfigError>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> io::Result<String>,
{
    let attempts = settings.number("SIGN_IN_ATTEMPTS", 5, 1..=1_000)?;
    let email_attempts = settings.number("SIGN_IN_EMAIL_ATTEMPTS", 50, 1..=100_000)?;
    if attempts.saturating_mul(2) > email_attempts {
        return Err(ConfigError::invalid(
            "SIGN_IN_ATTEMPTS",
            "at most half SIGN_IN_EMAIL_ATTEMPTS",
        ));
    }
    let window = settings.duration("SIGN_IN_WINDOW_SECONDS", 900, 1..=86_400)?;
    let bucket = |name: &'static str, limit: u32| Bucket {
        name,
        limit,
        window,
    };
    Ok([
        bucket(SIGN_IN_BUCKET, attempts),
        bucket(SIGN_IN_EMAIL_BUCKET, email_attempts),
        bucket(SIGN_IN_DEVICE_BUCKET, attempts),
    ])
}

/// Whether every account must have a second factor, from `SECOND_FACTOR_REQUIRED`, `true` or
/// `false`, and `false` when it is unset (AUTH-070).
fn second_factor_required<E, F>(settings: &Settings<E, F>) -> Result<bool, ConfigError>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> io::Result<String>,
{
    let name = "SECOND_FACTOR_REQUIRED";
    settings.text_setting(name)?.map_or(Ok(false), |value| {
        value
            .parse()
            .map_err(|_| ConfigError::invalid(name, "true or false"))
    })
}

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
/// user's is the largest batch unless a setting says otherwise, and a replica's four times a
/// user's, whatever that is set to, and the most that a budget takes at most, so that no one
/// user's batches spend it unless a setting says otherwise: its own, or a user's of 536,870,912,
/// half the most, or more (the client_telemetry spec's "The budgets").
fn client_telemetry_budgets<E, F>(
    settings: &Settings<E, F>,
) -> Result<client_telemetry::Settings, ConfigError>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> io::Result<String>,
{
    let (least, most) = (client_telemetry::MAX_BATCH_BYTES, 1 << 30);
    let budget = |name: &'static str, setting: &str, default: u32| {
        Ok(Bucket {
            name,
            limit: settings.number(setting, default, least..=most)?,
            window: TELEMETRY_WINDOW,
        })
    };
    let user_budget = budget(
        TELEMETRY_USER_BUDGET,
        "CLIENT_TELEMETRY_USER_BYTES_PER_MINUTE",
        least,
    )?;
    let replica_budget = budget(
        TELEMETRY_REPLICA_BUDGET,
        "CLIENT_TELEMETRY_REPLICA_BYTES_PER_MINUTE",
        user_budget.limit.saturating_mul(4).min(most),
    )?;
    Ok(client_telemetry::Settings {
        user_budget,
        replica_budget,
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
        let [sign_in, sign_in_email, sign_in_device] = sign_in_limits(settings)?;
        let refresh = settings.duration("REFRESH_TOKEN_SECONDS", 30 * day, 3_600..=365 * day)?;
        let access = settings.duration("ACCESS_TOKEN_SECONDS", 3_600, 60..=day)?;
        // No access token outlives its refresh token (AUTH-007).
        if access > refresh {
            return Err(ConfigError::invalid(
                "ACCESS_TOKEN_SECONDS",
                "at most REFRESH_TOKEN_SECONDS",
            ));
        }
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
                    access,
                    refresh,
                    absolute: settings.duration(
                        "SESSION_LIFETIME_SECONDS",
                        90 * day,
                        3_600..=365 * day,
                    )?,
                },
                sign_in,
                sign_in_email,
                sign_in_device,
                device_tokens: settings.duration(
                    "DEVICE_TOKEN_SECONDS",
                    365 * day,
                    day..=5 * 365 * day,
                )?,
                second_factor_required: second_factor_required(settings)?,
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
// part: client_telemetry

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_telemetry_008_without_its_setting_a_replica_s_budget_is_1_073_741_824_at_most() {
        // A user's budget whose fourfold is 4 bytes beyond the most.
        let user = (1_u32 << 28) + 1;
        let env = |name: &str| {
            (name == "CLIENT_TELEMETRY_USER_BYTES_PER_MINUTE")
                .then(|| OsString::from(user.to_string()))
        };
        let settings = Settings::new(env, |_: &Path| -> io::Result<String> {
            Err(io::ErrorKind::NotFound.into())
        });

        let budgets = client_telemetry_budgets(&settings).unwrap();

        assert_eq!(budgets.user_budget.limit, user);
        assert_eq!(budgets.replica_budget.limit, 1_073_741_824);
    }
}
// end part: client_telemetry
