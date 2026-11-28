use std::{sync::Arc, time::Duration};

use futures_util::FutureExt;
use platform::{
    blocking::Workers,
    clock::{SharedClock, TestClock},
    db::{BootstrapError, ConnectError, Database, DatabaseUrl, RoleProblem},
    jobs::{Job, JobScope, Jobs},
};
use serde::{Deserialize, Serialize};

// part: files
use crate::support::bucket;
// end part: files
use crate::support::{Proxy, TestDatabase, cleanup::Backlog, client, closed_port, execute_on};

/// Asserts that `serve` refuses to start as the role `url` connects as, for `problem`.
async fn assert_refused(url: &DatabaseUrl, problem: &RoleProblem) {
    let refused = Database::connect(url, 1, &app::MIGRATIONS).await.err();
    assert!(
        matches!(&refused, Some(ConnectError::UnsafeRole(problems)) if problems.contains(problem)),
        "expected {problem:?}, got {refused:?}"
    );
}

#[tokio::test]
async fn test_05_the_api_under_test_connects_as_the_app_role() {
    let db = TestDatabase::migrated().await;

    let app = db.app().await;

    let mut uow = app.begin(&crate::support::client(), None).await.unwrap();
    let role = sqlx::query_scalar!(r#"SELECT current_user AS "role!""#)
        .fetch_one(uow.executor())
        .await
        .unwrap();
    assert_eq!(role, "app");
}

/// A job that succeeds at once, which the test of an opening that is cancelled runs.
#[derive(Serialize, Deserialize)]
struct Noop;

impl Job for Noop {
    const KIND: &'static str = "tests.noop";
}

/// Waits until a session of `app` on the test database is idle inside a transaction, as the
/// administrator sees it: the state Postgres reaches once it has run a `BEGIN` of the API's.
async fn app_idles_in_a_transaction(db: &TestDatabase) {
    let sql = format!(
        "SELECT count(*)::text FROM pg_stat_activity \
         WHERE datname = '{}' AND usename = 'app' AND state = 'idle in transaction'",
        db.name()
    );
    while db.admin_reads(&sql).await == ["0"] {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn platform_107_after_an_opening_is_cancelled_a_statement_that_commits_by_itself_commits() {
    let db = TestDatabase::migrated().await;
    let proxy = Proxy::to(db.app_url().address()).await;
    // One connection, so the job's claim and its deletion run on the cancelled opening's.
    let database = Database::connect(&db.app_url().through(proxy.port()), 1, &app::MIGRATIONS)
        .await
        .unwrap();
    let mut jobs = Jobs::new(&database, Duration::from_secs(86_400));
    jobs.queued(|_, Noop| async { Ok(()) }).unwrap();
    let client = client();
    let mut uow = database.begin(&client, None).await.unwrap();
    uow.enqueue(JobScope::for_tests(), &Noop).await.unwrap();
    uow.commit().await.unwrap();
    // Postgres opens the transaction, and the opening is dropped before it hears so.
    proxy.hold_answers_from_the_next_begin();
    let mut opening = Box::pin(database.begin(&client, None));
    let cancelled = tokio::select! {
        _ = &mut opening => false,
        () = app_idles_in_a_transaction(&db) => true,
    };
    drop(opening);
    proxy.thaw();

    let clock: SharedClock = Arc::new(TestClock::default());
    jobs.run_pending(&database, &clock).await;

    assert!(
        cancelled,
        "the opening ended before Postgres's answer came back"
    );
    let left = db.owner_reads("SELECT count(*)::text FROM jobs").await;
    assert_eq!(left, ["0"]);
}

#[tokio::test]
async fn platform_007_serve_logs_the_most_connections_the_instance_opens() {
    let db = TestDatabase::migrated().await;

    let server = db
        .serve(&[
            ("ENVIRONMENT", "production"),
            ("DATABASE_MAX_CONNECTIONS", "7"),
        ])
        .await;

    let connected = server.started(|line| line["message"] == "connected to the database");
    assert_eq!(connected.len(), 1, "{connected:#?}");
    assert_eq!(connected[0]["level"], "INFO", "{}", connected[0]);
    assert_eq!(connected[0]["database.connections"], 8, "{}", connected[0]);
}

#[tokio::test]
async fn platform_001_serve_refuses_a_role_with_power_beyond_logging_in() {
    let db = TestDatabase::migrated().await;
    for (attribute, problem) in [
        ("SUPERUSER", RoleProblem::Superuser),
        ("CREATEDB", RoleProblem::CreatesDatabases),
        ("CREATEROLE", RoleProblem::CreatesRoles),
        ("REPLICATION", RoleProblem::Replicates),
        ("BYPASSRLS", RoleProblem::BypassesRowSecurity),
    ] {
        let (_, url) = db.role(attribute).await;

        assert_refused(&url, &problem).await;
    }
}

#[tokio::test]
async fn platform_001_serve_refuses_a_role_that_owns_objects() {
    let db = TestDatabase::migrated().await;
    let (role, url) = db.role("").await;
    db.admin_executes(&format!(
        "CREATE TABLE owned (id integer); ALTER TABLE owned OWNER TO {role}"
    ))
    .await;

    assert_refused(&url, &RoleProblem::OwnsObjects).await;
}

#[tokio::test]
async fn platform_001_serve_refuses_a_role_that_owns_objects_in_another_database() {
    let db = TestDatabase::migrated().await;
    let other = TestDatabase::empty().await;
    let (role, url) = db.role("").await;
    other
        .admin_executes(&format!(
            "CREATE TABLE owned (id integer); ALTER TABLE owned OWNER TO {role}"
        ))
        .await;

    assert_refused(&url, &RoleProblem::OwnsObjects).await;
}

#[tokio::test]
async fn platform_001_serve_refuses_a_role_that_belongs_to_another_role() {
    let db = TestDatabase::migrated().await;
    let (role, url) = db.role("").await;
    db.admin_executes(&format!("GRANT owner TO {role}")).await;

    assert_refused(&url, &RoleProblem::MemberOfRoles).await;
}

#[tokio::test]
async fn platform_001_the_refusal_names_every_way_the_role_breaks_the_rule() {
    let db = TestDatabase::migrated().await;
    let (role, url) = db.role("CREATEDB CREATEROLE").await;
    let expected = [
        RoleProblem::NotApp(role.clone()),
        RoleProblem::CreatesDatabases,
        RoleProblem::CreatesRoles,
    ];

    let refused = Database::connect(&url, 1, &app::MIGRATIONS).await.err();

    assert!(
        matches!(&refused, Some(ConnectError::UnsafeRole(problems)) if *problems == expected),
        "{refused:?}"
    );
    let message = refused.map(|error| error.to_string()).unwrap_or_default();
    assert_eq!(
        message,
        format!(
            "DATABASE_URL connects as a role that can't serve: it is {role}, not app; it can \
             create databases; it can create roles"
        )
    );
}

#[tokio::test]
async fn platform_001_serve_refuses_a_role_other_than_app() {
    let db = TestDatabase::migrated().await;
    let (role, url) = db.role("").await;

    let refused = Database::connect(&url, 1, &app::MIGRATIONS).await.err();

    assert!(
        matches!(&refused, Some(ConnectError::UnsafeRole(problems)) if *problems == [RoleProblem::NotApp(role)]),
        "{refused:?}"
    );
}

#[tokio::test]
async fn platform_002_after_bootstrap_the_roles_have_their_attributes_and_grants() {
    let db = TestDatabase::migrated().await;
    db.with_roles_locked(platform::db::bootstrap(
        &db.bootstrap_config(),
        &Workers::new(),
    ))
    .await
    .unwrap();

    let mut uow = db
        .app()
        .await
        .begin(&crate::support::client(), None)
        .await
        .unwrap();

    let roles = sqlx::query!(
        r#"
        SELECT rolname AS "name!", rolcanlogin AS "login!",
               rolsuper OR rolcreatedb OR rolcreaterole OR rolreplication OR rolbypassrls AS "more_power!"
        FROM pg_roles WHERE rolname IN ('owner', 'app') ORDER BY rolname
        "#
    )
    .fetch_all(uow.executor())
    .await
    .unwrap();
    let grants = sqlx::query!(
        r#"
        SELECT has_database_privilege('owner', current_database(), 'CONNECT') AS "owner_connects!",
               has_database_privilege('app', current_database(), 'CONNECT') AS "app_connects!",
               has_database_privilege('app', current_database(), 'TEMPORARY') AS "app_creates_temporary_tables!",
               has_schema_privilege('owner', 'public', 'CREATE') AS "owner_creates_objects!",
               has_schema_privilege('app', 'public', 'CREATE') AS "app_creates_objects!"
        "#
    )
    .fetch_one(uow.executor())
    .await
    .unwrap();

    let roles: Vec<_> = roles
        .into_iter()
        .map(|r| (r.name, r.login, r.more_power))
        .collect();
    assert_eq!(
        roles,
        [
            ("app".to_owned(), true, false),
            ("owner".to_owned(), true, false)
        ]
    );
    assert!(grants.owner_connects && grants.app_connects && grants.owner_creates_objects);
    assert!(!grants.app_creates_temporary_tables && !grants.app_creates_objects);
}

#[tokio::test]
async fn platform_002_bootstrap_refuses_urls_that_don_t_all_name_one_database() {
    let db = TestDatabase::migrated().await;
    // The administrator's URL reaches nothing, so a bootstrap that connected first would fail to
    // connect instead.
    let closed = closed_port();
    let config = db.bootstrap_config();
    let config = platform::config::BootstrapConfig {
        admin: config.admin.through(closed.port()),
        ..config
    };
    let other = format!("{}_other", db.name());
    let owner_elsewhere = platform::config::BootstrapConfig {
        owner: config.owner.with_database(&other),
        ..config.clone()
    };
    let app_elsewhere = platform::config::BootstrapConfig {
        app: config.app.with_database(&other),
        ..config
    };

    let refused = [
        platform::db::bootstrap(&owner_elsewhere, &Workers::new()).await,
        platform::db::bootstrap(&app_elsewhere, &Workers::new()).await,
    ];

    for refused in refused {
        assert!(
            matches!(refused, Err(BootstrapError::DifferentDatabases)),
            "{refused:?}"
        );
    }
}

#[tokio::test]
async fn platform_002_bootstrap_sets_the_app_roles_timeouts() {
    let db = TestDatabase::migrated().await;
    let app = db.app_url();

    let timeouts = db
        .with_roles_locked(async {
            db.admin_executes(
                "ALTER ROLE app RESET statement_timeout;
                 ALTER ROLE app RESET idle_in_transaction_session_timeout;",
            )
            .await;
            platform::db::bootstrap(&db.bootstrap_config(), &Workers::new())
                .await
                .unwrap();
            (
                db.setting(&app, "statement_timeout").await,
                db.setting(&app, "idle_in_transaction_session_timeout")
                    .await,
            )
        })
        .await;

    assert_eq!(timeouts, ("30s".to_owned(), "15s".to_owned()));
}

#[tokio::test]
async fn platform_002_running_bootstrap_again_keeps_the_roles_and_grants() {
    let db = TestDatabase::migrated().await;

    let (before, after) = db
        .with_roles_locked(async {
            let before = snapshot(&db).await;
            platform::db::bootstrap(&db.bootstrap_config(), &Workers::new())
                .await
                .unwrap();
            (before, snapshot(&db).await)
        })
        .await;

    assert_eq!(after, before);
    db.app().await;
}

#[tokio::test]
async fn platform_002_running_bootstrap_again_sets_each_password_from_its_url() {
    let db = TestDatabase::migrated().await;
    let verifiers = "SELECT rolname || ' ' || rolpassword FROM pg_authid \
                     WHERE rolname IN ('owner', 'app') ORDER BY rolname";

    let (before, after) = db
        .with_roles_locked(async {
            let before = db.admin_reads(verifiers).await;
            platform::db::bootstrap(&db.bootstrap_config(), &Workers::new())
                .await
                .unwrap();
            (before, db.admin_reads(verifiers).await)
        })
        .await;

    // Each verifier gets a new salt, so a password that was set again has a new verifier.
    assert_eq!(before.len(), 2);
    for (old, new) in before.iter().zip(&after) {
        assert_ne!(old, new);
    }
    db.app().await;
    assert_eq!(db.owner_reads("SELECT current_user::text").await, ["owner"]);
}

#[tokio::test]
async fn platform_003_in_production_bootstrap_refuses_a_server_with_another_database_of_its_roles()
{
    let db = TestDatabase::migrated().await;
    // Another database that bootstrap ran on, as another deployment's on the same server.
    let other = TestDatabase::migrated().await;
    let verifiers = "SELECT rolname || ' ' || rolpassword FROM pg_authid \
                     WHERE rolname IN ('owner', 'app') ORDER BY rolname";
    let production = platform::config::BootstrapConfig {
        environment: platform::config::Environment::Production,
        ..db.bootstrap_config()
    };

    let (before, refused, after) = db
        .with_roles_locked(async {
            let before = db.admin_reads(verifiers).await;
            let refused = platform::db::bootstrap(&production, &Workers::new()).await;
            (before, refused, db.admin_reads(verifiers).await)
        })
        .await;

    let named = match &refused {
        Err(BootstrapError::SharedServer(named)) => named.as_str(),
        _ => "",
    };
    assert!(!named.is_empty() && named != db.name(), "{refused:?}");
    // A database that neither role owns, so the refusal is for a privilege that they hold on it.
    let owner = db
        .admin_reads(&format!(
            "SELECT pg_get_userbyid(datdba)::text FROM pg_database WHERE datname = '{}'",
            named.replace('\'', "''")
        ))
        .await;
    assert!(
        owner.len() == 1 && owner[0] != "owner" && owner[0] != "app",
        "{named}: {owner:?}"
    );
    assert_eq!(after, before, "no password changed");
    drop(other);
}

#[tokio::test]
async fn platform_003_in_production_bootstrap_refuses_a_server_with_a_database_its_role_owns() {
    let db = TestDatabase::migrated().await;
    // A database that `owner` owns, with no grant on it, named to come first of the server's
    // databases, since bootstrap names the first by name, so the refusal names it: `name`'s
    // collation, C, orders names by their bytes, and U+0001, the lowest byte that a name may
    // hold, starts no other database's name that a server holds by its tasks or its tests.
    let owned = format!("\u{1}platform_003_owned_{}", std::process::id());
    let production = platform::config::BootstrapConfig {
        environment: platform::config::Environment::Production,
        ..db.bootstrap_config()
    };

    // The database is there only while the lock is held, so a test that holds it too, such as
    // the test of another database of the roles', never meets it.
    let refused = db
        .with_roles_locked(async {
            db.admin_executes(&format!("DROP DATABASE IF EXISTS \"{owned}\""))
                .await;
            db.admin_executes(&format!("CREATE DATABASE \"{owned}\" OWNER owner"))
                .await;
            let refused = platform::db::bootstrap(&production, &Workers::new()).await;
            db.admin_executes(&format!("DROP DATABASE \"{owned}\""))
                .await;
            refused
        })
        .await;

    assert!(
        matches!(&refused, Err(BootstrapError::SharedServer(named)) if *named == owned),
        "{refused:?}"
    );
}

#[tokio::test]
async fn data_20_the_app_role_runs_read_committed_whatever_the_database_default() {
    let db = TestDatabase::migrated().await;
    db.admin_executes(&format!(
        "ALTER DATABASE \"{}\" SET default_transaction_isolation = 'serializable'",
        db.name()
    ))
    .await;

    assert_eq!(
        db.setting(&db.app_url(), "transaction_isolation").await,
        "read committed"
    );
}

#[tokio::test]
async fn data_19_bootstrap_leaves_only_owner_able_to_create_objects_in_the_schema() {
    let db = TestDatabase::migrated().await;
    db.admin_executes("GRANT CREATE ON SCHEMA public TO PUBLIC")
        .await;

    db.with_roles_locked(platform::db::bootstrap(
        &db.bootstrap_config(),
        &Workers::new(),
    ))
    .await
    .unwrap();

    assert!(
        db.try_execute(&db.app_url(), "CREATE TABLE more (id integer)")
            .await
            .is_err(),
        "app must not create tables"
    );
}

#[tokio::test]
async fn obs_04_bootstrap_logs_no_password_verifier() {
    let db = TestDatabase::migrated().await;

    let output = db
        .with_roles_locked(db.run_api(
            &["bootstrap"],
            &[("ENVIRONMENT", "test"), ("LOG_LEVEL", "trace")],
        ))
        .await;

    let logs = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{logs}");
    assert!(logs.contains("bootstrap finished"), "{logs}");
    assert!(!logs.contains("SCRAM-SHA-256"), "{logs}");
}

/// The roles' attributes and settings, and the grants on the database and its schema, sorted.
async fn snapshot(db: &TestDatabase) -> Vec<String> {
    let mut uow = db
        .app()
        .await
        .begin(&crate::support::client(), None)
        .await
        .unwrap();
    let mut lines = sqlx::query_scalar!(
        r#"
        SELECT concat_ws(' ', r.rolname, r.rolcanlogin, r.rolsuper, r.rolinherit, r.rolcreaterole,
                         r.rolcreatedb, r.rolreplication, r.rolbypassrls, r.rolconnlimit,
                         s.setconfig::text) AS "line!"
        FROM pg_roles r
        LEFT JOIN pg_db_role_setting s ON s.setrole = r.oid AND s.setdatabase = 0
        WHERE r.rolname IN ('owner', 'app')
        UNION ALL
        SELECT datacl::text FROM pg_database WHERE datname = current_database()
        UNION ALL
        SELECT nspacl::text FROM pg_namespace WHERE nspname = 'public'
        "#
    )
    .fetch_all(uow.executor())
    .await
    .unwrap();
    lines.sort();
    lines
}

#[tokio::test]
async fn data_19_the_app_role_reads_and_writes_rows_and_changes_nothing_else() {
    let db = TestDatabase::migrated().await;
    db.owner_executes(
        "CREATE TABLE notes (id uuid PRIMARY KEY DEFAULT uuidv7(), body text NOT NULL);
         CREATE FUNCTION answer() RETURNS integer LANGUAGE sql AS 'SELECT 42';",
    )
    .await;
    let app = db.app_url();

    for allowed in [
        "INSERT INTO notes (body) VALUES ('first')",
        "SELECT body FROM notes",
        "UPDATE notes SET body = 'second'",
        "DELETE FROM notes",
        "SELECT version FROM _sqlx_migrations",
    ] {
        assert_eq!(db.try_execute(&app, allowed).await, Ok(()), "{allowed}");
    }
    for refused in [
        "TRUNCATE notes",
        "DROP TABLE notes",
        "ALTER TABLE notes ADD COLUMN extra text",
        "CREATE TABLE more (id integer)",
        "CREATE TEMPORARY TABLE scratch (id integer)",
        "SELECT answer()",
        "DELETE FROM _sqlx_migrations",
    ] {
        assert!(
            db.try_execute(&app, refused).await.is_err(),
            "{refused} must be refused"
        );
    }
    // platform_002_bootstrap_sets_the_app_roles_timeouts resets these under the roles lock.
    let timeouts = db
        .with_roles_locked(async {
            (
                db.setting(&app, "statement_timeout").await,
                db.setting(&app, "idle_in_transaction_session_timeout")
                    .await,
            )
        })
        .await;
    assert_eq!(timeouts, ("30s".to_owned(), "15s".to_owned()));
}

#[tokio::test]
async fn arch_11_a_unit_of_work_commits_all_its_writes_or_none() {
    let db = TestDatabase::migrated().await;
    db.owner_executes("CREATE TABLE notes (body text NOT NULL)")
        .await;
    let app = db.app().await;

    let mut committed = app.begin(&crate::support::client(), None).await.unwrap();
    execute_on(committed.executor(), "INSERT INTO notes VALUES ('kept')").await;
    execute_on(
        committed.executor(),
        "INSERT INTO notes VALUES ('also kept')",
    )
    .await;
    committed.commit().await.unwrap();
    let mut dropped = app.begin(&crate::support::client(), None).await.unwrap();
    execute_on(dropped.executor(), "INSERT INTO notes VALUES ('lost')").await;
    drop(dropped);

    assert_eq!(
        db.owner_reads("SELECT body FROM notes ORDER BY body").await,
        ["also kept", "kept"]
    );
}

#[tokio::test]
async fn ops_21_a_run_of_the_tests_drops_the_databases_and_roles_that_an_earlier_run_left() {
    let db = TestDatabase::empty().await;
    let prefix = db.name().split("_test_").next().unwrap().to_owned();
    // What a test process that runs no more left: no process has this ID, so none holds the
    // lock that it names.
    let database = format!("{prefix}_test_2147483647_0");
    let role = format!("{prefix}_role_2147483647_0");
    db.admin_executes(&format!("CREATE ROLE \"{role}\"")).await;
    db.admin_executes(&format!("CREATE DATABASE \"{database}\" OWNER \"{role}\""))
        .await;
    // part: files
    // A bucket of that process's whose database has gone, as a process built without files
    // leaves it once it has dropped the database.
    let orphan = bucket::name(&format!("{prefix}_test_2147483647_1"));
    bucket::create(&orphan).await;
    // end part: files

    db.drop_what_earlier_runs_left().await;

    let left = db
        .admin_reads(&format!(
            "SELECT datname::text FROM pg_database WHERE datname IN ('{database}', '{}') \
             UNION ALL SELECT rolname::text FROM pg_roles WHERE rolname = '{role}'",
            db.name()
        ))
        .await;
    // The database of a test that runs stays.
    assert_eq!(left, [db.name()]);
    // part: files
    let buckets = bucket::named(&bucket::name(&format!("{prefix}_test_2147483647_"))).await;
    assert!(buckets.is_empty(), "{buckets:?}");
    // end part: files
}

#[tokio::test]
async fn ops_21_a_test_waits_to_create_its_database_while_too_many_wait_to_be_dropped() {
    let backlog = Backlog::new();
    for _ in 0..=Backlog::MOST {
        backlog.left();
    }
    let mut room = std::pin::pin!(backlog.room());

    assert!(room.as_mut().now_or_never().is_none());

    backlog.dropped();
    assert!(room.now_or_never().is_some());
}

/// One database more than leaves room among those yet to be dropped, however many the cleanup
/// drops meanwhile, counted until this goes, as it does when its test fails too.
struct Full(&'static Backlog);

impl Full {
    fn of(backlog: &'static Backlog) -> Self {
        for _ in 0..=Backlog::MOST {
            backlog.left();
        }
        Self(backlog)
    }
}

impl Drop for Full {
    fn drop(&mut self) {
        for _ in 0..=Backlog::MOST {
            self.0.dropped();
        }
    }
}

#[tokio::test]
async fn ops_21_a_test_s_database_is_created_once_the_ones_yet_to_be_dropped_leave_room() {
    let backlog = Backlog::of_the_process();
    let full = Full::of(backlog);
    let created = tokio::spawn(TestDatabase::empty());

    while backlog.waiting() == 0 {
        tokio::task::yield_now().await;
    }
    assert!(!created.is_finished());

    drop(full);
    created.await.unwrap();
}
