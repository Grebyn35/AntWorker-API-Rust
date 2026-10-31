//! The bundled database's backups and restores (OPS-17), on a server of the tests' own, with a
//! stand-in bucket.

use std::process::Output;

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

/// Sends `signal` to Postgres's archiver: `STOP`, so what Postgres writes from then on doesn't
/// reach the bucket, which stays readable, as when the archiving fails on its own, until `CONT`
/// or until Postgres stops.
fn signal_archiver(deployment: &Deployment, signal: &str) {
    deployment.server.write(
        "/root/signal-archiver.sh",
        &format!(
            "signaled=no\n\
             for process in /proc/[0-9]*; do\n\
             case $(tr '\\0' ' ' < \"$process/cmdline\" 2> /dev/null) in\n\
             \"postgres: archiver\"*) kill -{signal} \"${{process#/proc/}}\" && signaled=yes ;;\n\
             esac\n\
             done\n\
             [ \"$signaled\" = yes ]\n"
        ),
    );
    deployment
        .server
        .shell_ok("docker exec -i deployment-postgres-1 sh < /root/signal-archiver.sh");
}

/// Makes Postgres reject, or accept again, the connections through its socket, which the
/// restore's own statements use, while the replicas connect over the network. It returns once
/// Postgres does.
fn local_connections(deployment: &Deployment, accepted: bool) {
    let (change, until) = if accepted {
        ("1d", "until")
    } else {
        ("1i local all all reject", "while")
    };
    deployment.server.write(
        "/root/local-connections.sh",
        &format!(
            "sed -i '{change}' \"$PGDATA/pg_hba.conf\" && pg_ctl reload --silent\n\
             {until} psql -qAt -c 'SELECT 1' > /dev/null 2>&1; do sleep 0.1; done\n"
        ),
    );
    deployment.server.shell_ok(
        "docker exec -i --user postgres deployment-postgres-1 sh < /root/local-connections.sh",
    );
}

/// The full and differential backups the bucket holds.
fn backups(deployment: &Deployment) -> Vec<Value> {
    let info: Value = serde_json::from_str(&deployment.server.shell_ok(
        "docker exec --user postgres deployment-postgres-1 pgbackrest --stanza=db info --output=json",
    ))
    .expect("pgBackRest's report");
    info[0]["backup"].as_array().expect("the backups").clone()
}

/// The moment that `restored`, a restore's output, names as the one a restore to undoes it.
fn undoing(restored: &Output) -> String {
    let output = lossy(restored);
    let named = output.lines().find_map(|line| {
        line.strip_prefix("deploy: a restore to ")?
            .strip_suffix(" undoes this one")
    });
    assert!(
        named.is_some(),
        "the restore names no moment that undoes it: {output}"
    );
    named.expect("the moment").to_owned()
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
async fn ops_17_a_restore_reaches_a_moment_after_the_last_change() {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('last');",
    );
    // Nothing commits after the moment, as on a deployment that nothing wrote to since.
    let moment = deployment.sql("SELECT now()");
    archive_everything_committed(&deployment);

    let restored = deployment.restore(Some(&moment));

    assert!(restored.status.success(), "{}", lossy(&restored));
    assert_eq!(deployment.sql("SELECT name FROM restore_marker"), "last");
}

#[tokio::test]
async fn ops_17_a_restore_to_the_moment_an_earlier_restore_names_undoes_it() {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('early');",
    );
    let early = deployment.sql("SELECT now()");
    // The last change before the restore, which no commit follows until the restore runs.
    deployment.sql("INSERT INTO restore_marker VALUES ('last')");
    archive_everything_committed(&deployment);
    let went_back = deployment.restore(Some(&early));
    assert!(went_back.status.success(), "{}", lossy(&went_back));
    assert_eq!(
        deployment.sql("SELECT name FROM restore_marker ORDER BY name"),
        "early"
    );

    let undone = deployment.restore(Some(&undoing(&went_back)));

    assert!(undone.status.success(), "{}", lossy(&undone));
    assert_eq!(
        deployment.sql("SELECT name FROM restore_marker ORDER BY name"),
        "early\nlast"
    );
}

#[tokio::test]
async fn ops_17_a_restore_reaches_a_moment_while_an_earlier_restore_ran() {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('early');",
    );
    let early = deployment.sql("SELECT now()");
    deployment.sql("INSERT INTO restore_marker VALUES ('last')");
    archive_everything_committed(&deployment);
    let went_back = deployment.restore(Some(&early));
    assert!(went_back.status.success(), "{}", lossy(&went_back));
    // A second after the moment that restore named: once its transaction had committed, and
    // before its timeline began, which waited for its second check, Postgres's stop and the
    // restore of the backup. No commit that the bucket holds follows it on the timeline that
    // restore abandoned.
    let during = deployment.sql(&format!(
        "SELECT '{}'::timestamptz + interval '1 second'",
        undoing(&went_back)
    ));

    let restored = deployment.restore(Some(&during));

    let written = lossy(&restored);
    assert!(restored.status.success(), "{written}");
    assert!(
        written.contains("replays the timeline to its end"),
        "{written}"
    );
    assert!(
        written.contains("ends with the last commit on timeline 1 that the bucket holds, at "),
        "{written}"
    );
    assert_eq!(
        deployment.sql("SELECT name FROM restore_marker ORDER BY name"),
        "early\nlast"
    );
    assert_eq!(deployment.server.running("api-blue").len(), 2);
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

/// A bundled deployment that held "on the lost server" when its server was lost, and a new
/// server set up from its settings, secrets and bucket, which nothing has deployed to yet. The
/// lost deployment's bucket, which stands in for one off the server, lives as long as it does.
async fn lost_and_new_server() -> (Deployment, Deployment) {
    let lost = Deployment::bundled(&[]);
    let deployed = lost.apply(&lost.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    lost.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('on the lost server');",
    );
    archive_everything_committed(&lost);
    let new = lost.on_a_new_server();
    // The server is lost, while its bucket stays.
    lost.server.shell_ok(
        "docker ps --quiet --filter label=com.docker.compose.project=deployment | xargs docker rm --force",
    );
    (lost, new)
}

#[tokio::test]
async fn ops_17_a_new_server_serves_only_once_it_restores_the_database_its_bucket_holds() {
    let (_lost, new) = lost_and_new_server().await;

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
    assert!(
        !lossy(&restored).contains("undoes this one"),
        "{}",
        lossy(&restored)
    );
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
async fn ops_17_a_restore_on_a_new_server_reaches_a_moment_after_the_last_archived_change_and_names_no_moment_that_undoes_it()
 {
    let (_lost, new) = lost_and_new_server().await;
    // The first deploy stops before any replica starts, with Postgres running the empty database
    // it has just created, which the bucket doesn't back up.
    let refused = new.apply(&new.api, &[]).await;
    assert!(!refused.status.success(), "{}", lossy(&refused));
    // After the lost server's last archived change, which no commit the bucket holds follows.
    let moment = new.sql("SELECT now()");

    let restored = new.restore(Some(&moment));

    let written = lossy(&restored);
    assert!(restored.status.success(), "{written}");
    assert!(!written.contains("undoes this one"), "{written}");
    assert!(
        written.contains("the bucket backs up another database"),
        "{written}"
    );
    assert!(
        written.contains("replays the timeline to its end"),
        "{written}"
    );
    assert_eq!(
        new.sql("SELECT name FROM restore_marker"),
        "on the lost server"
    );
}

#[tokio::test]
async fn ops_17_a_restore_refuses_while_the_log_doesn_t_reach_the_bucket_unless_allowed() {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('archived');",
    );
    archive_everything_committed(&deployment);
    signal_archiver(&deployment, "STOP");
    deployment.sql("INSERT INTO restore_marker VALUES ('unarchived')");

    let refused = deployment.restore(None);
    let replicas = deployment.server.running("api-blue");
    let backup = deployment.server.running("backup");
    let (status, _) = get(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
    );
    let kept = deployment.sql("SELECT name FROM restore_marker ORDER BY name");
    // Postgres stops only once its stopped archiver is killed, at the end of the stop's grace.
    let restored = deployment.restore_with(None, &["--allow-unarchived"]);

    assert!(!refused.status.success(), "{}", lossy(&refused));
    assert!(
        lossy(&refused).contains("add --allow-unarchived"),
        "{}",
        lossy(&refused)
    );
    assert!(
        !lossy(&refused).contains("stopping the API and the backups"),
        "{}",
        lossy(&refused)
    );
    assert_eq!(replicas.len(), 2);
    assert_eq!(backup.len(), 1);
    assert_eq!(status, 200);
    assert_eq!(kept, "archived\nunarchived");
    assert!(restored.status.success(), "{}", lossy(&restored));
    assert!(
        !lossy(&restored).contains("undoes this one"),
        "{}",
        lossy(&restored)
    );
    assert_eq!(
        deployment.sql("SELECT name FROM restore_marker"),
        "archived"
    );
}

/// The line a restore writes once its first check has passed, before the replicas stop and its
/// transaction commits.
const STOPPING: &str = "stopping the API and the backups";

/// Checks that `written`, what a restore that failed wrote, says it refused once the replicas had
/// stopped, and that the deployment serves the database as the restore found it.
fn served_again_and_refused(deployment: &Deployment, succeeded: bool, written: &str) {
    assert!(!succeeded, "{written}");
    assert!(written.contains("add --allow-unarchived"), "{written}");
    assert!(
        written.contains("started the API and the backups again"),
        "{written}"
    );
    assert!(!written.contains("restoring the database"), "{written}");
    assert_eq!(deployment.server.running("api-blue").len(), 2);
    assert_eq!(deployment.server.running("backup").len(), 1);
    let (status, _) = get(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
    );
    assert_eq!(status, 200);
    assert_eq!(deployment.sql("SELECT name FROM restore_marker"), "kept");
}

#[tokio::test]
async fn ops_17_a_restore_whose_log_stops_reaching_the_bucket_once_the_replicas_stop_serves_again_and_refuses_unless_allowed()
 {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('kept');",
    );

    let running = deployment.restore_until(&[], STOPPING);
    signal_archiver(&deployment, "STOP");
    let (refused, refusal) = running.finish();
    served_again_and_refused(&deployment, refused, &refusal);
    signal_archiver(&deployment, "CONT");
    // Postgres stops only once its stopped archiver is killed, at the end of the stop's grace.
    let running = deployment.restore_until(&["--allow-unarchived"], STOPPING);
    signal_archiver(&deployment, "STOP");
    let (restored, restoring) = running.finish();

    assert!(restored, "{restoring}");
    assert!(restoring.contains("restoring anyway"), "{restoring}");
    assert!(!restoring.contains("undoes this one"), "{restoring}");
    assert_eq!(deployment.sql("SELECT name FROM restore_marker"), "kept");
}

#[tokio::test]
async fn ops_17_a_restore_whose_transaction_fails_once_the_replicas_stop_serves_again_and_refuses()
{
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('kept');",
    );

    let running = deployment.restore_until(&[], STOPPING);
    local_connections(&deployment, false);
    let (refused, refusal) = running.finish();
    local_connections(&deployment, true);

    served_again_and_refused(&deployment, refused, &refusal);
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
