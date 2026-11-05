//! The bundled database's backups and restores (OPS-15), on a server of the tests' own, with a
//! stand-in bucket.

use std::{
    io::Write,
    process::{Command, Output, Stdio},
};

use serde_json::{Value, json};

use crate::{
    deployment::{Deployment, get},
    docker::lossy,
    images::Pushed,
    server::{DEPLOYMENT, Server},
};

/// When the timeline `id` began, as a restore's plan reads it: the second its history file
/// reached the bucket, since the Unix epoch, as `restore-inputs` lists the bucket's histories.
fn timeline_began(deployment: &Deployment, id: u32) -> i64 {
    let inputs = deployment.server.deploy(&["restore-inputs"]);
    assert!(inputs.status.success(), "{}", lossy(&inputs));
    let listed = String::from_utf8_lossy(&inputs.stdout);
    let histories: Value = serde_json::from_str(
        listed
            .lines()
            .find_map(|line| line.strip_prefix("histories "))
            .expect("the bucket's histories"),
    )
    .expect("the histories' JSON");
    let suffix = format!("/{id:08X}.history");
    let began = histories
        .as_object()
        .expect("the histories by name")
        .iter()
        .find_map(|(name, file)| name.ends_with(&suffix).then(|| file["time"].as_i64()))
        .flatten();
    assert!(
        began.is_some(),
        "no history of timeline {id} in the bucket: {histories}"
    );
    began.expect("its time")
}

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
/// or until Postgres stops. Postgres reports the archiver's process ID, which its title in /proc
/// doesn't give where the image runs through QEMU: there, every process shows QEMU's command line.
fn signal_archiver(deployment: &Deployment, signal: &str) {
    deployment.server.write(
        "/root/signal-archiver.sh",
        &format!(
            "archiver=$(psql --username=postgres --dbname=api -qAt \
             -c \"SELECT pid FROM pg_stat_activity WHERE backend_type = 'archiver'\")\n\
             [ -n \"$archiver\" ] && kill -{signal} \"$archiver\"\n"
        ),
    );
    deployment
        .server
        .shell_ok("docker exec -i deployment-postgres-1 sh < /root/signal-archiver.sh");
}

/// Makes Postgres reject, or accept again, the connections through its socket, which the
/// restore's own statements use, while the replicas connect over the network. It returns once
/// Postgres does, and fails the test if Postgres doesn't within a minute (TEST-06).
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
        "docker exec -i --user postgres deployment-postgres-1 timeout 60 sh \
         < /root/local-connections.sh",
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

/// What the Postgres image's retention.jq prints for pgBackRest's info listing `backups`, with
/// a retention of 28 days, at `now`: the retention of the next expire, then each full backup it
/// keeps beyond 28 days, with the time of the restore that marked it.
fn retention(backups: &Value, now: i64) -> String {
    let image = Pushed::postgres();
    let mut jq = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--interactive",
            "--platform",
            image.platform(),
        ])
        .args(["--entrypoint", "jq", &image.on_host()])
        .args([
            "--raw-output",
            "--argjson",
            "days",
            "28",
            "--argjson",
            "now",
        ])
        .args([
            &now.to_string(),
            "--from-file",
            "/usr/local/lib/pgbackrest/retention.jq",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("run docker");
    jq.stdin
        .take()
        .expect("jq's input")
        .write_all(json!([{ "backup": backups }]).to_string().as_bytes())
        .expect("write pgBackRest's info");
    let output = jq.wait_with_output().expect("wait for jq");
    assert!(output.status.success(), "{}", lossy(&output));
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

/// A backup of pgBackRest's info: `label`, of `kind`, which started and ended at the times that
/// `run` gives, in seconds since the epoch, with the annotation of a restore at `restored`, if
/// one marked it.
fn backup(label: &str, kind: &str, run: (i64, i64), restored: Option<i64>) -> Value {
    let mut backup =
        json!({ "label": label, "type": kind, "timestamp": { "start": run.0, "stop": run.1 } });
    if let Some(at) = restored {
        backup["annotation"] = json!({ format!("restore-{at}"): at.to_string() });
    }
    backup
}

#[test]
fn ops_15_the_retention_keeps_the_full_backup_a_restore_started_from_until_a_backup_after_the_restore_is_as_old()
 {
    const DAY: i64 = 86_400;
    let now = 1_800_000_000;
    let ago = |days: i64| now - days * DAY;
    let older = backup("F1", "full", (ago(70), ago(70) + 60), None);
    // The next full backup after F1 ended 40 days ago: pgBackRest keeps F1 only with a retention
    // of 41 days.
    let next = backup("F2", "full", (ago(40) - 60, ago(40)), None);
    let held = |restored: i64| backup("F1", "full", (ago(70), ago(70) + 60), Some(restored));
    // A full backup taken right after a restore, which ended `days` ago.
    let closing = |days: i64| backup("F3", "full", (ago(days) - 60, ago(days)), None);

    let unmarked = retention(&json!([older, next]), now);
    let marked = retention(&json!([held(ago(20) - 120), next, closing(20)]), now);
    let unbacked = retention(&json!([held(ago(35)), next]), now);
    let released = retention(&json!([held(ago(30) - 120), next, closing(30)]), now);
    let newest = retention(
        &json!([
            older,
            backup("F2", "full", (ago(40) - 60, ago(40)), Some(ago(20)))
        ]),
        now,
    );
    let young = retention(
        &json!([
            held(ago(9)),
            backup("F2", "full", (ago(10) - 60, ago(10)), None)
        ]),
        now,
    );
    let diff = retention(
        &json!([
            held(ago(20) - 120),
            backup("F1_D1", "diff", (ago(60), ago(60) + 30), None),
            next,
            closing(20)
        ]),
        now,
    );

    assert_eq!(unmarked, "28");
    assert_eq!(marked, format!("41\nF1 {}", ago(20) - 120));
    assert_eq!(
        unbacked,
        format!("41\nF1 {}", ago(35)),
        "no backup since the restore"
    );
    assert_eq!(
        released, "28",
        "a backup since the restore is older than 28 days"
    );
    assert_eq!(newest, "28", "no full backup follows the marked one");
    assert_eq!(young, "28", "28 days keep the marked one");
    assert_eq!(
        diff,
        format!("41\nF1 {}", ago(20) - 120),
        "a differential backup isn't the next full one"
    );
}

#[tokio::test]
async fn ops_15_a_restore_marks_the_full_backup_it_starts_from_in_the_bucket() {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('early');",
    );
    let early = deployment.sql("SELECT now()");
    // A full backup after the moment, which the restore goes back past.
    let backed_up = deployment.server.deploy(&["backup"]);
    assert!(backed_up.status.success(), "{}", lossy(&backed_up));
    archive_everything_committed(&deployment);
    let before = deployment.sql("SELECT extract(epoch FROM now())::bigint");

    let restored = deployment.restore(Some(&early));

    let after = deployment.sql("SELECT extract(epoch FROM now())::bigint");
    assert!(restored.status.success(), "{}", lossy(&restored));
    let backups = backups(&deployment);
    let first = backups[0]["label"]
        .as_str()
        .expect("the first deploy's backup");
    assert!(
        lossy(&restored).contains(&format!(
            "the bucket keeps the full backup {first}, which the restore starts from"
        )),
        "{}",
        lossy(&restored)
    );
    let marks: Vec<(String, Value)> = backups
        .iter()
        .flat_map(|backup| {
            let label = backup["label"].as_str().unwrap_or_default().to_owned();
            backup["annotation"]
                .as_object()
                .into_iter()
                .flatten()
                .map(move |(key, value)| (format!("{label} {key}"), value.clone()))
        })
        .collect();
    assert_eq!(marks.len(), 1, "{marks:?}");
    let (mark, at) = &marks[0];
    let at: i64 = at.as_str().and_then(|at| at.parse().ok()).expect("a time");
    assert_eq!(*mark, format!("{first} restore-{at}"));
    let (before, after): (i64, i64) = (before.parse().unwrap(), after.parse().unwrap());
    assert!((before..=after).contains(&at), "{before} {at} {after}");
}

#[tokio::test]
async fn ops_15_a_restore_recovers_the_database_as_it_stood_at_a_moment() {
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
async fn ops_15_a_restore_reaches_a_moment_after_the_last_change() {
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
async fn ops_15_a_restore_to_the_moment_an_earlier_restore_names_undoes_it() {
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
async fn ops_15_a_restore_reaches_a_moment_while_an_earlier_restore_ran() {
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
    // Halfway from the moment that restore named to the second its timeline's history reached
    // the bucket, which a restore's plan reads as when that timeline began: after the
    // restore's transaction committed, and before its timeline began, for the plan as for
    // Postgres. No commit or rollback that the bucket holds follows it on the timeline that
    // restore abandoned.
    let named = undoing(&went_back);
    let began = timeline_began(&deployment, 2);
    let during = deployment.sql(&format!(
        "SELECT CASE WHEN to_timestamp({began}) > '{named}'::timestamptz \
         THEN '{named}'::timestamptz + (to_timestamp({began}) - '{named}'::timestamptz) / 2 END"
    ));
    assert!(
        !during.is_empty(),
        "timeline 2 began at {began}, in the second the restore named, {named}: a plan, which \
         reads whole seconds, can't tell the moments between apart"
    );

    let restored = deployment.restore(Some(&during));

    let written = lossy(&restored);
    assert!(restored.status.success(), "{written}");
    assert!(
        written.contains("replays the timeline to its end"),
        "{written}"
    );
    assert!(
        written.contains("ends where timeline 1 ends in the bucket, with its last commit at "),
        "{written}"
    );
    assert_eq!(
        deployment.sql("SELECT name FROM restore_marker ORDER BY name"),
        "early\nlast"
    );
    assert_eq!(deployment.server.running("api-blue").len(), 2);
}

#[tokio::test]
async fn ops_15_a_restore_reaches_a_moment_that_an_earlier_restore_went_back_past() {
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
async fn ops_15_a_restore_reaches_a_moment_after_an_earlier_restore() {
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
async fn ops_15_a_restore_reaches_a_moment_on_a_restored_timeline_before_its_first_backup() {
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
async fn ops_15_a_restore_of_the_latest_state_starts_from_a_backup_on_the_latest_timeline() {
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
/// server set up from its settings, secrets and bucket, which nothing has deployed to yet. With
/// `backed_up`, `backup` took a full backup after that change, which no commit the bucket holds
/// then follows. The lost deployment's bucket, which stands in for one off the server, lives as
/// long as it does.
async fn lost_and_new_server(backed_up: bool) -> (Deployment, Deployment) {
    // Both servers' shares at once, so no test holds one while it waits for another.
    let (lost_server, new_server) = Server::start_two();
    let lost = Deployment::bundled_on(lost_server, &[]);
    let deployed = lost.apply(&lost.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    lost.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('on the lost server');",
    );
    archive_everything_committed(&lost);
    if backed_up {
        // Autovacuum's ANALYZE commits on a schedule of its own, which could put a commit during
        // or after the backup: it stops, and the workers already running end, first.
        lost.sql("ALTER SYSTEM SET autovacuum = off");
        lost.sql("SELECT pg_reload_conf()");
        lost.server.write(
            "/root/autovacuum-ended.sh",
            "until [ \"$(docker exec deployment-postgres-1 psql --username=postgres --dbname=api -qAt \
             -c \"SELECT count(*) FROM pg_stat_activity WHERE backend_type = 'autovacuum worker'\")\" = 0 ]; \
             do sleep 1; done\n",
        );
        lost.server
            .shell_ok("timeout 120 sh /root/autovacuum-ended.sh");
        let backed = lost.server.deploy(&["backup"]);
        assert!(backed.status.success(), "{}", lossy(&backed));
    }
    let new = lost.on_a_new_server(new_server);
    // The server is lost, while its bucket stays.
    lost.server.shell_ok(
        "docker ps --quiet --filter label=com.docker.compose.project=deployment | xargs docker rm --force",
    );
    (lost, new)
}

#[tokio::test]
async fn ops_15_a_new_server_serves_only_once_it_restores_the_database_its_bucket_holds() {
    let (_lost, new) = lost_and_new_server(false).await;

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
async fn ops_15_a_restore_on_a_new_server_reaches_a_moment_after_the_last_archived_change_and_names_no_moment_that_undoes_it()
 {
    // With a backup after the lost server's last change, so the replay from it finds no commit.
    let (_lost, new) = lost_and_new_server(true).await;
    // The first deploy stops before any replica starts, with Postgres running the empty database
    // it has just created, which the bucket doesn't back up.
    let refused = new.apply(&new.api, &[]).await;
    assert!(!refused.status.success(), "{}", lossy(&refused));
    // After the lost server's last archived change, which no commit or rollback the bucket holds
    // follows.
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
    assert!(
        written.contains("which holds no commit on it after the backup"),
        "{written}"
    );
    assert_eq!(
        new.sql("SELECT name FROM restore_marker"),
        "on the lost server"
    );
}

#[tokio::test]
async fn ops_15_a_restore_refuses_while_the_log_doesn_t_reach_the_bucket_unless_allowed() {
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
        !lossy(&refused).contains("stopping the backups"),
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

/// Overwrites `segment`'s object in the stand-in bucket with bytes that aren't a segment, as a
/// bucket that fails to give pgBackRest the segment does: pgBackRest finds the object, and fails
/// to read it.
fn break_in_the_bucket(deployment: &Deployment, segment: &str) {
    deployment.server.write(
        "/root/break-segment.sh",
        &format!(
            "set -e
            for archive in $(docker exec --user postgres deployment-postgres-1 pgbackrest --stanza=db --log-level-console=error repo-ls archive/db); do
                directory=archive/db/$archive/$(echo {segment} | cut -c 1-16)
                name=$(docker exec --user postgres deployment-postgres-1 pgbackrest --stanza=db --log-level-console=error repo-ls \"$directory\" | grep '^{segment}-' || true)
                if [ -n \"$name\" ]; then break; fi
            done
            [ -n \"$name\" ]
            docker exec bucket sh -c \"mkdir -p /tmp/broken && echo 'not a segment' > /tmp/broken/$name && \
                weed filer.copy /tmp/broken/$name http://localhost:8888/buckets/backups/pgbackrest/$directory/\"
            "
        ),
    );
    deployment.server.shell_ok("sh /root/break-segment.sh");
}

#[tokio::test]
async fn ops_15_a_restore_whose_replay_can_t_fetch_a_segment_fails_rather_than_ending_the_replay_there()
 {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('before');",
    );
    archive_everything_committed(&deployment);
    deployment.sql("INSERT INTO restore_marker VALUES ('after')");
    let segment = deployment.sql("SELECT pg_walfile_name(pg_current_wal_lsn())");
    archive_everything_committed(&deployment);
    break_in_the_bucket(&deployment, &segment);

    let restored = deployment.restore(None);
    let (status, _) = get(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
    );

    assert!(!restored.status.success(), "{}", lossy(&restored));
    assert!(
        lossy(&restored).contains("couldn't fetch a segment of the log from the bucket"),
        "{}",
        lossy(&restored)
    );
    assert_eq!(status, 503);
}

/// The line a restore writes once its first check has passed and it has marked its full backup,
/// before the replicas stop and its transaction commits.
const STOPPING: &str = "stopping the API's replicas";

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
async fn ops_15_a_restore_whose_log_stops_reaching_the_bucket_once_the_replicas_stop_serves_again_and_refuses_unless_allowed()
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
async fn ops_15_a_restore_whose_transaction_fails_once_the_replicas_stop_serves_again_and_refuses()
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
async fn ops_15_a_restore_whose_mark_fails_refuses_with_the_api_serving() {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('kept');",
    );

    // A backup that the bucket doesn't hold, as when an expire removed it after the restore
    // planned it, fails the mark as a bucket that stops answering does, whatever the options.
    let refusals = [&[][..], &["--allow-unarchived"][..]].map(|options| {
        deployment
            .server
            .deploy(&[&["restore", "", "20000101-000000F", "1"][..], options].concat())
    });

    for refused in &refusals {
        assert!(!refused.status.success(), "{}", lossy(refused));
        assert!(
            lossy(refused).contains("couldn't mark the full backup 20000101-000000F"),
            "{}",
            lossy(refused)
        );
        assert!(!lossy(refused).contains(STOPPING), "{}", lossy(refused));
    }
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
async fn ops_15_a_restore_finds_its_postgres_while_a_one_off_of_the_service_runs() {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('kept');",
    );
    // A container of the Postgres service that `compose run` starts, as restore-inputs does while
    // another restore plans, without the lock. The restore's stop of Postgres leaves it running.
    let one_off = deployment.server.shell_ok(&format!(
        "cd {DEPLOYMENT} && docker compose --project-name deployment --project-directory . \
         --file compose.yaml --env-file settings.env --env-file state/deploy.env --profile database \
         run --detach --rm --no-deps --entrypoint sleep postgres 600"
    ));

    let restored = deployment.restore(None);

    assert!(restored.status.success(), "{}", lossy(&restored));
    // Still running once the restore ends, so it ran while the restore looked Postgres up.
    assert_eq!(
        deployment.server.shell_ok(&format!(
            "docker inspect --format '{{{{.State.Running}}}}' {one_off}"
        )),
        "true"
    );
    assert_eq!(deployment.sql("SELECT name FROM restore_marker"), "kept");
    let (status, _) = get(
        deployment.server.caddy_root(),
        deployment.url("https", 443, "/v1/capabilities"),
    );
    assert_eq!(status, 200);
}

#[tokio::test]
async fn ops_15_a_restore_that_finds_postgres_replaying_the_log_restores_without_a_check() {
    let deployment = Deployment::bundled(&[]);
    let deployed = deployment.apply(&deployment.api, &[]).await;
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    deployment.sql(
        "CREATE TABLE restore_marker (name text PRIMARY KEY); \
         INSERT INTO restore_marker VALUES ('archived');",
    );
    archive_everything_committed(&deployment);
    // Postgres in recovery, as a restore that failed during its replay leaves it: a standby that
    // nothing feeds, which stays in recovery and accepts connections.
    deployment.server.shell_ok(
        "docker exec --user postgres deployment-postgres-1 sh -c 'touch \"$PGDATA/standby.signal\"' \
         && docker restart deployment-postgres-1 > /dev/null \
         && timeout 120 sh -c 'until [ \"$(docker exec deployment-postgres-1 psql --username=postgres \
            --dbname=api -qAt -c \"SELECT pg_is_in_recovery()\" 2> /dev/null)\" = t ]; do sleep 1; done'",
    );

    let restored = deployment.restore(None);

    let written = lossy(&restored);
    assert!(restored.status.success(), "{written}");
    assert!(!written.contains("pgBackRest's check failed"), "{written}");
    assert!(!written.contains("undoes this one"), "{written}");
    assert_eq!(
        deployment.sql("SELECT name FROM restore_marker"),
        "archived"
    );
    assert_eq!(deployment.sql("SELECT pg_is_in_recovery()"), "f");
}

#[tokio::test]
async fn ops_15_a_restore_after_a_failed_deploy_migrates_with_the_live_release() {
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
