//! Second factors (D134): reading a user's, setting up an authenticator app and confirming the
//! setup, new recovery codes and removing the app; in `completion.rs`, a sign-in's challenge and
//! its completion with a code; and in `checks.rs`, the check of a code, which a completion, a
//! re-authentication and a change of the password go through (AUTH-054 to AUTH-069, the auth
//! spec's "Second factors").

mod checks;
mod completion;

use platform::{
    authn::Principal,
    db::UnitOfWork,
    encryption::Binding,
    errors::Internal,
    jobs::JobScope,
    rate_limit::RateLimits,
    types::{Client, EmailAddress, Secret, Timestamp, UserId},
};

pub(crate) use completion::{Challenge, Completion};

use self::checks::{count, recovery_value};
use super::{Auth, buckets, reauthentication};
use crate::modules::{
    PRODUCT,
    audit::{self as audit_log, Entry},
    auth::{
        audit,
        emails::{FactorChange, FactorChangedEmail},
        error::AuthError,
        metrics::{self, FactorCheck},
        model::{AppCode, AppSecret, RecoveryCode, STEP_SECONDS},
        repo::second_factors as factors,
    },
};

/// How long a setup lasts, in minutes: what installing an app may take (AUTH-055).
const SETUP_MINUTES: i32 = 15;

/// How many recovery codes each set holds (AUTH-056, AUTH-063).
const RECOVERY_CODES: usize = 10;

/// The kind of the keyed hash of a recovery code (AUTH-062).
const RECOVERY_CODE_KIND: &str = "recovery_code";

/// The tables and the column whose rows' secrets the key ring seals (SEC-02).
const APPS: &str = "totp_factors";
const SETUPS: &str = "totp_setups";
const SECRET: &str = "secret";

/// A user's second factors, as they read them (AUTH-054).
pub(crate) struct SecondFactors {
    /// Their authenticator app: when its setup was confirmed, and when the last step whose code
    /// it accepted began.
    pub(crate) app: Option<(Timestamp, Timestamp)>,
    /// How many of their recovery codes still work.
    pub(crate) recovery_codes_left: i64,
}

/// A setup of an app, as its answer shows it once (AUTH-055).
pub(crate) struct Setup {
    /// The secret, in base32.
    pub(crate) secret: Secret<String>,
    /// The `otpauth://totp/` URI that holds it.
    pub(crate) uri: Secret<String>,
    /// When the setup expires.
    pub(crate) expires_at: Timestamp,
}

impl Auth {
    /// The caller's second factors (AUTH-054).
    pub(crate) async fn second_factors(
        &self,
        client: &Client,
        principal: &Principal,
    ) -> Result<SecondFactors, AuthError> {
        let user_id = principal.user_id();
        let mut uow = self.database.begin(client).await?;
        let app = factors::app(&mut uow, user_id).await?;
        let recovery_codes_left = factors::count_recovery_codes(&mut uow, user_id).await?;
        drop(uow);
        let app = app
            .map(|app| Ok::<_, Internal>((app.created_at, step_start(app.last_used_step)?)))
            .transpose()?;
        Ok(SecondFactors {
            app,
            recovery_codes_left,
        })
    }

    /// Starts a setup of an app for the caller, once their session proved who they are within
    /// the last 5 minutes, in place of the setup before it, at the user's turn, so two setups
    /// take turns (AUTH-055, DATA-20).
    pub(crate) async fn start_setup(
        &self,
        client: &Client,
        principal: &Principal,
    ) -> Result<Setup, AuthError> {
        let user_id = principal.user_id();
        let mut uow = self.database.begin(client).await?;
        reauthentication::require_recent_proof(&mut uow, principal).await?;
        let address = reauthentication::take_turn(&mut uow, principal).await?;
        let secret = AppSecret::generate()?;
        let id = factors::new_id(&mut uow).await?;
        let sealed = self
            .keyring
            .seal(&Binding::new(SETUPS, SECRET, id), secret.bytes())?;
        let expires_at =
            factors::replace_setup(&mut uow, user_id, id, &sealed, SETUP_MINUTES).await?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::SECOND_FACTOR_SETUP_STARTED, audit::SECOND_FACTOR, id),
        )
        .await?;
        uow.commit().await?;
        Ok(Setup {
            secret: Secret::new(secret.base32()),
            uri: Secret::new(secret.uri(PRODUCT, address.email.as_str())),
            expires_at,
        })
    }

    /// Confirms the caller's setup with a code of its secret: the app becomes theirs, in place of
    /// any before it, with ten new recovery codes, which it returns (AUTH-056).
    pub(crate) async fn confirm_setup(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
        code: AppCode,
    ) -> Result<Vec<RecoveryCode>, AuthError> {
        let user_id = principal.user_id();
        count(
            limits,
            buckets::SECOND_FACTOR_CHECK,
            user_id.uuid().as_bytes(),
        )
        .await?;
        let mut uow = self.database.begin(client).await?;
        let address = reauthentication::take_turn(&mut uow, principal).await?;
        let setup = factors::lasting_setup(&mut uow, user_id)
            .await?
            .ok_or(AuthError::SetupExpired)?;
        let secret = AppSecret::from_bytes(
            self.keyring
                .open(&Binding::new(SETUPS, SECRET, setup.id), &setup.secret)?
                .expose(),
        )?;
        let now = factors::now(&mut uow).await?;
        let Some(step) = secret.accepts(&code, now, None) else {
            metrics::factor_check(FactorCheck::WrongCode);
            return Err(self
                .wrong_factor(uow, principal.audit_scope(), user_id, AuthError::WrongCode)
                .await?);
        };
        metrics::factor_check(FactorCheck::Passed);
        // Of two confirmations of one setup, the second waits for the first's turn, then finds
        // the setup gone.
        if !factors::claim_setup(&mut uow, user_id, setup.id).await? {
            return Err(AuthError::SetupExpired);
        }
        let id = factors::new_id(&mut uow).await?;
        let sealed = self
            .keyring
            .seal(&Binding::new(APPS, SECRET, id), secret.bytes())?;
        factors::replace_app(&mut uow, user_id, id, &sealed, step).await?;
        let codes = self.issue_recovery_codes(&mut uow, user_id).await?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::SECOND_FACTOR_ENABLED, audit::SECOND_FACTOR, id),
        )
        .await?;
        self.tell_factor_changed(
            &mut uow,
            principal.job_scope(),
            address.email,
            FactorChange::AppAdded,
        )
        .await?;
        uow.commit().await?;
        Ok(codes)
    }

    /// Replaces the caller's recovery codes with ten new ones, which it returns, once their
    /// session proved who they are within the last 5 minutes (AUTH-063).
    pub(crate) async fn new_recovery_codes(
        &self,
        client: &Client,
        principal: &Principal,
    ) -> Result<Vec<RecoveryCode>, AuthError> {
        let user_id = principal.user_id();
        let mut uow = self.database.begin(client).await?;
        reauthentication::require_recent_proof(&mut uow, principal).await?;
        let address = reauthentication::take_turn(&mut uow, principal).await?;
        if !factors::has_app(&mut uow, user_id).await? {
            return Err(AuthError::NoSecondFactor);
        }
        let codes = self.issue_recovery_codes(&mut uow, user_id).await?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::RECOVERY_CODES_ISSUED, audit::USER, user_id.uuid()),
        )
        .await?;
        self.tell_factor_changed(
            &mut uow,
            principal.job_scope(),
            address.email,
            FactorChange::CodesReplaced,
        )
        .await?;
        uow.commit().await?;
        Ok(codes)
    }

    /// Removes the caller's app, with their recovery codes and their sign-in challenges, once
    /// their session proved who they are within the last 5 minutes; without an app, it changes
    /// nothing (AUTH-064, AUTH-069).
    pub(crate) async fn remove_app(
        &self,
        client: &Client,
        principal: &Principal,
    ) -> Result<(), AuthError> {
        let user_id = principal.user_id();
        let mut uow = self.database.begin(client).await?;
        reauthentication::require_recent_proof(&mut uow, principal).await?;
        let address = reauthentication::take_turn(&mut uow, principal).await?;
        let Some(id) = factors::delete_app(&mut uow, user_id).await? else {
            return Ok(());
        };
        factors::delete_recovery_codes(&mut uow, user_id).await?;
        factors::delete_challenges(&mut uow, user_id).await?;
        audit_log::record(
            &mut uow,
            principal.audit_scope(),
            Entry::new(audit::SECOND_FACTOR_REMOVED, audit::SECOND_FACTOR, id),
        )
        .await?;
        self.tell_factor_changed(
            &mut uow,
            principal.job_scope(),
            address.email,
            FactorChange::AppRemoved,
        )
        .await?;
        uow.commit().await?;
        Ok(())
    }

    /// Ten new recovery codes of `user_id`'s, whose HMACs replace those before them (AUTH-062).
    async fn issue_recovery_codes(
        &self,
        uow: &mut UnitOfWork,
        user_id: UserId,
    ) -> Result<Vec<RecoveryCode>, Internal> {
        let codes = (0..RECOVERY_CODES)
            .map(|_| RecoveryCode::generate())
            .collect::<Result<Vec<_>, _>>()?;
        let hashes: Vec<Vec<u8>> = codes
            .iter()
            .map(|code| {
                self.key
                    .hash(RECOVERY_CODE_KIND, &recovery_value(user_id, code))
                    .to_vec()
            })
            .collect();
        factors::replace_recovery_codes(uow, user_id, &hashes).await?;
        Ok(codes)
    }

    /// Enqueues in `uow` the email that tells `to` its account's second factor changed, while
    /// email is on (AUTH-068).
    async fn tell_factor_changed(
        &self,
        uow: &mut UnitOfWork,
        scope: JobScope,
        to: EmailAddress,
        change: FactorChange,
    ) -> Result<(), Internal> {
        if !self.mail.is_on() {
            return Ok(());
        }
        let email = FactorChangedEmail { to, change }.render()?;
        self.mail.send(uow, scope, email).await
    }
}

/// When the step `step` began.
fn step_start(step: i64) -> Result<Timestamp, Internal> {
    let at = time::OffsetDateTime::from_unix_timestamp(step * STEP_SECONDS)
        .map_err(|_| Internal::new("an app's last used step is out of range"))?;
    Ok(Timestamp::from_database(at))
}
