//! The modules' settings, which `serve` reads before its async runtime starts and hands to the
//! composition root (OPS-01).

use std::{ffi::OsString, io, path::Path, time::Duration};

use platform::{
    config::{ConfigError, Settings},
    rate_limit::Bucket,
};

use crate::modules::auth;
// part: files
use crate::modules::files;
// end part: files
// part: invitations
use crate::modules::invitations;
// end part: invitations

/// The settings of the modules, which `serve` reads before its async runtime starts (OPS-01).
pub struct Config {
    pub(super) auth: auth::Settings,
    /// The OAuth providers that the deployment configures (AUTH-081).
    pub(super) providers: auth::Providers,
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

// part: invitations
/// The name of the per-member limit's bucket on inviting (INVITATIONS-009).
const INVITATION_BUCKET: &str = "invitation";
// end part: invitations

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
            // part: files
            files: files::Settings {
                max_bytes: usize::try_from(settings.number(
                    "FILE_MAX_BYTES",
                    10_485_760,
                    1..=20_971_520,
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
