//! The health probes (OPS-05).

use std::time::Duration;

use http::StatusCode;
use serde_json::json;

use crate::support::{Api, TestDatabase};

/// How often the instance checks its own database connection.
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

#[tokio::test]
async fn platform_019_the_live_probe_answers_alive() {
    let api = Api::new();

    let response = api.get("/health/live").send().await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json(), json!({"status": "alive"}));
}

#[tokio::test]
async fn platform_037_a_draining_instance_is_not_ready() {
    let api = Api::new();
    api.readiness().drain();

    let response = api.get("/health/ready").send().await;

    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.json(), json!({"status": "draining"}));
}

#[tokio::test]
async fn platform_037_readiness_follows_the_instance_s_own_database_connection() {
    let database = TestDatabase::migrated().await;
    let api = Api::new();
    let clock = api.clock();
    database
        .app()
        .await
        .watch(api.readiness().clone(), clock.clone());
    clock.sleeping(CHECK_INTERVAL).await;
    let ready = api.get("/health/ready").send().await;

    // Nothing can connect as app any more, and its open connections end.
    database
        .admin_executes(&format!(
            "REVOKE CONNECT ON DATABASE \"{0}\" FROM PUBLIC, app;
             SELECT pg_terminate_backend(pid) FROM pg_stat_activity
             WHERE datname = '{0}' AND usename = 'app'",
            database.name()
        ))
        .await;
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_INTERVAL).await;
    let unavailable = api.get("/health/ready").send().await;

    database
        .admin_executes(&format!(
            "GRANT CONNECT ON DATABASE \"{}\" TO app",
            database.name()
        ))
        .await;
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_INTERVAL).await;
    let recovered = api.get("/health/ready").send().await;

    assert_eq!(ready.status, StatusCode::OK);
    assert_eq!(ready.json(), json!({"status": "ready"}));
    assert_eq!(unavailable.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        unavailable.json(),
        json!({"status": "database_unavailable"})
    );
    assert_eq!(recovered.status, StatusCode::OK);
}

#[tokio::test]
async fn platform_037_a_pool_with_every_connection_in_use_stays_ready() {
    let database = TestDatabase::migrated().await;
    let api = Api::new();
    let clock = api.clock();
    let pool = database.app().await;
    pool.watch(api.readiness().clone(), clock.clone());
    clock.sleeping(CHECK_INTERVAL).await;

    let mut held = Vec::new();
    for _ in 0..TestDatabase::POOL_SIZE {
        held.push(pool.begin().await.unwrap());
    }
    clock.advance(CHECK_INTERVAL);
    clock.sleeping(CHECK_INTERVAL).await;
    let response = api.get("/health/ready").send().await;

    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(response.json(), json!({"status": "ready"}));
    drop(held);
}
