use platform::db::{ConnectError, Database, MigrateError, SchemaProblem, Versions, migrate};
use sqlx::migrate::Migrator;

use crate::support::TestDatabase;

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
