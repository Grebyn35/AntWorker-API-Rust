//! The bundled database's backups and restores (OPS-17), on a server of the tests' own, with a
//! stand-in bucket.

use serde_json::Value;

use crate::{
    deployment::{Deployment, get},
    docker::lossy,
};

/// Waits until Postgres has archived the segment of the write-ahead log that holds everything
/// committed so far: it ends that segment, and waits until the archiver has passed it.
fn archive_everything_committed(deployment: &Deployment) {
    let segment = deployment.sql("SELECT pg_walfile_name(pg_current_wal_lsn())");
    deployment.sql("SELECT pg_switch_wal()");
    // The archiver runs on its own, and Postgres reports what it archived last.
    deployment.server.write(
        "/root/archived.sh",
        &format!(
            "until [ \"$(docker exec deployment-postgres-1 psql --username=postgres --dbname=api -qAt \
             -c \"SELECT last_archived_wal >= '{segment}' FROM pg_stat_archiver\")\" = t ]; do sleep 1; done\n"
        ),
    );
    deployment
        .server
        .shell_ok("timeout 120 sh /root/archived.sh");
}

/// The full and differential backups the bucket holds.
fn backups(deployment: &Deployment) -> Vec<Value> {
    let info: Value = serde_json::from_str(&deployment.server.shell_ok(
        "docker exec --user postgres deployment-postgres-1 pgbackrest --stanza=db info --output=json",
    ))
    .expect("pgBackRest's report");
    info[0]["backup"].as_array().expect("the backups").clone()
}

#[tokio::test]
async fn ops_17_a_restore_recovers_the_database_as_it_stood_at_a_moment() {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('before the moment');",
    );
    let backed_up = deployment.server.deploy(&["backup"]);
    assert!(backed_up.status.success(), "{}", lossy(&backed_up));
    let moment = deployment.sql("SELECT now()");
    deployment.sql("INSERT INTO restore_marker VALUES ('after the moment')");
    archive_everything_committed(&deployment);

    let restored = deployment.server.deploy(&["restore", &moment]);

    assert!(restored.status.success(), "{}", lossy(&restored));
    assert_eq!(
        deployment.sql("SELECT name FROM restore_marker ORDER BY name"),
        "before the moment"
    );
    assert_eq!(deployment.server.running("api-blue").len(), 2);
    let (status, _) = get(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
    );
    assert_eq!(status, 200);
    let backups = backups(&deployment);
    let kinds: Vec<&str> = backups
        .iter()
        .map(|backup| backup["type"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        kinds,
        ["full", "full", "full"],
        "the first deploy's, `backup`'s, and the restore's"
    );
}
