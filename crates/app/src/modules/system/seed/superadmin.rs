//! The superadmin's step of the seed (SYSTEM-010): the account that it makes a superadmin sets an
//! authenticator app up, as a client does, and creates a workspace through a superadmin's route.
// part: invitations
//! It then hands the workspace over, as a superadmin does once its next owner has joined.
// end part: invitations

use axum::http::Method;
use platform::errors::Internal;
use serde_json::json;

use super::{Account, Api, PASSWORD, text};
use crate::modules::system::error::SystemError;

/// The operator's making of a superadmin, as the composition root hands it to the seed: given an
/// account's address, it makes the account a superadmin (SYSTEM-010).
pub(crate) type MakeSuperadmin<'a> = &'a dyn Fn(&'static str) -> MakingSuperadmin<'a>;

/// A making of a superadmin that runs.
pub(crate) type MakingSuperadmin<'a> =
    std::pin::Pin<Box<dyn Future<Output = Result<(), Internal>> + 'a>>;

/// The seconds of a step of an authenticator app's codes (AUTH-057).
const APP_STEP_SECONDS: u64 = 30;

impl Api {
    /// Sets up an authenticator app for `account`, as a client does, and returns its secret, in
    /// base32, whose codes a sign-in to the account then takes (AUTH-055, AUTH-056). The account
    /// re-authenticates first: the answer's `authenticated_at` is the request's time by the
    /// database's clock, by which the API checks an app's code (AUTH-021, AUTH-057), and the
    /// seed reads no clock of its own (CODE-06).
    pub(super) async fn set_up_app(&self, account: &Account) -> Result<String, SystemError> {
        let session = self
            .send(
                Method::POST,
                "/v1/users/me/sessions/current/reauthenticate",
                Some(account),
                Some(json!({ "password": PASSWORD })),
            )
            .await?;
        let now = unix_seconds(&text(&session, "authenticated_at")?)?;
        let setup = self
            .send(
                Method::POST,
                "/v1/users/me/second-factors/totp/setup",
                Some(account),
                None,
            )
            .await?;
        let secret = text(&setup, "secret")?;
        let code = app_code(&secret, now)?;
        self.send(
            Method::POST,
            "/v1/users/me/second-factors/totp/setup/confirm",
            Some(account),
            Some(json!({ "code": code })),
        )
        .await?;
        Ok(secret)
    }

    /// Creates the workspace `name` as `superadmin`, who owns it, and returns its ID
    /// (WORKSPACES-031).
    pub(super) async fn create_as_superadmin(
        &self,
        superadmin: &Account,
        name: &str,
    ) -> Result<String, SystemError> {
        let answer = self
            .send(
                Method::POST,
                "/v1/superadmin/workspaces",
                Some(superadmin),
                Some(json!({ "name": name })),
            )
            .await?;
        text(&answer, "id")
    }

    // part: invitations
    /// Hands `workspace` over from `owner`, who created it, to `next`, as a superadmin hands a
    /// workspace over: `owner` invites `next` as an admin, who accepts, transfers the workspace
    /// to them, and leaves it (WORKSPACES-015, WORKSPACES-014).
    pub(super) async fn hand_over(
        &self,
        owner: &Account,
        workspace: &str,
        next: &Account,
    ) -> Result<(), SystemError> {
        let invitation = self.invite(owner, workspace, next, "admin").await?;
        let accept = format!("/v1/users/me/invitations/{invitation}/accept");
        self.send(Method::POST, &accept, Some(next), None).await?;
        let record = self
            .send(Method::GET, "/v1/users/me", Some(next), None)
            .await?;
        let admin = text(&record, "id")?;
        self.send(
            Method::POST,
            &format!("/v1/workspaces/{workspace}/transfer"),
            Some(owner),
            Some(json!({ "user_id": admin })),
        )
        .await?;
        self.send(
            Method::POST,
            &format!("/v1/workspaces/{workspace}/leave"),
            Some(owner),
            None,
        )
        .await?;
        Ok(())
    }
    // end part: invitations
}

/// The seconds since the Unix epoch of `timestamp`, a time as the API writes one: RFC 3339 in
/// UTC, as `2026-11-15T10:30:00.123456Z` (API-11).
fn unix_seconds(timestamp: &str) -> Result<u64, SystemError> {
    let not_a_time = || {
        SystemError::Internal(Internal::new(format!(
            "the answer's time isn't one: {timestamp}"
        )))
    };
    let number = |from: usize, to: usize| {
        timestamp
            .get(from..to)
            .and_then(|digits| digits.parse::<u32>().ok())
            .ok_or_else(not_a_time)
    };
    let narrow = |value: u32| u8::try_from(value).map_err(|_| not_a_time());
    let year = i32::try_from(number(0, 4)?).map_err(|_| not_a_time())?;
    let month = time::Month::try_from(narrow(number(5, 7)?)?).map_err(|_| not_a_time())?;
    let date = time::Date::from_calendar_date(year, month, narrow(number(8, 10)?)?)
        .map_err(|_| not_a_time())?;
    let time = time::Time::from_hms(
        narrow(number(11, 13)?)?,
        narrow(number(14, 16)?)?,
        narrow(number(17, 19)?)?,
    )
    .map_err(|_| not_a_time())?;
    let at = time::PrimitiveDateTime::new(date, time).assume_utc();
    u64::try_from(at.unix_timestamp()).map_err(|_| not_a_time())
}

/// The code that an authenticator app with `secret`, in base32, shows at `at`, in seconds since
/// the Unix epoch, as RFC 6238 computes it (AUTH-057).
fn app_code(secret: &str, at: u64) -> Result<String, SystemError> {
    let not_a_secret = || SystemError::Internal(Internal::new("the setup's secret isn't one"));
    let bytes = data_encoding::BASE32_NOPAD
        .decode(secret.as_bytes())
        .map_err(|_| not_a_secret())?;
    let bytes: [u8; 20] = bytes.try_into().map_err(|_| not_a_secret())?;
    let app = totp_rs::Builder::new()
        .with_algorithm(totp_rs::Algorithm::SHA1)
        .with_digits(6)
        .with_skew(0)
        .with_step_duration(APP_STEP_SECONDS)
        .with_secret(totp_rs::Secret::new_stack(bytes))
        .build()
        .map_err(|_| not_a_secret())?;
    Ok(app.generate(at).to_string())
}
