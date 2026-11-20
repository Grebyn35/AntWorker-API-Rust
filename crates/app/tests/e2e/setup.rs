//! The setup of a new server, and the check of what a deploy needs (OPS-19), on a server of the
//! tests' own. It runs deploy/server-setup.sh as root, as `just server-setup` does, with what the
//! developer's machine would send it, and deploy.sh's check as `just server-check` does.

use crate::{deployment::API_ADDRESS, docker::lossy, images::Pushed, server::Server};

/// The deployment's directory: the home of the user that the setup creates.
const DEPLOYMENT: &str = "/home/deploy/deployment";
/// A copy of the release's files that the deployment's user can read, as `just deploy` sends
/// them to its home.
const RELEASE: &str = "/opt/release";
/// A lost server's superuser password, which holds characters that end a URL's password unless
/// they are percent-encoded.
const LOST_PASSWORD: &str = "a lost server's / + @ ? # % : password";

/// Runs the setup as `just server-setup` does: copies the release's files to a temporary
/// directory, and runs its server-setup.sh there with the settings in /root/setup/settings.env,
/// and with `secrets` as its argument, the bucket's key pair, its authority, the registry's
/// login, and the superuser's password in /root/setup/postgres_password if there is one, in the
/// form that server-setup.sh's comment describes.
const SET_UP: &str = concat!(
    r#"set -e
encode() { base64 < "$1" | tr -d '\n'; }
{
    printf 'settings %s\n' "$(encode /root/setup/settings.env)"
    if [ "${1:-}" = secrets ]; then
        printf 'e2e' > /root/setup/key
        printf 'e2e-secret' > /root/setup/key_secret
        printf 'e2e-token' > /root/setup/token
        printf 'secret backup_s3_key %s\n' "$(encode /root/setup/key)"
        printf 'secret backup_s3_key_secret %s\n' "$(encode /root/setup/key_secret)"
        printf 'https://hooks.example.invalid/page' > /root/setup/page_url
        printf 'secret alertmanager/page_url %s\n' "$(encode /root/setup/page_url)"
        printf 'cert backup-ca.crt %s\n' "$(encode /root/bucket/ca.crt)"
"#,
    // part: files
    r#"        printf 'secret files_s3_key %s\n' "$(encode /root/setup/key)"
        printf 'secret files_s3_key_secret %s\n' "$(encode /root/setup/key_secret)"
        printf 'cert files-ca.crt %s\n' "$(encode /root/bucket/ca.crt)"
"#,
    // end part: files
    r#"        printf 'login registry:5000 e2e %s\n' "$(encode /root/setup/token)"
        if [ -s /root/setup/postgres_password ]; then
            printf 'secret postgres_password %s\n' "$(encode /root/setup/postgres_password)"
        fi
    fi
} > /root/setup/payload
directory=$(mktemp -d)
cp -r /root/release/. "$directory"
sh "$directory/server-setup.sh" apply < /root/setup/payload
"#
);

/// Runs `command` in a shell of the deployment's user, with the deployment's directory.
fn as_deploy(server: &Server, command: &str) -> std::process::Output {
    server.shell(&format!(
        "su -s /bin/sh deploy -c 'DEPLOYMENT={DEPLOYMENT} {command}'"
    ))
}

/// A new server, with a stand-in bucket, the setup's script and its settings: the bundled
/// database, which backs up to the bucket, and no telemetry. Returns the server and the settings.
fn new_server() -> (Server, String) {
    let server = Server::start();
    let mut settings = format!(
        "IMAGE_REPOSITORY=registry:5000/image\nSOURCE=https://example.invalid/e2e\n\
         API_ADDRESS={API_ADDRESS}\nTELEMETRY=off\nEMAIL=off\nDATABASE=bundled\nAPI_REPLICAS=2\n\
         DATABASE_MAX_CONNECTIONS=2\n"
    );
    let buckets = server.start_bucket();
    // part: files
    let buckets = [buckets, server.start_files_bucket()].concat();
    // end part: files
    for (name, value) in buckets {
        settings.push_str(&format!("{name}={value}\n"));
    }
    server.write("/root/setup/settings.env", &settings);
    server.write("/root/setup/set-up.sh", SET_UP);
    server.shell_ok(&format!(
        "cp -r /root/release {RELEASE} && chmod -R a+rX {RELEASE}"
    ));
    (server, settings)
}

/// A new server that the setup has prepared, as `new_server` starts it, and the command of the
/// check of what a deploy of the images under test needs on it, as `just server-check` runs it.
fn prepared_server() -> (Server, String) {
    let (server, settings) = new_server();
    let api = Pushed::api().in_network();
    let postgres = Pushed::postgres().in_network();
    let set_up = server.shell("sh /root/setup/set-up.sh secrets");
    assert_set_up(
        &server,
        &set_up,
        &settings,
        &["backup_cipher_pass", "postgres_password"],
    );
    (
        server,
        format!("sh {RELEASE}/deploy.sh check-images {api} {postgres}"),
    )
}

/// Asserts what the first setup of a new server did, which wrote `output`, and that it wrote
/// `settings`, and generated the key ring and the secrets `generated` and no others. Returns the
/// passphrase.
fn assert_set_up(
    server: &Server,
    output: &std::process::Output,
    settings: &str,
    generated: &[&str],
) -> String {
    assert!(output.status.success(), "{}", lossy(output));
    let output = lossy(output);
    for line in [
        "Docker",
        "created the user deploy",
        "added deploy to the docker group",
        "wrote /home/deploy/deployment/secrets/backup_s3_key_secret",
        "wrote /home/deploy/deployment/certs/backup-ca.crt",
        "wrote /home/deploy/deployment/secrets/alertmanager/page_url",
        "logged in to registry:5000 as e2e",
        "generated these secrets",
    ] {
        assert!(output.contains(line), "{line}: {output}");
    }
    for name in ["backup_cipher_pass", "postgres_password"] {
        let value = server.shell_ok(&format!("cat {DEPLOYMENT}/secrets/{name}"));
        let shown = output
            .lines()
            .any(|line| line.trim() == format!("{name}: {value}"));
        if generated.contains(&name) {
            assert!(
                value.len() == 48 && value.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "{name}: {value}"
            );
            assert!(shown, "{name}, shown once: {output}");
        } else {
            assert!(!shown, "{name}, which the setup was given: {output}");
        }
    }
    // The key ring, which every first setup generates: one key, version 1, the base64 of 32
    // random bytes, shown once (PLATFORM-086).
    let ring = server.shell_ok(&format!("cat {DEPLOYMENT}/secrets/encryption_keys"));
    assert!(
        ring.len() == 46 && ring.starts_with("1:") && ring.ends_with('='),
        "{ring}"
    );
    assert!(
        output
            .lines()
            .any(|line| line.trim() == format!("encryption_keys: {ring}")),
        "encryption_keys, shown once: {output}"
    );
    assert_eq!(
        server.shell_ok(&format!("stat -c '%U %a' {DEPLOYMENT}/secrets")),
        "deploy 700"
    );
    assert_eq!(
        server.shell_ok(&format!("cat {DEPLOYMENT}/settings.env")),
        settings.trim()
    );
    server.shell_ok("grep -q registry:5000 /home/deploy/.docker/config.json");
    server.shell_ok(&format!("cat {DEPLOYMENT}/secrets/backup_cipher_pass"))
}

#[tokio::test]
async fn ops_19_a_new_server_that_the_setup_prepares_passes_the_check_and_runs_a_deploy() {
    let (server, settings) = new_server();
    let api = Pushed::api().in_network();
    let postgres = Pushed::postgres().in_network();
    let check = format!("sh {RELEASE}/deploy.sh check-images {api} {postgres}");
    server.write("/root/setup/postgres_password", LOST_PASSWORD);

    let first = server.shell("sh /root/setup/set-up.sh secrets");
    let passphrase = assert_set_up(&server, &first, &settings, &["backup_cipher_pass"]);
    let again = server.shell("sh /root/setup/set-up.sh");
    let before = as_deploy(&server, &check);
    let deployed = as_deploy(
        &server,
        &format!("sh {RELEASE}/deploy.sh apply {api} {postgres}"),
    );
    let after = as_deploy(&server, &check);
    // The server is lost, and a new one, which has no database yet, uses its bucket, which
    // stands in for one off the server.
    server.shell_ok(
        "docker ps --all --quiet --filter label=com.docker.compose.project=deployment \
         | xargs docker rm --force > /dev/null \
         && docker volume rm deployment_postgres > /dev/null",
    );
    let replaced = as_deploy(&server, &check);

    assert!(again.status.success(), "{}", lossy(&again));
    assert!(!lossy(&again).contains("generated"), "{}", lossy(&again));
    assert!(
        !lossy(&again).contains("created the user"),
        "{}",
        lossy(&again)
    );
    assert_eq!(
        server.shell_ok(&format!("cat {DEPLOYMENT}/secrets/backup_cipher_pass")),
        passphrase
    );
    assert_eq!(
        server.shell_ok(&format!("cat {DEPLOYMENT}/secrets/postgres_password")),
        LOST_PASSWORD
    );
    assert_checked(&before, "holds no backups yet");
    // The deploy's bootstrap signs in as the superuser with the lost server's password.
    assert!(deployed.status.success(), "{}", lossy(&deployed));
    assert_checked(&after, "holds backups that the passphrase decrypts");
    assert!(
        !lossy(&after).contains("no database yet"),
        "{}",
        lossy(&after)
    );
    assert_checked(&replaced, "holds backups that the passphrase decrypts");
    assert!(
        lossy(&replaced).contains(
            "check: note  the bucket holds a database's backups, and this server has no database \
             yet"
        ),
        "{}",
        lossy(&replaced)
    );
}

/// Asserts that the check that wrote `output` passed each of its checks, and found that the
/// bucket `holds` what it says.
fn assert_checked(output: &std::process::Output, holds: &str) {
    assert!(output.status.success(), "{}", lossy(output));
    let output = lossy(output);
    for line in [
        "check: ok    Docker",
        "check: ok    the settings",
        "check: ok    the secrets",
        "check: ok    memory",
        "check: ok    the server pulls",
        &format!("check: ok    the bucket backups answers to its key pair, and {holds}"),
        "check: ok    the backups' bucket, backups, keeps what is deleted in it for 7 days, and \
         its key pair can't remove it",
        // part: files
        "check: ok    the files bucket, files, keeps what is deleted in it for 7 days, and its key \
         pair can't remove it",
        // end part: files
        // The tests' server has no systemd.
        "check: note  whether the clock is synchronized",
        "check: note  whether the disk is encrypted",
        "deploy: every check on the server passed",
    ] {
        assert!(output.contains(line), "{line}: {output}");
    }
}

#[tokio::test]
async fn ops_19_the_check_finds_what_a_deploy_would_refuse_of_the_buckets_and_the_settings() {
    let (server, settings) = new_server();
    let api = Pushed::api().in_network();
    let postgres = Pushed::postgres().in_network();
    let set_up = server.shell("sh /root/setup/set-up.sh secrets");
    assert_set_up(
        &server,
        &set_up,
        &settings,
        &["backup_cipher_pass", "postgres_password"],
    );
    let check = || {
        as_deploy(
            &server,
            &format!("sh {RELEASE}/deploy.sh check-images {api} {postgres}"),
        )
    };

    server.shell_ok(&format!(
        "printf wrong > {DEPLOYMENT}/secrets/backup_s3_key_secret"
    ));
    let refused_key = check();
    server.shell_ok(&format!(
        "sed -i /^API_ADDRESS=/d {DEPLOYMENT}/settings.env"
    ));
    let missing_setting = check();

    assert_found(
        &refused_key,
        false,
        &[
            "check: FAIL  pgBackRest can't read the bucket backups with its key pair",
            "1 of the checks on the server failed",
        ],
    );
    assert_found(
        &missing_setting,
        false,
        &["check: FAIL  API_ADDRESS is required"],
    );
}

#[tokio::test]
async fn ops_22_the_check_fails_while_a_bucket_s_key_pair_can_remove_what_the_bucket_keeps() {
    let (server, command) = prepared_server();
    let check = || as_deploy(&server, &command);
    // How many versions of the check's object the backups' bucket holds, and how many delete
    // markers, as the operator's key pair lists them.
    let versions = || {
        let listed = server.shell_ok(
            "docker exec bucket curl --silent --show-error --fail --aws-sigv4 aws:amz:us-east-1:s3 \
             --user operator:operator-secret 'http://localhost:8333/backups?versions&prefix=server-check/'",
        );
        (
            listed.matches("<Version>").count(),
            listed.matches("<DeleteMarker>").count(),
        )
    };

    // The key pair that the stand-in keeps to its policy, then the operator's, which may delete
    // a version and write the bucket's versioning and lifecycle, as a provider's key pair with
    // every permission may.
    let limited = check();
    let kept = versions();
    server.shell_ok(&format!(
        "printf operator > {DEPLOYMENT}/secrets/backup_s3_key \
         && printf operator-secret > {DEPLOYMENT}/secrets/backup_s3_key_secret"
    ));
    let unlimited = check();
    let removed = versions();
    let lifecycle = server.shell_ok(
        "docker exec bucket curl --silent --show-error --fail --aws-sigv4 aws:amz:us-east-1:s3 \
         --user operator:operator-secret 'http://localhost:8333/backups?lifecycle'",
    );

    assert_found(
        &limited,
        true,
        &[
            "check: ok    the backups' bucket, backups, keeps what is deleted in it for 7 days, and \
             its key pair can't remove it",
        ],
    );
    // The check's object stays as a version that the bucket keeps, under its delete marker.
    assert_eq!(kept, (1, 1));
    assert_found(
        &unlimited,
        false,
        &[
            "check: FAIL  the backups' bucket, backups, let its key pair delete a version that it \
             kept",
            "check: FAIL  the backups' bucket, backups, let its key pair write its lifecycle rules",
            "check: FAIL  the backups' bucket, backups, let its key pair write its versioning",
            "3 of the checks on the server failed",
        ],
    );
    // The second check's version went, and the writes that the bucket took left its rule.
    assert_eq!(removed, (1, 2));
    assert!(
        lifecycle.contains("<NoncurrentDays>7</NoncurrentDays>"),
        "{lifecycle}"
    );
}

#[tokio::test]
async fn ops_22_the_check_fails_unless_a_bucket_keeps_versions_for_the_days_the_setting_gives() {
    let (server, command) = prepared_server();
    let check = || as_deploy(&server, &command);
    let days = |days: u32| {
        server.shell_ok(&format!(
            "sed -i /^BUCKETS_KEEP_DELETED_DAYS=/d {DEPLOYMENT}/settings.env \
             && echo BUCKETS_KEEP_DELETED_DAYS={days} >> {DEPLOYMENT}/settings.env"
        ));
    };
    let expires_after = |bucket: &str, days: u32| {
        server.configure_bucket(
            bucket,
            "lifecycle",
            &format!(
                "<LifecycleConfiguration><Rule><ID>expire</ID><Filter><Prefix></Prefix></Filter>\
                 <Status>Enabled</Status><NoncurrentVersionExpiration><NoncurrentDays>{days}\
                 </NoncurrentDays></NoncurrentVersionExpiration></Rule></LifecycleConfiguration>"
            ),
        );
    };

    // The backups' bucket expires its noncurrent versions after a day, as Backblaze B2's "Keep
    // only the last version" does, where the setting gives 7 days, then 0, which asks for
    // that; and after 7 days, where the setting gives 0.
    expires_after("backups", 1);
    let expires_sooner = check();
    days(0);
    // part: files
    expires_after("files", 1);
    // end part: files
    let keeps_nothing = check();
    expires_after("backups", 7);
    let keeps_longer = check();
    // part: files
    expires_after("files", 7);
    // end part: files
    days(7);
    // Its versioning is suspended, so it keeps no version of what is deleted from then on.
    server.configure_bucket(
        "backups",
        "versioning",
        "<VersioningConfiguration><Status>Suspended</Status></VersioningConfiguration>",
    );
    let suspended = check();

    assert_found(
        &expires_sooner,
        false,
        &[
            "check: FAIL  the backups' bucket, backups, doesn't expire its noncurrent versions 7 \
             days after they become noncurrent, as BUCKETS_KEEP_DELETED_DAYS says",
            "1 of the checks on the server failed",
        ],
    );
    assert_found(
        &keeps_nothing,
        true,
        &[
            "check: ok    the backups' bucket, backups, keeps versions, and a lifecycle rule \
             expires its noncurrent ones after a day",
        ],
    );
    assert_found(
        &keeps_longer,
        false,
        &[
            "check: FAIL  the backups' bucket, backups, keeps versions, and no enabled lifecycle \
             rule of the whole bucket expires its noncurrent ones after a day",
            "1 of the checks on the server failed",
        ],
    );
    assert_found(
        &suspended,
        false,
        &[
            "check: FAIL  the backups' bucket, backups, has no versioning on, so it keeps nothing \
             of what is deleted in it",
            "1 of the checks on the server failed",
        ],
    );
}

#[tokio::test]
async fn ops_19_the_check_fails_while_the_server_s_clock_isn_t_synchronized() {
    let (server, settings) = new_server();
    let api = Pushed::api().in_network();
    let postgres = Pushed::postgres().in_network();
    let set_up = server.shell("sh /root/setup/set-up.sh secrets");
    assert_set_up(
        &server,
        &set_up,
        &settings,
        &["backup_cipher_pass", "postgres_password"],
    );
    // A stand-in for systemd's timedatectl, which the tests' server lacks, that answers the one
    // question the check asks as systemd does of the kernel's clock, and no other.
    let answers = |synchronized: &str| {
        server.write(
            "/usr/local/bin/timedatectl",
            &format!(
                "#!/bin/sh\n[ \"$*\" = 'show --property=NTPSynchronized --value' ] || exit 2\n\
                 echo {synchronized}\n"
            ),
        );
        server.shell_ok("chmod +x /usr/local/bin/timedatectl");
        as_deploy(
            &server,
            &format!("sh {RELEASE}/deploy.sh check-images {api} {postgres}"),
        )
    };

    let unsynchronized = answers("no");
    let synchronized = answers("yes");

    assert_found(
        &unsynchronized,
        false,
        &[
            "check: FAIL  the clock isn't synchronized",
            "1 of the checks on the server failed",
        ],
    );
    assert_found(
        &synchronized,
        true,
        &["check: ok    the clock is synchronized"],
    );
}

/// Whether `function` of deploy/tasks/buckets.sh, `expiring_rules` or `only_expiry`, prints an
/// expiry of noncurrent versions after `days` days for the lifecycle configuration that holds
/// `rules`.
fn expires(function: &str, rules: &str, days: &str) -> bool {
    let buckets = concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/tasks/buckets.sh");
    let lifecycle = format!("<LifecycleConfiguration>{rules}</LifecycleConfiguration>");
    let output = std::process::Command::new("sh")
        .args([
            "-c",
            ". \"$1\" && \"$2\" \"$3\" \"$4\"",
            "sh",
            buckets,
            function,
            &lifecycle,
            days,
        ])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", lossy(&output));
    !output.stdout.is_empty()
}

/// A lifecycle rule with `filter`, `status` and `action`.
fn rule(filter: &str, status: &str, action: &str) -> String {
    format!("<Rule><ID>r</ID>{filter}<Status>{status}</Status>{action}</Rule>")
}

/// The filter of a rule of the whole bucket.
const WHOLE: &str = "<Filter><Prefix></Prefix></Filter>";
/// The filter of a rule of one prefix's objects.
const PREFIX: &str = "<Filter><Prefix>workspaces/</Prefix></Filter>";
/// An expiry of noncurrent versions a day after they became noncurrent.
const AFTER_A_DAY: &str = "<NoncurrentVersionExpiration><NoncurrentDays>1</NoncurrentDays>\
                           </NoncurrentVersionExpiration>";
/// One 7 days after.
const AFTER_A_WEEK: &str = "<NoncurrentVersionExpiration><NoncurrentDays>7</NoncurrentDays>\
                            </NoncurrentVersionExpiration>";

#[test]
fn ops_22_the_check_takes_only_a_rule_that_deletes_noncurrent_versions_after_the_days_asked() {
    let expiring = |rules: &str| expires("expiring_rules", rules, "1");

    // Backblaze B2's "Keep only the last version of the file".
    assert!(expiring(&rule(WHOLE, "Enabled", AFTER_A_DAY)));
    assert!(!expiring(&rule(WHOLE, "Disabled", AFTER_A_DAY)));
    assert!(!expiring(&rule(PREFIX, "Enabled", AFTER_A_DAY)));
    assert!(!expiring(&rule(
        WHOLE,
        "Enabled",
        "<NoncurrentVersionTransition><NoncurrentDays>1</NoncurrentDays>\
         <StorageClass>GLACIER</StorageClass></NoncurrentVersionTransition>"
    )));
    assert!(!expiring(&rule(
        WHOLE,
        "Enabled",
        "<NoncurrentVersionExpiration><NoncurrentDays>1</NoncurrentDays>\
         <NewerNoncurrentVersions>3</NewerNoncurrentVersions></NoncurrentVersionExpiration>"
    )));
    assert!(!expiring(&rule(WHOLE, "Enabled", AFTER_A_WEEK)));
    // The days that the setting gives, and no others, such as those that start with them.
    let after_a_week = |rules: &str| expires("expiring_rules", rules, "7");
    assert!(after_a_week(&rule(WHOLE, "Enabled", AFTER_A_WEEK)));
    assert!(!after_a_week(&rule(WHOLE, "Enabled", AFTER_A_DAY)));
    assert!(!after_a_week(&rule(
        WHOLE,
        "Enabled",
        "<NoncurrentVersionExpiration><NoncurrentDays>70</NoncurrentDays>\
         </NoncurrentVersionExpiration>"
    )));
    assert!(!after_a_week(&rule(WHOLE, "Disabled", AFTER_A_WEEK)));
}

#[test]
fn ops_22_the_check_takes_a_lifecycle_s_expiry_of_noncurrent_versions_only_when_it_holds_no_other()
{
    let only = |rules: &str| expires("only_expiry", rules, "7");
    let kept = rule(WHOLE, "Enabled", AFTER_A_WEEK);
    // A second expiry, of one prefix's versions, could expire those sooner.
    let sooner = rule(PREFIX, "Enabled", AFTER_A_DAY);

    assert!(only(&kept));
    assert!(!only(&format!("{kept}{sooner}")));
    assert!(!only(&format!("{kept}{kept}")));
    assert!(!only(&rule(WHOLE, "Enabled", AFTER_A_DAY)));
    assert!(!only(""));
}

/// Asserts that the check that wrote `output` passed, or failed, as `passed` says, and wrote each
/// of `lines`.
fn assert_found(output: &std::process::Output, passed: bool, lines: &[&str]) {
    assert_eq!(output.status.success(), passed, "{}", lossy(output));
    for line in lines {
        assert!(lossy(output).contains(line), "{line}: {}", lossy(output));
    }
}
