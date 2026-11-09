//! The composition root: builds every service once and mounts every module's routes (ARCH-12).
//! `main.rs` opens the process's resources, such as the database, and hands them here.

use std::{ffi::OsString, io, path::Path, sync::Arc, time::Duration};

use axum::Router;

use platform::{
    authn::SessionVerifier,
    authz::Authorizer,
    blocking::Workers,
    clock::SharedClock,
    config::{ConfigError, Environment, ServerConfig, Settings},
    db::Database,
    errors::{self, Internal},
    events::{Hub, Listener},
    http::{Origins, Pipeline, Routes},
    jobs::Jobs,
    lifecycle::Readiness,
    mail::Mail,
    personal_data::{self, PersonalData},
    rate_limit::{Bucket, RateLimiter},
    telemetry::Service,
};
// part: files
use platform::{scanner::Scanner, signed_urls::SignedUrls, storage::Storage};
// end part: files
use sqlx::migrate::Migrator;
use utoipa::openapi::OpenApi;

use crate::modules::{
    access,
    accounts,
    audit,
    auth,
    // part: files
    files::{self, Files},
    // end part: files
    // part: invitations
    invitations::{self, Invitations},
    // end part: invitations
    realtime::{self, Realtime},
    system,
    workspaces,
};

/// Every migration in `migrations/`, embedded in the binary. `migrate` applies them, and
/// `serve` refuses to start until they are all applied (DATA-09).
pub static MIGRATIONS: Migrator = sqlx::migrate!("../../migrations");

/// The settings of the modules, which `serve` reads before its async runtime starts (OPS-01).
pub struct Config {
    auth: auth::Settings,
    // part: files
    files: files::Settings,
    // end part: files
    // part: invitations
    invitations: invitations::Settings,
    // end part: invitations
    heartbeat: Duration,
}

/// The name of the bucket of the sign-in limit of an email from one client address (AUTH-006).
const SIGN_IN_BUCKET: &str = "sign_in";

/// The name of the bucket of the sign-in limit of an email from every address (AUTH-006).
const SIGN_IN_EMAIL_BUCKET: &str = "sign_in_email";

/// The name of the bucket of the sign-in limit of a device token (AUTH-006).
const SIGN_IN_DEVICE_BUCKET: &str = "sign_in_device";

/// The bucket of each user's checks of their password, once signed in: 5 in 15 minutes, fixed
/// (AUTH-022).
const PASSWORD_CHECK: Bucket = Bucket {
    name: "password_check",
    limit: 5,
    window: Duration::from_secs(900),
};

// part: invitations
/// The name of the per-member limit's bucket on inviting (INVITATIONS-009).
const INVITATION_BUCKET: &str = "invitation";
// end part: invitations

impl Config {
    /// The modules' settings from `settings`.
    pub fn read<E, F>(settings: &Settings<E, F>) -> Result<Self, ConfigError>
    where
        E: Fn(&str) -> Option<OsString>,
        F: Fn(&Path) -> io::Result<String>,
    {
        let day: u32 = 86_400;
        let sign_in_attempts = settings.number("SIGN_IN_ATTEMPTS", 5, 1..=1_000)?;
        let sign_in_window = settings.duration("SIGN_IN_WINDOW_SECONDS", 900, 1..=day)?;
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
                password_check: PASSWORD_CHECK,
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

/// The process's resources that the services and the pipeline share, which `main.rs` opens
/// before the services are built (ARCH-12).
pub struct Resources {
    /// The pool of connections as the role `app`.
    pub database: Database,
    /// The clock every service reads (CODE-06).
    pub clock: SharedClock,
    /// Whether the instance takes requests, which the hub and the shutdown change (OPS-05).
    pub readiness: Readiness,
    /// The turns of the CPU-heavy work, such as a password's hash (PLATFORM-060).
    pub workers: Workers,
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
    /// Every job's handler, which `serve` runs beside the server (D13).
    pub jobs: Jobs,
}

/// Builds every service once, on `resources`, with `server`'s settings for `platform`'s and the
/// modules' own in `config`, mounts every module's routes, registers every job, and starts the
/// hub on `listener`, which records in the resources' readiness whether it reaches the database.
pub async fn compose(
    resources: &Resources,
    server: &ServerConfig,
    config: Config,
    listener: Listener,
) -> Result<Services, Internal> {
    let Resources {
        database,
        clock,
        readiness,
        workers,
    } = resources;
    let mut jobs = Jobs::new(database, server.job_dead_letter_retention);
    let mail = Mail::new(server.mail.clone(), clock.clone())?;
    mail.register(&mut jobs).map_err(Internal::new)?;
    let auth = Arc::new(
        auth::Auth::new(
            database.clone(),
            config.auth,
            server.hmac_key.clone(),
            workers.clone(),
            accounts::Deletion::new(contracts(), mail.clone()),
        )
        .await?,
    );
    // part: files
    let files = Files::new(
        database.clone(),
        Storage::new(&server.storage, clock.clone())?,
        Scanner::new(&server.scanner, clock.clone()),
        workers.clone(),
        SignedUrls::new(server.hmac_key.clone()),
        config.files,
    );
    files.register(&mut jobs).map_err(Internal::new)?;
    // end part: files
    // part: invitations
    let invitations = Invitations::new(database.clone(), mail.clone(), config.invitations);
    invitations.register(&mut jobs).map_err(Internal::new)?;
    // end part: invitations
    let features = system::Features::new(mail.is_on());
    let verifier: Arc<dyn SessionVerifier> = Arc::new(auth::Verifier(auth.clone()));
    let policy = Arc::new(access::Policy);
    let realtime = Realtime {
        hub: Hub::start(
            listener,
            policy.clone(),
            verifier.clone(),
            readiness.clone(),
            clock.clone(),
            server.stream_recheck,
        ),
        clock: clock.clone(),
        heartbeat: config.heartbeat,
    };
    let routes = system::routes()
        .with_state(features)
        .merge(accounts::routes().with_state(database.clone()))
        .merge(auth::routes().with_state(auth.clone()))
        // part: invitations
        .merge(invitations::routes().with_state(invitations))
        // end part: invitations
        .merge(workspaces::routes().with_state(database.clone()))
        .merge(realtime::routes().with_state(realtime));
    // part: files
    let routes = routes
        .merge(accounts::avatar_routes().with_state(accounts::Avatars::new(
            database.clone(),
            files.clone(),
            workers.clone(),
        )))
        .merge(files::routes().with_state(files));
    // end part: files
    Ok(Services {
        routes,
        verifier,
        authorizer: Authorizer {
            database: database.clone(),
            policy,
            sink: Arc::new(audit::Sink::new(database.clone())),
        },
        jobs,
    })
}

/// The request pipeline that `config` describes, which counts its rate limits in the database of
/// `resources` and verifies access tokens through `services`, and which serves the API reference
/// of `service`'s document in development (PLATFORM-052).
pub fn pipeline(
    resources: &Resources,
    config: &ServerConfig,
    services: &Services,
    service: Service,
) -> Pipeline {
    let reference = if config.environment == Environment::Development {
        match serde_json::to_string(&openapi(service)) {
            Ok(document) => Some(Arc::from(document)),
            // Development goes without the reference, which serves no client (ERR-11).
            Err(error) => {
                errors::ignore(&error, "api_reference_not_written");
                None
            }
        }
    } else {
        None
    };
    Pipeline {
        clock: resources.clock.clone(),
        max_concurrent_requests: usize::try_from(config.max_concurrent_requests)
            .unwrap_or(usize::MAX),
        readiness: resources.readiness.clone(),
        trusted_proxies: config.trusted_proxies.clone(),
        origins: Origins::new(&config.cors_allowed_origins),
        limiter: RateLimiter::new(resources.database.clone(), config.hmac_key.clone()),
        rate_limits: config.rate_limits,
        verifier: services.verifier.clone(),
        // part: files
        signed_urls: SignedUrls::new(config.hmac_key.clone()),
        // end part: files
        authorizer: services.authorizer.clone(),
        reference,
    }
}

/// The OpenAPI document of every module's routes, with the event catalog of every event they
/// publish, which needs no database (API-17, REALTIME-008).
pub fn openapi(service: Service) -> OpenApi {
    platform::http::document([
        system::routes().openapi(service),
        accounts::routes().openapi(service),
        auth::routes().openapi(service),
        workspaces::routes().openapi(service),
        realtime::routes().openapi(service),
        // part: invitations
        invitations::routes().openapi(service),
        // end part: invitations
        // part: files
        accounts::avatar_routes().openapi(service),
        files::routes().openapi(service),
        // end part: files
    ])
}

/// What `seed` created: each account's email address and name, which all share `password`, and
/// each workspace's name and ID (SYSTEM-003).
pub struct SeedReport {
    /// Each account's email address and name, in the order it registered them.
    pub accounts: Vec<(&'static str, &'static str)>,
    /// Every account's password.
    pub password: &'static str,
    /// Each workspace's name and ID.
    pub workspaces: Vec<(&'static str, String)>,
}

/// Why `seed` refused, or stopped (SYSTEM-002, SYSTEM-004).
#[derive(Debug, thiserror::Error)]
#[error(transparent)]
pub struct SeedError(system::SystemError);

/// Refuses `seed` unless `environment` is development, before it connects to anything
/// (SYSTEM-002).
pub fn seed_allowed(environment: Environment) -> Result<(), SeedError> {
    system::seed_allowed(environment).map_err(SeedError)
}

/// Fills the database behind `router`, the API as `serve` builds it, with demo accounts and
/// workspaces, through its routes (the system spec's Seed).
pub async fn seed(router: Router) -> Result<SeedReport, SeedError> {
    let seeded = system::seed(router).await.map_err(SeedError)?;
    Ok(SeedReport {
        accounts: seeded
            .accounts
            .iter()
            .map(|account| (account.email, account.name))
            .collect(),
        password: system::PASSWORD,
        workspaces: seeded.workspaces,
    })
}

/// The personal data contracts of every module but `accounts`, whose own `Deletion` adds last, in
/// the order an account's deletion runs them (the accounts spec's "Deleting an account").
fn contracts() -> Vec<Arc<dyn PersonalData>> {
    vec![
        Arc::new(workspaces::Contract),
        Arc::new(access::Contract),
        // part: files
        Arc::new(files::Contract),
        // end part: files
        // part: invitations
        Arc::new(invitations::Contract),
        // end part: invitations
        Arc::new(auth::Contract),
        Arc::new(audit::Contract),
        Arc::new(personal_data::Contract),
    ]
}

/// Every column that the registered personal data contracts cover, which a test compares with
/// the migrated schema's columns that reference `users` or hold personal data (DATA-23).
pub fn personal_data_columns() -> Vec<(&'static str, &'static str)> {
    accounts::columns(&contracts())
        .into_iter()
        .map(|column| (column.table, column.name))
        .collect()
}

/// The permission matrix of `access`: each permission's name, with the names of the roles that
/// hold it, which the generated test of every workspace route reads (TEST-08).
pub fn permissions() -> Vec<(&'static str, Vec<&'static str>)> {
    access::matrix()
}
