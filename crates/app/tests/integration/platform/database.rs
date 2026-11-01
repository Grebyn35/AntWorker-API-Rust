use platform::db::{ConnectError, Database, DatabaseUrl, RoleProblem};

use crate::support::{TestDatabase, execute_on};

/// Asserts that `serve` refuses to start as the role `url` connects as, for `problem`.
async fn assert_refused(url: &DatabaseUrl, problem: &RoleProblem) {
    let refused = Database::connect(url, 1, &app::MIGRATIONS).await.err();
    assert!(
        matches!(&refused, Some(ConnectError::UnsafeRole(problems)) if problems.contains(problem)),
        "expected {problem:?}, got {refused:?}"
    );
}

#[tokio::test]
async fn test_06_the_api_under_test_connects_as_the_app_role() {
    let db = TestDatabase::migrated().await;

    let app = db.app().await;

    let mut uow = app.begin(&crate::support::client()).await.unwrap();
    let role = sqlx::query_scalar!(r#"SELECT current_user AS "role!""#)
        .fetch_one(uow.executor())
        .await
        .unwrap();
    assert_eq!(role, "app");
}

#[tokio::test]
async fn platform_045_serve_logs_the_most_connections_the_instance_opens() {
    let db = TestDatabase::migrated().await;

    let server = db
        .serve(&[
            ("ENVIRONMENT", "production"),
            ("DATABASE_MAX_CONNECTIONS", "7"),
        ])
        .await;

    let connected = server
        .started(|line| line["message"] == "connected to the database")
        .expect("the line that gives the connections");
    assert_eq!(connected["level"], "INFO", "{connected}");
    assert_eq!(connected["database.connections"], 8, "{connected}");
}

#[tokio::test]
async fn platform_009_serve_refuses_a_role_with_power_beyond_logging_in() {
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
async fn platform_009_serve_refuses_a_role_that_owns_objects() {
    let db = TestDatabase::migrated().await;
    let (role, url) = db.role("").await;
    db.admin_executes(&format!(
        "CREATE TABLE owned (id integer); ALTER TABLE owned OWNER TO {role}"
    ))
    .await;

    assert_refused(&url, &RoleProblem::OwnsObjects).await;
}

#[tokio::test]
async fn platform_009_serve_refuses_a_role_that_owns_objects_in_another_database() {
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
async fn platform_009_serve_refuses_a_role_that_belongs_to_another_role() {
    let db = TestDatabase::migrated().await;
    let (role, url) = db.role("").await;
    db.admin_executes(&format!("GRANT owner TO {role}")).await;

    assert_refused(&url, &RoleProblem::MemberOfRoles).await;
}

#[tokio::test]
async fn platform_009_the_refusal_names_every_way_the_role_breaks_the_rule() {
    let db = TestDatabase::migrated().await;
    let (role, url) = db.role("CREATEDB CREATEROLE").await;
    let expected = [
        RoleProblem::NotApp(role),
        RoleProblem::CreatesDatabases,
        RoleProblem::CreatesRoles,
    ];

    let refused = Database::connect(&url, 1, &app::MIGRATIONS).await.err();

    assert!(
        matches!(&refused, Some(ConnectError::UnsafeRole(problems)) if *problems == expected),
        "{refused:?}"
    );
    let message = refused.map(|error| error.to_string()).unwrap_or_default();
    for problem in &expected {
        assert!(message.contains(&problem.to_string()), "{message}");
    }
}

#[tokio::test]
async fn platform_009_serve_refuses_a_role_other_than_app() {
    let db = TestDatabase::migrated().await;
    let (role, url) = db.role("").await;

    let refused = Database::connect(&url, 1, &app::MIGRATIONS).await.err();

    assert!(
        matches!(&refused, Some(ConnectError::UnsafeRole(problems)) if *problems == [RoleProblem::NotApp(role)]),
        "{refused:?}"
    );
}

#[tokio::test]
async fn platform_010_after_bootstrap_the_roles_have_their_attributes_and_grants() {
    let db = TestDatabase::migrated().await;
    db.with_roles_locked(platform::db::bootstrap(&db.bootstrap_config()))
        .await
        .unwrap();

    let mut uow = db
        .app()
        .await
        .begin(&crate::support::client())
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
async fn platform_010_bootstrap_sets_the_app_roles_timeouts() {
    let db = TestDatabase::migrated().await;
    let app = db.app_url();

    let timeouts = db
        .with_roles_locked(async {
            db.admin_executes(
                "ALTER ROLE app RESET statement_timeout;
                 ALTER ROLE app RESET idle_in_transaction_session_timeout;",
            )
            .await;
            platform::db::bootstrap(&db.bootstrap_config())
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
async fn platform_010_running_bootstrap_again_keeps_the_roles_and_grants() {
    let db = TestDatabase::migrated().await;

    let (before, after) = db
        .with_roles_locked(async {
            let before = snapshot(&db).await;
            platform::db::bootstrap(&db.bootstrap_config())
                .await
                .unwrap();
            (before, snapshot(&db).await)
        })
        .await;

    assert_eq!(after, before);
    db.app().await;
}

#[tokio::test]
async fn platform_010_running_bootstrap_again_sets_each_password_from_its_url() {
    let db = TestDatabase::migrated().await;
    let verifiers = "SELECT rolname || ' ' || rolpassword FROM pg_authid \
                     WHERE rolname IN ('owner', 'app') ORDER BY rolname";

    let (before, after) = db
        .with_roles_locked(async {
            let before = db.admin_reads(verifiers).await;
            platform::db::bootstrap(&db.bootstrap_config())
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
async fn data_25_the_app_role_runs_read_committed_whatever_the_database_default() {
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

    db.with_roles_locked(platform::db::bootstrap(&db.bootstrap_config()))
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
        .begin(&crate::support::client())
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
    // platform_010_bootstrap_sets_the_app_roles_timeouts resets these under the roles lock.
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

    let mut committed = app.begin(&crate::support::client()).await.unwrap();
    execute_on(committed.executor(), "INSERT INTO notes VALUES ('kept')").await;
    execute_on(
        committed.executor(),
        "INSERT INTO notes VALUES ('also kept')",
    )
    .await;
    committed.commit().await.unwrap();
    let mut dropped = app.begin(&crate::support::client()).await.unwrap();
    execute_on(dropped.executor(), "INSERT INTO notes VALUES ('lost')").await;
    drop(dropped);

    assert_eq!(
        db.owner_reads("SELECT body FROM notes ORDER BY body").await,
        ["also kept", "kept"]
    );
}
