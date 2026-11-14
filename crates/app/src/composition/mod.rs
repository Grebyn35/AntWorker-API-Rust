//! The composition root: builds every service once and mounts every module's routes (ARCH-12).
//! `main.rs` opens the process's resources, such as the database, and hands them here. The
//! modules' settings are in `config.rs`, and the entry points of the other subcommands that run
//! the modules' logic in `tasks.rs`.

mod config;
mod providers;
mod tasks;

pub use config::Config;
pub use tasks::{RemovalRefused, SeedError, SeedReport, remove_second_factor, seed, seed_allowed};
// part: workspace_setup
pub use tasks::{SetupRefused, setup_workspace};
// end part: workspace_setup

use std::{sync::Arc, time::Duration};

use platform::{
    authn::SessionVerifier,
    authz::Authorizer,
    blocking::Workers,
    clock::SharedClock,
    config::{Environment, ServerConfig},
    db::Database,
    errors::{self, Internal},
    events::{Hub, Listener},
    http::{Origins, Pipeline, Routes},
    jobs::Jobs,
    lifecycle::Readiness,
    mail::Mail,
    outbound::Configured,
    personal_data::{self, PersonalData},
    rate_limit::RateLimiter,
    telemetry::Service,
};
// part: files
use platform::{outbound::Untrusted, scanner::Scanner, signed_urls::SignedUrls, storage::Storage};
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
    /// The replica's hub, whose head the tests read to wait until it holds what committed
    /// (TEST-06).
    #[cfg(feature = "test-support")]
    pub hub: Hub,
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
    let auth = auth_service(resources, server, config.auth, &providers, &contracts).await?;
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
    let avatars = accounts::Avatars::new(
        database.clone(),
        files.clone(),
        workers.clone(),
        Untrusted::new(&server.outbound).map_err(Internal::new)?,
    );
    avatars.register(&mut jobs).map_err(Internal::new)?;
    let pictures = workspaces::Pictures::new(files.clone(), workers.clone());
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
    #[cfg(feature = "test-support")]
    let hub = realtime.hub.clone();
    let states = States {
        capabilities: system::Capabilities::new(
            system::Features::new(mail.is_on()),
            providers::sign_in_providers(&providers),
        ),
        auth,
        providers,
        exports,
        realtime,
        // part: invitations
        invitations,
        // end part: invitations
        // part: files
        files,
        avatars,
        pictures,
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
        #[cfg(feature = "test-support")]
        hub,
    })
}

/// The service of `auth`, on `resources`, with `server`'s keys and outbound settings, the module's
/// `settings`, the deployment's `providers`, and the personal data `contracts` that a deletion
/// erases.
async fn auth_service(
    resources: &Resources,
    server: &ServerConfig,
    settings: auth::Settings,
    providers: &Arc<auth::Providers>,
    contracts: &[Arc<dyn PersonalData>],
) -> Result<Arc<auth::Auth>, Internal> {
    let outbound = Configured::new(server.environment, &server.outbound).map_err(Internal::new)?;
    Ok(Arc::new(
        auth::Auth::new(
            resources.database.clone(),
            settings,
            auth::Keys::of(server),
            resources.workers.clone(),
            accounts::Deletion::new(contracts.to_vec()),
            auth::Outside {
                mail: resources.mail.clone(),
                providers: providers.clone(),
                outbound,
                clock: resources.clock.clone(),
            },
        )
        .await?,
    ))
}

/// The states of the modules' routes that `compose` builds, which [`routes`] mounts.
struct States {
    capabilities: system::Capabilities,
    auth: Arc<auth::Auth>,
    providers: Arc<auth::Providers>,
    exports: accounts::Exports,
    realtime: Realtime,
    // part: invitations
    invitations: Invitations,
    // end part: invitations
    // part: files
    files: Files,
    avatars: accounts::Avatars,
    pictures: workspaces::Pictures,
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
            auth::provider_routes().with_state(auth::ProviderRoutes::new(
                states.auth.clone(),
                states.providers,
            )),
        )
        .merge(
            auth::export_routes()
                .with_state(auth::ExportRequests::new(states.auth, states.exports)),
        )
        // part: invitations
        .merge(invitations::routes().with_state(states.invitations))
        // end part: invitations
        // part: files
        .merge(accounts::avatar_routes().with_state(states.avatars))
        .merge(files::routes().with_state(states.files))
        .merge(workspaces::picture_routes().with_state(states.pictures))
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
        auth::provider_routes().openapi(service),
        workspaces::routes().openapi(service),
        realtime::routes().openapi(service),
        // part: invitations
        invitations::routes().openapi(service),
        // end part: invitations
        // part: files
        accounts::avatar_routes().openapi(service),
        files::routes().openapi(service),
        workspaces::picture_routes().openapi(service),
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
