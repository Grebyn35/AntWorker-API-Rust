//! The composition root: builds every service once and mounts every module's routes (ARCH-12).
//! `main.rs` opens the process's resources, such as the database, and hands them here. The
//! modules' settings are in `config.rs`, the request pipeline that serves the routes in
//! `pipeline.rs`, and the other subcommands that run the modules' logic in `tasks.rs`, those that
//! an operator runs against the database, and in `seed.rs`, the entry points of `seed`.
// part: files
//! The one that an operator runs against the bucket of files' bytes is in `restore_files.rs`.
// end part: files

mod config;
mod pipeline;
mod providers;
// part: files
mod restore_files;
// end part: files
mod seed;
mod tasks;

pub use config::Config;
pub use pipeline::pipeline;
// part: files
pub use restore_files::{RestoreFiles, RestoreRefused};
// end part: files
pub use seed::{SeedError, SeedReport, seed, seed_allowed};
pub use tasks::{Done, EndingRefused, Operation, Refused, RemovalRefused};
// part: superadmins
pub use tasks::{SuperadminCommand, SuperadminRefused};
// end part: superadmins
// part: workspace_setup
pub use tasks::SetupRefused;
// end part: workspace_setup

use std::{sync::Arc, time::Duration};

use platform::{
    authn::SessionVerifier,
    authz::Authorizer,
    blocking::Workers,
    clock::SharedClock,
    config::ServerConfig,
    db::Database,
    errors::Internal,
    events::{Hub, Listener},
    http::Routes,
    jobs::Jobs,
    lifecycle::Readiness,
    mail::Mail,
    outbound::Configured,
    personal_data::{self, PersonalData},
    telemetry::Service,
};
// part: files
use platform::{outbound::Untrusted, scanner::Scanner, signed_urls::SignedUrls, storage::Storage};
// end part: files
// part: client_telemetry
use platform::telemetry::Relay;
// end part: client_telemetry
use sqlx::migrate::Migrator;
use utoipa::openapi::OpenApi;

use crate::modules::{
    access,
    accounts,
    audit,
    auth,
    // part: client_telemetry
    client_telemetry,
    // end part: client_telemetry
    // part: files
    files::{self, Files},
    // end part: files
    // part: invitations
    invitations::{self, Invitations},
    // end part: invitations
    realtime::{self, Realtime},
    // part: superadmins
    superadmins,
    // end part: superadmins
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
    let mut jobs = registry(database, server, mail, config.audit)?;
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
        Untrusted::new(&server.outbound, providers.picture_hosts()).map_err(Internal::new)?,
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
    let features = system::Features::new(mail.is_on());
    // part: client_telemetry
    let features = features.client_telemetry(config.client_telemetry.endpoint.is_some());
    // end part: client_telemetry
    let states = States {
        capabilities: system::Capabilities::new(features, providers::sign_in_providers(&providers)),
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
        // part: client_telemetry
        client_telemetry: client_telemetry_state(resources, &config.client_telemetry)?,
        // end part: client_telemetry
    };
    Ok(Services {
        routes: routes(resources, states),
        verifier,
        authorizer: Authorizer {
            database: database.clone(),
            policy,
            sink: Arc::new(audit::Sink::new(database.clone())),
            // part: superadmins
            superadmins: Arc::new(superadmins::Policy),
            // end part: superadmins
        },
        jobs,
        #[cfg(feature = "test-support")]
        hub,
    })
}

/// The registry of jobs, with `platform`'s sweeps, the sending of email, and audit's retention,
/// none of which a module's service runs (AUDIT-011).
fn registry(
    database: &Database,
    server: &ServerConfig,
    mail: &Mail,
    audit: audit::Settings,
) -> Result<Jobs, Internal> {
    let mut jobs = Jobs::new(database, server.job_dead_letter_retention);
    mail.register(&mut jobs).map_err(Internal::new)?;
    audit::Retention::new(database.clone(), audit)
        .register(&mut jobs)
        .map_err(Internal::new)?;
    Ok(jobs)
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

// part: client_telemetry
/// The state of `client_telemetry`, on `resources`: with the collector's receiver that `config`
/// names, if it names one, its proxy, which forwards through a relay to that receiver, within
/// the two budgets of `config`, the replica's counted for an ID drawn here.
fn client_telemetry_state(
    resources: &Resources,
    config: &config::TelemetryProxy,
) -> Result<client_telemetry::ClientTelemetry, Internal> {
    let replica = platform::random::bytes::<16>()?;
    let proxy = config
        .endpoint
        .as_ref()
        .map(|endpoint| Relay::new(endpoint, resources.clock.clone()))
        .transpose()
        .map_err(Internal::new)?
        .map(|relay| client_telemetry::Proxy::new(relay, config.budgets, replica));
    Ok(client_telemetry::ClientTelemetry::new(
        resources.database.clone(),
        resources.workers.clone(),
        proxy,
    ))
}
// end part: client_telemetry

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
    // part: client_telemetry
    client_telemetry: client_telemetry::ClientTelemetry,
    // end part: client_telemetry
}

/// Every module's routes, each with its state from `states`, or the pool or the workers of
/// `resources`. `just new-module` adds a module's routes before the chain's last link.
fn routes(resources: &Resources, states: States) -> Routes {
    let database = &resources.database;
    system::routes()
        .with_state(states.capabilities)
        .merge(accounts::routes().with_state(database.clone()))
        .merge(accounts::export_routes().with_state(states.exports.clone()))
        .merge(audit::routes().with_state(database.clone()))
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
        // part: superadmins
        .merge(superadmins::routes().with_state(database.clone()))
        // end part: superadmins
        // part: client_telemetry
        .merge(client_telemetry::routes().with_state(states.client_telemetry))
        // end part: client_telemetry
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

/// The action catalog: every action that the modules record (AUDIT-012). `just new-module` adds a
/// module's actions before the list's end.
fn catalog() -> audit::Catalog {
    audit::Catalog::new(&[
        accounts::AUDIT_ACTIONS,
        audit::AUDIT_ACTIONS,
        auth::AUDIT_ACTIONS,
        workspaces::AUDIT_ACTIONS,
        // part: invitations
        invitations::AUDIT_ACTIONS,
        // end part: invitations
        // part: superadmins
        superadmins::AUDIT_ACTIONS,
        // end part: superadmins
        // part: client_telemetry
        client_telemetry::AUDIT_ACTIONS,
        // end part: client_telemetry
        // part: files
        files::AUDIT_ACTIONS,
        // end part: files
    ])
}

/// The OpenAPI document of every module's routes, with the event catalog of every event they
/// publish, and the action catalog of every action they record, which needs no database
/// (API-17, REALTIME-008, AUDIT-012).
pub fn openapi(service: Service) -> OpenApi {
    let mut document = platform::http::document([
        system::routes().openapi(service),
        accounts::routes().openapi(service),
        accounts::export_routes().openapi(service),
        audit::routes().openapi(service),
        auth::routes().openapi(service),
        auth::export_routes().openapi(service),
        auth::provider_routes().openapi(service),
        workspaces::routes().openapi(service),
        realtime::routes().openapi(service),
        // part: invitations
        invitations::routes().openapi(service),
        // end part: invitations
        // part: superadmins
        superadmins::routes().openapi(service),
        // end part: superadmins
        // part: client_telemetry
        client_telemetry::routes().openapi(service),
        // end part: client_telemetry
        // part: files
        accounts::avatar_routes().openapi(service),
        files::routes().openapi(service),
        workspaces::picture_routes().openapi(service),
        // end part: files
    ]);
    catalog().document(&mut document);
    document
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
        // part: superadmins
        Arc::new(superadmins::Contract),
        // end part: superadmins
        // part: client_telemetry
        Arc::new(client_telemetry::Contract),
        // end part: client_telemetry
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
