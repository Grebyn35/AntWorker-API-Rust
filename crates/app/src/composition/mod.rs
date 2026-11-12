//! The composition root: builds every service once and mounts every module's routes (ARCH-12).
//! `main.rs` opens the process's resources, such as the database, and hands them here. The entry
//! points of the other subcommands that run the modules' logic are in `tasks.rs`.

mod tasks;

pub use tasks::{RemovalRefused, SeedError, SeedReport, remove_second_factor, seed, seed_allowed};

use std::{ffi::OsString, io, path::Path, sync::Arc, time::Duration};

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
    outbound::Configured,
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
    /// The OAuth providers that the deployment configures (AUTH-081).
    providers: auth::Providers,
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
    /// The deployment's emails, which `main.rs` builds from the settings, and the seed with an
    /// inbox of its own (the system spec's Seed).
    pub mail: Mail,
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
        workers,
        mail,
        ..
    } = resources;
    let mut jobs = Jobs::new(database, server.job_dead_letter_retention);
    mail.register(&mut jobs).map_err(Internal::new)?;
    let contracts = contracts(mail);
    let providers = Arc::new(config.providers);
    let outbound = Configured::new(server.environment, &server.outbound).map_err(Internal::new)?;
    let auth = Arc::new(
        auth::Auth::new(
            database.clone(),
            config.auth,
            auth::Keys::of(server),
            workers.clone(),
            accounts::Deletion::new(contracts.clone()),
            auth::Outside {
                mail: mail.clone(),
                providers: providers.clone(),
                outbound,
            },
        )
        .await?,
    );
    auth::register(&auth, &mut jobs).map_err(Internal::new)?;
    // part: files
    let files = Files::new(
        database.clone(),
        Storage::new(&server.storage, resources.clock.clone())?,
        Scanner::new(&server.scanner, resources.clock.clone()),
        workers.clone(),
        SignedUrls::new(server.hmac_key.clone()),
        config.files,
    );
    files.register(&mut jobs).map_err(Internal::new)?;
    // end part: files
    let exports = accounts::Exports::new(
        database.clone(),
        mail.clone(),
        workers.clone(),
        contracts,
        // part: files
        files.clone(),
        // end part: files
    );
    exports.register(&mut jobs).map_err(Internal::new)?;
    // part: invitations
    let invitations = Invitations::new(database.clone(), mail.clone(), config.invitations);
    invitations.register(&mut jobs).map_err(Internal::new)?;
    // end part: invitations
    let verifier: Arc<dyn SessionVerifier> = Arc::new(auth::Verifier(auth.clone()));
    let policy = Arc::new(access::Policy);
    let realtime = realtime(
        resources,
        server,
        config.heartbeat,
        listener,
        &policy,
        &verifier,
    );
    let states = States {
        capabilities: system::Capabilities::new(
            system::Features::new(mail.is_on()),
            sign_in_providers(&providers),
        ),
        auth,
        exports,
        realtime,
        // part: invitations
        invitations,
        // end part: invitations
        // part: files
        files,
        // end part: files
    };
    Ok(Services {
        routes: routes(resources, states),
        verifier,
        authorizer: Authorizer {
            database: database.clone(),
            policy,
            sink: Arc::new(audit::Sink::new(database.clone())),
        },
        jobs,
    })
}

/// The providers that `providers` configures, as the capabilities endpoint lists them
/// (AUTH-081).
fn sign_in_providers(providers: &auth::Providers) -> Vec<system::SignInProvider> {
    providers
        .listed()
        .into_iter()
        .map(|listed| system::SignInProvider {
            provider: listed.provider,
            name: listed.name,
            authorization_endpoint: listed.authorization_endpoint,
            scopes: listed.scopes.to_vec(),
            clients: listed
                .clients
                .into_iter()
                .map(|(client_id, redirect_uris)| system::SignInClient {
                    client_id,
                    redirect_uris,
                })
                .collect(),
        })
        .collect()
}

/// The states of the modules' routes that `compose` builds, which [`routes`] mounts.
struct States {
    capabilities: system::Capabilities,
    auth: Arc<auth::Auth>,
    exports: accounts::Exports,
    realtime: Realtime,
    // part: invitations
    invitations: Invitations,
    // end part: invitations
    // part: files
    files: Files,
    // end part: files
}

/// Every module's routes, each with its state from `states`, or the pool or the workers of
/// `resources`. `just new-module` adds a module's routes before the chain's last link.
fn routes(resources: &Resources, states: States) -> Routes {
    let database = &resources.database;
    system::routes()
        .with_state(states.capabilities)
        .merge(accounts::routes().with_state(database.clone()))
        .merge(accounts::export_routes().with_state(states.exports.clone()))
        .merge(auth::routes().with_state(states.auth.clone()))
        .merge(
            auth::export_routes()
                .with_state(auth::ExportRequests::new(states.auth, states.exports)),
        )
        // part: invitations
        .merge(invitations::routes().with_state(states.invitations))
        // end part: invitations
        // part: files
        .merge(accounts::avatar_routes().with_state(accounts::Avatars::new(
            database.clone(),
            states.files.clone(),
            resources.workers.clone(),
        )))
        .merge(files::routes().with_state(states.files))
        // end part: files
        .merge(workspaces::routes().with_state(database.clone()))
        .merge(realtime::routes().with_state(states.realtime))
}

/// The state of `realtime`: the hub, started on `listener` with the resources' readiness and
/// clock, which re-checks each stream as `server` says through `policy` and `verifier`, and the
/// heartbeat its streams send every `heartbeat` (the realtime spec).
fn realtime(
    resources: &Resources,
    server: &ServerConfig,
    heartbeat: Duration,
    listener: Listener,
    policy: &Arc<access::Policy>,
    verifier: &Arc<dyn SessionVerifier>,
) -> Realtime {
    Realtime {
        hub: Hub::start(
            listener,
            policy.clone(),
            verifier.clone(),
            resources.readiness.clone(),
            resources.clock.clone(),
            server.stream_recheck,
        ),
        clock: resources.clock.clone(),
        heartbeat,
    }
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
        accounts::export_routes().openapi(service),
        auth::routes().openapi(service),
        auth::export_routes().openapi(service),
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

/// Every module's personal data contract, in the order an account's deletion runs them, the
/// contract of `accounts`, which sends its email through `mail`, last (the accounts spec's
/// "Deleting an account").
fn contracts(mail: &Mail) -> Vec<Arc<dyn PersonalData>> {
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
        Arc::new(accounts::Contract::new(mail.clone())),
    ]
}

/// Every column that the personal data contracts registered with `mail` cover, which a test
/// compares with the migrated schema's columns that reference `users` or hold personal data
/// (DATA-23).
#[cfg(feature = "test-support")]
pub fn personal_data_columns(mail: &Mail) -> Vec<(&'static str, &'static str)> {
    contracts(mail)
        .iter()
        .flat_map(|contract| contract.columns().iter())
        .map(|column| (column.table, column.name))
        .collect()
}

/// The permission matrix of `access`: each permission's name, with the names of the roles that
/// hold it, which the generated test of every workspace route reads (TEST-08).
#[cfg(feature = "test-support")]
pub fn permissions() -> Vec<(&'static str, Vec<&'static str>)> {
    access::matrix()
}
