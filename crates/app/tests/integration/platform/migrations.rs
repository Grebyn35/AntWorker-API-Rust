use std::path::Path;

use platform::db::{ConnectError, Database, MigrateError, SchemaProblem, Versions, migrate};
use sqlx::migrate::Migrator;

use crate::support::{self, TestDatabase, execute_on};

/// Two migrations, each creating one table.
static TWO_TABLES: Migrator = sqlx::migrate!("tests/integration/migrations/two_tables");
/// The first of `TWO_TABLES`, as an earlier release would hold it.
static FIRST_TABLE: Migrator = sqlx::migrate!("tests/integration/migrations/first_table");
/// `TWO_TABLES`, with the first migration edited after it was applied.
static EDITED_THEN_SECOND: Migrator =
    sqlx::migrate!("tests/integration/migrations/edited_then_second");
/// The first of `TWO_TABLES`, then a migration that fails after creating a table.
static SECOND_FAILS: Migrator = sqlx::migrate!("tests/integration/migrations/second_fails");
/// One migration, marked to run outside a transaction, that creates a table.
static NO_TRANSACTION: Migrator = sqlx::migrate!("tests/integration/migrations/no_transaction");
/// A table, then a migration marked to run outside a transaction that creates another.
static NO_TRANSACTION_SECOND: Migrator =
    sqlx::migrate!("tests/integration/migrations/no_transaction_second");
/// A table, then a migration that builds an index of it outside a transaction.
static INDEX_OUTSIDE: Migrator = sqlx::migrate!("tests/integration/migrations/index_outside");
/// A table that holds one name twice, then a migration that builds a unique index of the names
/// outside a transaction.
static INDEX_FAILS: Migrator = sqlx::migrate!("tests/integration/migrations/index_fails");

/// Whether the index `name` can be used, as `true` or `false`, or nothing when there is none.
async fn usable(db: &TestDatabase, name: &str) -> Vec<String> {
    db.owner_reads(&format!(
        "SELECT indisvalid::text FROM pg_index WHERE indexrelid = to_regclass('{name}')"
    ))
    .await
}

fn owned_by_owner(tables: &[&str]) -> Vec<(String, String)> {
    tables
        .iter()
        .map(|table| ((*table).to_owned(), "owner".to_owned()))
        .collect()
}

#[tokio::test]
async fn platform_004_migrate_applies_every_migration_in_order_as_the_owner() {
    let db = TestDatabase::empty().await;

    let applied = migrate(&db.owner_url(), &TWO_TABLES).await.unwrap();

    assert_eq!(applied, [1, 2]);
    assert_eq!(db.applied_versions().await, [1, 2]);
    assert_eq!(db.tables().await, owned_by_owner(&["first", "second"]));
}

#[tokio::test]
async fn platform_004_running_migrate_again_applies_nothing() {
    let db = TestDatabase::empty().await;
    migrate(&db.owner_url(), &TWO_TABLES).await.unwrap();

    let applied = migrate(&db.owner_url(), &TWO_TABLES).await.unwrap();

    assert_eq!(applied, Vec::<i64>::new());
    assert_eq!(db.applied_versions().await, [1, 2]);
}

#[tokio::test]
async fn platform_004_concurrent_runs_apply_each_migration_once() {
    let db = TestDatabase::empty().await;
    let url = db.owner_url();

    let (first, second) = tokio::join!(migrate(&url, &TWO_TABLES), migrate(&url, &TWO_TABLES));

    let mut applied = [first.unwrap(), second.unwrap()].concat();
    applied.sort_unstable();
    assert_eq!(applied, [1, 2]);
    assert_eq!(db.applied_versions().await, [1, 2]);
}

/// The key of the advisory lock that `migrate` takes on the database, as sqlx-postgres derives
/// it from the database's name: 1026731422 times the name's CRC-32.
const LOCK: &str = "1026731422 * crc32(convert_to(current_database(), 'UTF8'))";

#[tokio::test]
async fn platform_004_concurrent_runs_apply_a_migration_outside_a_transaction_once() {
    let db = TestDatabase::empty().await;
    let url = db.owner_url();
    // The test holds the lock until both runs wait for it, so that one run builds the index
    // while the other waits.
    let mut held = db.owner_connection().await;
    execute_on(&mut held, &format!("SELECT pg_advisory_lock({LOCK})")).await;
    let release = async {
        support::queries_wait_on_locks(&db, 2).await;
        execute_on(&mut held, &format!("SELECT pg_advisory_unlock({LOCK})")).await;
    };

    let (first, second, ()) = tokio::join!(
        migrate(&url, &INDEX_OUTSIDE),
        migrate(&url, &INDEX_OUTSIDE),
        release
    );

    let mut applied = [first.unwrap(), second.unwrap()].concat();
    applied.sort_unstable();
    assert_eq!(applied, [1, 2]);
    assert_eq!(db.applied_versions().await, [1, 2]);
    assert_eq!(usable(&db, "things_name_idx").await, ["true"]);
}

#[tokio::test]
async fn platform_004_a_wait_for_another_run_s_lock_that_is_cancelled_fails_migrate() {
    let db = TestDatabase::empty().await;
    // Each try then waits half an hour, so a try that waits again is one that the cancellation
    // didn't end.
    db.admin_executes(&format!(
        "ALTER DATABASE {} SET deadlock_timeout = '1h'",
        db.name()
    ))
    .await;
    let mut held = db.owner_connection().await;
    execute_on(&mut held, &format!("SELECT pg_advisory_lock({LOCK})")).await;
    let url = db.owner_url();
    let waiting = format!(
        "SELECT pid::text || ' ' || query_start::text FROM pg_stat_activity \
         WHERE datname = '{}' AND wait_event_type = 'Lock' AND query LIKE '%pg_advisory_lock%'",
        db.name()
    );

    let migrating = tokio::spawn(async move { migrate(&url, &TWO_TABLES).await });
    support::query_waits_on_a_lock(&db, "pg_advisory_lock").await;
    let first = db.admin_reads(&waiting).await.remove(0);
    let (pid, started) = first.split_once(' ').unwrap();
    db.admin_executes(&format!("SELECT pg_cancel_backend({pid})"))
        .await;
    let again = format!(
        "SELECT count(*)::text FROM pg_stat_activity WHERE pid = {pid} \
         AND wait_event_type = 'Lock' AND query_start > '{started}'::timestamptz"
    );
    let waited_again = loop {
        if migrating.is_finished() {
            break false;
        }
        if db.admin_reads(&again).await != ["0"] {
            break true;
        }
        tokio::task::yield_now().await;
    };
    let refused = if waited_again {
        None
    } else {
        migrating.await.unwrap().err()
    };

    assert!(!waited_again, "the run waited for the lock again");
    assert!(
        matches!(refused, Some(MigrateError::Failed(_))),
        "{refused:?}"
    );
    assert_eq!(db.tables().await, owned_by_owner(&[]));
}

#[tokio::test]
async fn platform_004_migrate_refuses_a_role_other_than_owner() {
    let db = TestDatabase::empty().await;

    let refused = migrate(&db.app_url(), &TWO_TABLES).await.err();

    assert!(
        matches!(&refused, Some(MigrateError::NotOwner(role)) if role == "app"),
        "{refused:?}"
    );
    assert_eq!(db.tables().await, owned_by_owner(&[]));
}

#[tokio::test]
async fn platform_005_a_failing_migration_leaves_no_trace() {
    let db = TestDatabase::empty().await;

    let failed = migrate(&db.owner_url(), &SECOND_FAILS).await.err();

    assert!(
        matches!(failed, Some(MigrateError::Failed(_))),
        "{failed:?}"
    );
    assert_eq!(db.applied_versions().await, [1]);
    assert_eq!(db.tables().await, owned_by_owner(&["first"]));
}

#[tokio::test]
async fn platform_005_migrate_refuses_a_migration_outside_a_transaction_that_builds_no_index() {
    let db = TestDatabase::empty().await;

    let refused = migrate(&db.owner_url(), &NO_TRANSACTION).await.err();

    assert!(
        matches!(&refused, Some(MigrateError::OutsideTransaction(Versions(versions))) if *versions == [1]),
        "{refused:?}"
    );
    assert_eq!(db.tables().await, owned_by_owner(&[]));
}

#[tokio::test]
async fn platform_005_migrate_applies_nothing_while_it_holds_a_marked_migration_that_builds_no_index()
 {
    let db = TestDatabase::empty().await;

    let refused = migrate(&db.owner_url(), &NO_TRANSACTION_SECOND).await.err();

    assert!(
        matches!(&refused, Some(MigrateError::OutsideTransaction(Versions(versions))) if *versions == [2]),
        "{refused:?}"
    );
    // The migration before the marked one, which a transaction would hold, stays unapplied too.
    assert_eq!(db.tables().await, owned_by_owner(&[]));
}

#[tokio::test]
async fn platform_005_an_index_built_outside_a_transaction_is_recorded_once_it_is_built() {
    let db = TestDatabase::empty().await;

    let applied = migrate(&db.owner_url(), &INDEX_OUTSIDE).await.unwrap();
    let again = migrate(&db.owner_url(), &INDEX_OUTSIDE).await.unwrap();

    assert_eq!(applied, [1, 2]);
    assert_eq!(again, Vec::<i64>::new());
    assert_eq!(db.applied_versions().await, [1, 2]);
    assert_eq!(usable(&db, "things_name_idx").await, ["true"]);
}

#[tokio::test]
async fn platform_005_an_index_that_was_built_and_not_recorded_is_recorded_by_the_next_run() {
    let db = TestDatabase::empty().await;
    migrate(&db.owner_url(), &INDEX_OUTSIDE).await.unwrap();
    // The record is written after the build, outside its transaction, so a run that ends
    // between the two leaves the index without it.
    db.owner_executes("DELETE FROM _sqlx_migrations WHERE version = 2")
        .await;

    let applied = migrate(&db.owner_url(), &INDEX_OUTSIDE).await.unwrap();

    assert_eq!(applied, [2]);
    assert_eq!(db.applied_versions().await, [1, 2]);
    assert_eq!(usable(&db, "things_name_idx").await, ["true"]);
}

#[tokio::test]
async fn platform_005_a_failed_build_leaves_an_unusable_index_that_the_next_run_builds_again() {
    let db = TestDatabase::empty().await;

    let failed = migrate(&db.owner_url(), &INDEX_FAILS).await.err();

    // The build's failure leaves the index, unusable, and no record of its migration.
    assert!(
        matches!(failed, Some(MigrateError::Failed(_))),
        "{failed:?}"
    );
    assert_eq!(db.applied_versions().await, [1]);
    assert_eq!(usable(&db, "things_name_key").await, ["false"]);

    // Once nothing keeps the index from building, the next run drops what the first left,
    // rather than take it for built, and builds it.
    db.owner_executes("DELETE FROM things WHERE id = 2").await;
    let applied = migrate(&db.owner_url(), &INDEX_FAILS).await.unwrap();

    assert_eq!(applied, [2]);
    assert_eq!(db.applied_versions().await, [1, 2]);
    assert_eq!(usable(&db, "things_name_key").await, ["true"]);
}

#[tokio::test]
async fn platform_006_migrate_applies_nothing_while_an_applied_migration_differs_from_its_file() {
    let db = TestDatabase::empty().await;
    migrate(&db.owner_url(), &FIRST_TABLE).await.unwrap();

    let refused = migrate(&db.owner_url(), &EDITED_THEN_SECOND).await.err();

    assert!(
        matches!(&refused, Some(MigrateError::Modified(Versions(versions))) if *versions == [1]),
        "{refused:?}"
    );
    assert_eq!(db.applied_versions().await, [1]);
    assert_eq!(db.tables().await, owned_by_owner(&["first"]));
}

#[tokio::test]
async fn platform_006_an_earlier_release_ignores_migrations_it_doesnt_contain() {
    let db = TestDatabase::empty().await;
    migrate(&db.owner_url(), &TWO_TABLES).await.unwrap();

    let applied = migrate(&db.owner_url(), &FIRST_TABLE).await.unwrap();

    assert_eq!(applied, Vec::<i64>::new());
}

#[tokio::test]
async fn platform_030_serve_refuses_a_database_that_was_never_migrated() {
    let db = TestDatabase::empty().await;

    let refused = Database::connect(&db.app_url(), 1, &TWO_TABLES).await.err();

    assert!(
        matches!(&refused, Some(ConnectError::Schema(SchemaProblem::Missing(Versions(versions)))) if *versions == [1, 2]),
        "{refused:?}"
    );
}

#[tokio::test]
async fn platform_030_serve_refuses_a_database_that_lacks_a_migration() {
    let db = TestDatabase::empty().await;
    migrate(&db.owner_url(), &FIRST_TABLE).await.unwrap();

    let refused = Database::connect(&db.app_url(), 1, &TWO_TABLES).await.err();

    assert!(
        matches!(&refused, Some(ConnectError::Schema(SchemaProblem::Missing(Versions(versions)))) if *versions == [2]),
        "{refused:?}"
    );
}

#[tokio::test]
async fn platform_030_serve_refuses_a_migration_applied_with_different_content() {
    let db = TestDatabase::empty().await;
    migrate(&db.owner_url(), &TWO_TABLES).await.unwrap();

    let refused = Database::connect(&db.app_url(), 1, &EDITED_THEN_SECOND)
        .await
        .err();

    assert!(
        matches!(&refused, Some(ConnectError::Schema(SchemaProblem::Modified(Versions(versions)))) if *versions == [1]),
        "{refused:?}"
    );
}

#[tokio::test]
async fn platform_030_serve_starts_against_a_schema_from_a_later_release() {
    let db = TestDatabase::empty().await;
    migrate(&db.owner_url(), &TWO_TABLES).await.unwrap();

    let connected = Database::connect(&db.app_url(), 1, &FIRST_TABLE).await;

    assert!(connected.is_ok(), "{:?}", connected.err());
}

#[tokio::test]
async fn platform_030_serve_starts_against_the_api_migrations() {
    let db = TestDatabase::migrated().await;

    let connected = Database::connect(&db.app_url(), 1, &app::MIGRATIONS).await;

    assert!(connected.is_ok(), "{:?}", connected.err());
}

/// The variable in which `just test` names the directory that holds the migrations of the
/// newest release and nothing else.
const RELEASE_MIGRATIONS: &str = "RELEASE_MIGRATIONS";

/// A database's schema as its catalog holds it, in lines that it orders. Of the schemas that
/// aren't Postgres's own, a line holds: a table, view or sequence with its owner, its row
/// security, its privileges, its options, whether it is logged, the identity that replication
/// reads, and its partitions' key and bound; a column with its place among its table's, its
/// type, whether it is `NOT NULL`, its default, its identity, how it is generated, its
/// privileges, its collation, how it is stored and compressed, and its statistics target; a
/// constraint and a view as Postgres writes its definition; an index with whether it is valid,
/// and a trigger with whether it is enabled, each as Postgres writes its definition; a function
/// or procedure with its owner, its privileges and its definition; an enum, domain or range
/// type; a collation; and the schema with its owner and privileges. Of the whole database, a
/// line holds: a policy, a rule, a sequence's bounds, the statistics that a migration asks for,
/// the default privileges, an extension with its version, and a comment on what isn't
/// Postgres's own.
const SCHEMA: &str = r"
WITH own AS (
  SELECT oid FROM pg_namespace WHERE nspname <> 'information_schema' AND nspname NOT LIKE 'pg\_%'
)
SELECT line FROM (
  SELECT format('relation %s kind=%s owner=%s row_security=%s forced=%s privileges=%s options=%s '
        || 'persistence=%s replica_identity=%s partition_key=%s partition_bound=%s',
      c.oid::regclass, c.relkind, pg_get_userbyid(c.relowner), c.relrowsecurity,
      c.relforcerowsecurity, c.relacl, c.reloptions, c.relpersistence, c.relreplident,
      pg_get_partkeydef(c.oid), pg_get_expr(c.relpartbound, c.oid)) AS line
    FROM pg_class c
    WHERE c.relnamespace IN (SELECT oid FROM own) AND c.relkind IN ('r', 'p', 'v', 'm', 'S')
  UNION ALL
  SELECT format('column %s %s %s type=%s not_null=%s default=%s identity=%s generated=%s '
        || 'privileges=%s collation=%s storage=%s compression=%s statistics=%s',
      a.attrelid::regclass, row_number() OVER (PARTITION BY a.attrelid ORDER BY a.attnum),
      a.attname, format_type(a.atttypid, a.atttypmod), a.attnotnull,
      pg_get_expr(d.adbin, d.adrelid), a.attidentity, a.attgenerated, a.attacl,
      a.attcollation::regcollation, a.attstorage, a.attcompression, a.attstattarget)
    FROM pg_attribute a
    JOIN pg_class c ON c.oid = a.attrelid
    LEFT JOIN pg_attrdef d ON d.adrelid = a.attrelid AND d.adnum = a.attnum
    WHERE c.relnamespace IN (SELECT oid FROM own) AND c.relkind IN ('r', 'p', 'v', 'm')
      AND a.attnum > 0 AND NOT a.attisdropped
  UNION ALL
  SELECT format('constraint %s on %s: %s',
      con.conname, con.conrelid::regclass, pg_get_constraintdef(con.oid))
    FROM pg_constraint con WHERE con.connamespace IN (SELECT oid FROM own)
  UNION ALL
  SELECT format('index %s usable=%s: %s',
      i.indexrelid::regclass, i.indisvalid, pg_get_indexdef(i.indexrelid))
    FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid
    WHERE c.relnamespace IN (SELECT oid FROM own)
  UNION ALL
  SELECT format('trigger %s on %s enabled=%s: %s',
      t.tgname, t.tgrelid::regclass, t.tgenabled, pg_get_triggerdef(t.oid))
    FROM pg_trigger t JOIN pg_class c ON c.oid = t.tgrelid
    WHERE NOT t.tgisinternal AND c.relnamespace IN (SELECT oid FROM own)
  UNION ALL
  SELECT format('view %s: %s', c.oid::regclass, pg_get_viewdef(c.oid))
    FROM pg_class c
    WHERE c.relnamespace IN (SELECT oid FROM own) AND c.relkind IN ('v', 'm')
  UNION ALL
  SELECT format('function %s owner=%s privileges=%s: %s',
      p.oid::regprocedure, pg_get_userbyid(p.proowner), p.proacl, pg_get_functiondef(p.oid))
    FROM pg_proc p
    WHERE p.pronamespace IN (SELECT oid FROM own) AND p.prokind IN ('f', 'p')
  UNION ALL
  SELECT format('policy %s on %s.%s permissive=%s command=%s roles=%s using=%s check=%s',
      policyname, schemaname, tablename, permissive, cmd, roles, qual, with_check)
    FROM pg_policies
  UNION ALL
  SELECT format('type %s kind=%s owner=%s labels=%s base=%s',
      t.oid::regtype, t.typtype, pg_get_userbyid(t.typowner),
      (SELECT array_agg(e.enumlabel ORDER BY e.enumsortorder) FROM pg_enum e
       WHERE e.enumtypid = t.oid),
      format_type(t.typbasetype, t.typtypmod))
    FROM pg_type t
    WHERE t.typnamespace IN (SELECT oid FROM own) AND t.typtype IN ('e', 'd', 'r')
  UNION ALL
  SELECT format('sequence %s.%s type=%s start=%s min=%s max=%s step=%s cycles=%s',
      schemaname, sequencename, data_type, start_value, min_value, max_value, increment_by,
      cycle)
    FROM pg_sequences
  UNION ALL
  SELECT format('schema %s owner=%s privileges=%s',
      n.nspname, pg_get_userbyid(n.nspowner), n.nspacl)
    FROM pg_namespace n WHERE n.oid IN (SELECT oid FROM own)
  UNION ALL
  SELECT format('default privileges of %s in %s for %s: %s',
      pg_get_userbyid(d.defaclrole), d.defaclnamespace::regnamespace, d.defaclobjtype,
      d.defaclacl)
    FROM pg_default_acl d
  UNION ALL
  SELECT format('extension %s %s', extname, extversion) FROM pg_extension
  UNION ALL
  SELECT format('rule %s on %s.%s: %s', rulename, schemaname, tablename, definition)
    FROM pg_rules
  UNION ALL
  SELECT format('statistics %s', pg_get_statisticsobjdef(s.oid)) FROM pg_statistic_ext s
  UNION ALL
  SELECT format('collation %s provider=%s locale=%s deterministic=%s',
      c.oid::regcollation, c.collprovider, coalesce(c.colllocale, c.collcollate),
      c.collisdeterministic)
    FROM pg_collation c WHERE c.collnamespace IN (SELECT oid FROM own)
  UNION ALL
  -- Postgres numbers its own objects below 16384.
  SELECT format('comment on %s: %s',
      pg_describe_object(d.classoid, d.objoid, d.objsubid), d.description)
    FROM pg_description d WHERE d.objoid >= 16384
) lines
ORDER BY line
";

#[tokio::test]
async fn data_11_the_migrations_build_on_the_newest_release_s_schema_what_they_build_on_an_empty_database()
 {
    let directory = std::env::var_os(RELEASE_MIGRATIONS)
        .expect("`just test` names the directory of the release's migrations");
    let release = Migrator::new(Path::new(&directory))
        .await
        .expect("read the release's migrations");
    let upgraded = TestDatabase::empty().await;
    let earlier = migrate(&upgraded.owner_url(), &release).await.unwrap();

    let new = migrate(&upgraded.owner_url(), &app::MIGRATIONS)
        .await
        .unwrap();

    // The template of `fresh` is an empty database to which `migrate` applied every migration.
    let fresh = TestDatabase::migrated().await;
    // A new migration may come before one of the release's, as one that a fork ports does.
    let mut applied = [earlier, new].concat();
    applied.sort_unstable();
    let every: Vec<i64> = app::MIGRATIONS
        .iter()
        .map(|migration| migration.version)
        .collect();
    assert_eq!(applied, every);
    let (upgraded, fresh) = (
        upgraded.owner_reads(SCHEMA).await,
        fresh.owner_reads(SCHEMA).await,
    );
    assert!(fresh.iter().any(|line| line.starts_with("relation users ")));
    let only = |of: &[String], other: &[String]| -> Vec<String> {
        of.iter()
            .filter(|line| !other.contains(line))
            .cloned()
            .collect()
    };
    assert_eq!(
        (only(&upgraded, &fresh), only(&fresh, &upgraded)),
        (Vec::new(), Vec::new()),
        "the lines of the upgraded database's schema alone, then those of the new one's alone"
    );
    assert_eq!(upgraded, fresh);
}
