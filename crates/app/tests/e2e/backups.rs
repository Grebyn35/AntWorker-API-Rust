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
    // A differential backup, as the backup service takes on schedule, from its container.
    deployment
        .server
        .shell_ok("docker exec deployment-backup-1 pgbackrest --stanza=db backup --type=diff");
    let moment = deployment.sql("SELECT now()");
    deployment.sql("INSERT INTO restore_marker VALUES ('after the moment')");
    archive_everything_committed(&deployment);

    let restored = deployment.restore(Some(&moment));

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
        ["full", "full", "diff", "full"],
        "the first deploy's, `backup`'s, the backup service's, and the restore's"
    );
}

#[tokio::test]
async fn ops_17_a_restore_reaches_a_moment_that_an_earlier_restore_went_back_past() {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('early');",
    );
    let early = deployment.sql("SELECT now()");
    deployment.sql("INSERT INTO restore_marker VALUES ('late')");
    let late = deployment.sql("SELECT now()");
    deployment.sql("INSERT INTO restore_marker VALUES ('last')");
    archive_everything_committed(&deployment);
    // Back too far: to `early`, past `late`, which the next restore reaches again.
    let went_back = deployment.restore(Some(&early));
    assert!(went_back.status.success(), "{}", lossy(&went_back));
    assert_eq!(
        deployment.sql("SELECT name FROM restore_marker ORDER BY name"),
        "early"
    );

    let restored = deployment.restore(Some(&late));

    assert!(restored.status.success(), "{}", lossy(&restored));
    assert_eq!(
        deployment.sql("SELECT name FROM restore_marker ORDER BY name"),
        "early\nlate"
    );
}

#[tokio::test]
async fn ops_17_a_restore_reaches_a_moment_after_an_earlier_restore() {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('early');",
    );
    let early = deployment.sql("SELECT now()");
    deployment.sql("INSERT INTO restore_marker VALUES ('gone')");
    archive_everything_committed(&deployment);
    let went_back = deployment.restore(Some(&early));
    assert!(went_back.status.success(), "{}", lossy(&went_back));
    // Written after the restore, on the timeline it began.
    deployment.sql("INSERT INTO restore_marker VALUES ('after')");
    let after = deployment.sql("SELECT now()");
    deployment.sql("INSERT INTO restore_marker VALUES ('last')");
    archive_everything_committed(&deployment);

    let restored = deployment.restore(Some(&after));

    assert!(restored.status.success(), "{}", lossy(&restored));
    assert_eq!(
        deployment.sql("SELECT name FROM restore_marker ORDER BY name"),
        "after\nearly"
    );
}

/// A bundled deployment whose database a restore took back to the moment after it held "early",
/// with no backup on the timeline that restore began, as while its closing backup runs, or
/// after that backup failed: the newest backup is on the timeline the restore abandoned, after
/// its branch, which a replay along the restored timeline can't start from.
async fn restored_with_no_backup_on_its_timeline() -> Deployment {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('early');",
    );
    let early = deployment.sql("SELECT now()");
    deployment.sql("INSERT INTO restore_marker VALUES ('gone')");
    let backed_up = deployment.server.deploy(&["backup"]);
    assert!(backed_up.status.success(), "{}", lossy(&backed_up));
    archive_everything_committed(&deployment);
    let went_back = deployment.restore(Some(&early));
    assert!(went_back.status.success(), "{}", lossy(&went_back));
    let closing = backups(&deployment)
        .last()
        .and_then(|backup| backup["label"].as_str().map(str::to_owned))
        .expect("the restore's closing backup");
    deployment.server.shell_ok(&format!(
        "docker exec --user postgres deployment-postgres-1 \
         pgbackrest --stanza=db expire --set={closing}"
    ));
    deployment
}

#[tokio::test]
async fn ops_17_a_restore_reaches_a_moment_on_a_restored_timeline_before_its_first_backup() {
    let deployment = restored_with_no_backup_on_its_timeline().await;
    deployment.sql("INSERT INTO restore_marker VALUES ('after')");
    let after = deployment.sql("SELECT now()");
    deployment.sql("INSERT INTO restore_marker VALUES ('last')");
    archive_everything_committed(&deployment);

    let restored = deployment.restore(Some(&after));

    assert!(restored.status.success(), "{}", lossy(&restored));
    assert_eq!(
        deployment.sql("SELECT name FROM restore_marker ORDER BY name"),
        "after\nearly"
    );
}

#[tokio::test]
async fn ops_17_a_restore_of_the_latest_state_starts_from_a_backup_on_the_latest_timeline() {
    let deployment = restored_with_no_backup_on_its_timeline().await;
    deployment.sql("INSERT INTO restore_marker VALUES ('after')");
    archive_everything_committed(&deployment);

    let restored = deployment.restore(None);

    assert!(restored.status.success(), "{}", lossy(&restored));
    assert_eq!(
        deployment.sql("SELECT name FROM restore_marker ORDER BY name"),
        "after\nearly"
    );
}

#[tokio::test]
async fn ops_17_a_new_server_serves_only_once_it_restores_the_database_its_bucket_holds() {
    let lost = Deployment::bundled(&[]);
    let deployed = lost.apply(&lost.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    lost.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('on the lost server');",
    );
    archive_everything_committed(&lost);
    let new = lost.on_a_new_server();
    // The server is lost, while its bucket, which stands in for one off the server, stays.
    lost.server.shell_ok(
        "docker ps --quiet --filter label=com.docker.compose.project=deployment | xargs docker rm --force",
    );

    let refused = new.apply(&new.api, &[]).await;
    let refused_replicas = new.server.running("api-blue");
    let restored = new.restore(None);
    let deployed = new.apply(&new.api, &[]).await;

    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("holds the backups of another database"),
        "{}",
        lossy(&refused)
    );
    assert_eq!(refused_replicas.len(), 0);
    assert!(restored.status.success(), "{}", lossy(&restored));
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    assert_eq!(
        new.sql("SELECT name FROM restore_marker"),
        "on the lost server"
    );
    let (status, _) = get(
        new.server.caddy_root(),
        new.url("https", 443, "/v1/capabilities"),
    );
    assert_eq!(status, 200);
}

#[tokio::test]
async fn ops_17_a_restore_after_a_failed_deploy_migrates_with_the_live_release() {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    // The API's image, whose every subcommand fails, so a deploy of it fails in `bootstrap`.
    let broken = deployment.image_from(
        &deployment.api,
        "broken",
        &format!(
            r#"ENTRYPOINT ["{}", "no-such-subcommand"]"#,
            deployment.api_binary()
        ),
    );
    let failed = deployment.apply(&broken, &[]).await;

    let restored = deployment.restore(None);

    assert!(!failed.status.success(), "{}", lossy(&failed));
    assert!(restored.status.success(), "{}", lossy(&restored));
    assert_eq!(deployment.server.running("api-blue").len(), 2);
    let (status, _) = get(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
    );
    assert_eq!(status, 200);
}
