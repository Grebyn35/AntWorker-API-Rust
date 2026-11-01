//! The composition root: builds every service once and mounts every module's routes (ARCH-12).
//! `main.rs` opens the process's resources, such as the database, and hands them here.

use std::{ffi::OsString, io, path::Path, sync::Arc};

use platform::{
    authn::SessionVerifier,
    authz::Authorizer,
    clock::SharedClock,
    config::{ConfigError, ServerConfig, Settings},
    db::Database,
    errors::Internal,
    http::{Origins, Pipeline, Routes},
    jobs::Jobs,
    lifecycle::Readiness,
    rate_limit::{Bucket, RateLimiter},
    telemetry::Service,
};
use sqlx::migrate::Migrator;
use utoipa::openapi::OpenApi;

use crate::modules::{
    access, accounts, audit, auth,
    invitations::{self, Invitations},
    mail::{self, Mail},
    system, workspaces,
};

/// Every migration in `migrations/`, embedded in the binary. `migrate` applies them, and
/// `serve` refuses to start until they are all applied (DATA-09).
pub static MIGRATIONS: Migrator = sqlx::migrate!("../../migrations");

/// The settings of the modules, which `serve` reads before its async runtime starts (OPS-01).
pub struct Config {
    auth: auth::Settings,
    mail: mail::Settings,
    invitations: invitations::Settings,
}

/// The name of the per-email sign-in limit's bucket (AUTH-006).
const SIGN_IN_BUCKET: &str = "sign_in";

impl Config {
    /// The modules' settings from `settings`.
    pub fn read<E, F>(settings: &Settings<E, F>) -> Result<Self, ConfigError>
    where
        E: Fn(&str) -> Option<OsString>,
        F: Fn(&Path) -> io::Result<String>,
    {
        let day: u32 = 86_400;
        Ok(Self {
            auth: auth::Settings {
                lifetimes: auth::Lifetimes {
                    access: settings.duration("ACCESS_TOKEN_SECONDS", 3_600, 60..=day)?,
                    refresh: settings.duration(
                        "REFRESH_TOKEN_SECONDS",
                        30 * day,
                        3_600..=365 * day,
                    )?,
                    absolute: settings.duration(
                        "SESSION_LIFETIME_SECONDS",
                        90 * day,
                        3_600..=365 * day,
                    )?,
                },
                sign_in: Bucket {
                    name: SIGN_IN_BUCKET,
                    limit: settings.number("SIGN_IN_ATTEMPTS", 5, 1..=1_000)?,
                    window: settings.duration("SIGN_IN_WINDOW_SECONDS", 900, 1..=day)?,
                },
            },
            mail: mail::Settings::read(settings)?,
            invitations: invitations::Settings {
                expiry_days: settings.number("INVITATION_EXPIRY_DAYS", 7, 1..=90)?,
            },
        })
    }
}

/// The services the request pipeline takes from the modules.
pub struct Services {
    /// Every module's routes, with their state.
    pub routes: Routes,
    /// The verifier of access tokens, which `auth` implements (ARCH-13).
    pub verifier: Arc<dyn SessionVerifier>,
    /// What authorizes workspace routes: the policy `access` implements, and the sink of
    /// refusals `audit` implements (ARCH-13).
    pub authorizer: Authorizer,
}

/// Builds every service once, on `database`, mounts every module's routes, and registers every
/// module's jobs in `jobs` (D13).
pub async fn compose(
    database: &Database,
    config: Config,
    clock: SharedClock,
    jobs: &mut Jobs,
) -> Result<Services, Internal> {
    let auth = Arc::new(auth::Auth::new(database.clone(), config.auth).await?);
    let mail = Mail::new(config.mail, clock)?;
    mail.register(jobs).map_err(Internal::new)?;
    let invitations = Invitations::new(database.clone(), mail.clone(), config.invitations);
    invitations.register(jobs).map_err(Internal::new)?;
    let features = system::Features {
        email: mail.is_on(),
    };
    let routes = system::routes()
        .with_state(features)
        .merge(accounts::routes().with_state(database.clone()))
        .merge(auth::routes().with_state(auth.clone()))
        .merge(workspaces::routes().with_state(database.clone()))
        .merge(invitations::routes().with_state(invitations));
    Ok(Services {
        routes,
        verifier: Arc::new(auth::Verifier(auth)),
        authorizer: Authorizer {
            database: database.clone(),
            policy: Arc::new(access::Policy),
            sink: Arc::new(audit::Sink::new(database.clone())),
        },
    })
}

/// The request pipeline that `config` describes, which counts its rate limits in `database` and
/// verifies access tokens through `services`.
pub fn pipeline(
    config: &ServerConfig,
    database: &Database,
    services: &Services,
    clock: SharedClock,
    readiness: Readiness,
) -> Pipeline {
    Pipeline {
        clock,
        max_concurrent_requests: usize::try_from(config.max_concurrent_requests)
            .unwrap_or(usize::MAX),
        readiness,
        trusted_proxies: config.trusted_proxies.clone(),
        origins: Origins::new(&config.cors_allowed_origins),
        limiter: RateLimiter::new(database.clone(), config.hmac_key.clone()),
        rate_limits: config.rate_limits,
        verifier: services.verifier.clone(),
        authorizer: services.authorizer.clone(),
    }
}

/// The OpenAPI document of every module's routes, which needs no database (API-17).
pub fn openapi(service: Service) -> OpenApi {
    let mut document = system::routes().openapi(service);
    document.merge(accounts::routes().openapi(service));
    document.merge(auth::routes().openapi(service));
    document.merge(workspaces::routes().openapi(service));
    document.merge(invitations::routes().openapi(service));
    document
}

/// The permission matrix of `access`: each permission's name, with the names of the roles that
/// hold it, which the generated test of every workspace route reads (TEST-09).
pub fn permissions() -> Vec<(&'static str, Vec<&'static str>)> {
    access::matrix()
}
