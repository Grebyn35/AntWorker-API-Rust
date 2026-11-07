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
const SET_UP: &str = r#"set -e
encode() { base64 < "$1" | tr -d '\n'; }
{
    printf 'settings %s\n' "$(encode /root/setup/settings.env)"
    if [ "${1:-}" = secrets ]; then
        printf 'e2e' > /root/setup/key
        printf 'e2e-secret' > /root/setup/key_secret
        printf 'e2e-token' > /root/setup/token
        printf 'secret backup_s3_key %s\n' "$(encode /root/setup/key)"
        printf 'secret backup_s3_key_secret %s\n' "$(encode /root/setup/key_secret)"
        printf 'secret files_copy_s3_key %s\n' "$(encode /root/setup/key)"
        printf 'secret files_copy_s3_key_secret %s\n' "$(encode /root/setup/key_secret)"
        printf 'https://hooks.example.invalid/page' > /root/setup/page_url
        printf 'secret alertmanager/page_url %s\n' "$(encode /root/setup/page_url)"
        printf 'cert backup-ca.crt %s\n' "$(encode /root/bucket/ca.crt)"
        printf 'cert files-copy-ca.crt %s\n' "$(encode /root/bucket/ca.crt)"
        printf 'login registry:5000 e2e %s\n' "$(encode /root/setup/token)"
        if [ -s /root/setup/postgres_password ]; then
            printf 'secret postgres_password %s\n' "$(encode /root/setup/postgres_password)"
        fi
    fi
} > /root/setup/payload
directory=$(mktemp -d)
cp -r /root/release/. "$directory"
sh "$directory/server-setup.sh" apply < /root/setup/payload
"#;

/// Runs `command` in a shell of the deployment's user, with the deployment's directory.
fn as_deploy(server: &Server, command: &str) -> std::process::Output {
    server.shell(&format!(
        "su -s /bin/sh deploy -c 'DEPLOYMENT={DEPLOYMENT} {command}'"
    ))
}

/// A new server, with a stand-in bucket, the setup's script and its settings: the bundled
/// database, which backs up to the bucket, the bundled files bucket, whose copy goes to the
/// bucket's stand-in too, and no telemetry. Returns the server and the settings.
fn new_server() -> (Server, String) {
    let server = Server::start();
    let mut settings = format!(
        "IMAGE_REPOSITORY=registry:5000/image\nSOURCE=https://example.invalid/e2e\n\
         API_ADDRESS={API_ADDRESS}\nTELEMETRY=off\nEMAIL=off\nDATABASE=bundled\nAPI_REPLICAS=2\n\
         DATABASE_MAX_CONNECTIONS=2\n"
    );
    for (name, value) in server
        .start_bucket()
        .into_iter()
        .chain(server.start_files_copy_bucket())
    {
        settings.push_str(&format!("{name}={value}\n"));
    }
    server.write("/root/setup/settings.env", &settings);
    server.write("/root/setup/set-up.sh", SET_UP);
    server.shell_ok(&format!(
        "cp -r /root/release {RELEASE} && chmod -R a+rX {RELEASE}"
    ));
    (server, settings)
}

/// Asserts what the first setup of a new server did, which wrote `output`, and that it wrote
/// `settings`, and generated the secrets `generated` and no others. Returns the passphrase.
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
    for name in [
        "backup_cipher_pass",
        "postgres_password",
        "files_copy_cipher_pass",
    ] {
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
    let passphrase = assert_set_up(
        &server,
        &first,
        &settings,
        &["backup_cipher_pass", "files_copy_cipher_pass"],
    );
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
        "check: ok    the backups' bucket, backups, keeps no versions",
        "check: ok    openssl, which issues the bundled files bucket's certificate",
        "check: ok    the bucket files-copy of the files' copy answers to its key pair",
        "check: ok    the bucket of the files' copy, files-copy, keeps no versions",
        "check: note  whether the disk is encrypted",
        "deploy: every check on the server passed",
    ] {
        assert!(output.contains(line), "{line}: {output}");
    }
}

#[tokio::test]
async fn ops_19_the_check_finds_a_bucket_that_keeps_versions_a_key_pair_it_refuses_and_a_missing_setting()
 {
    let (server, settings) = new_server();
    let api = Pushed::api().in_network();
    let postgres = Pushed::postgres().in_network();
    let set_up = server.shell("sh /root/setup/set-up.sh secrets");
    assert_set_up(
        &server,
        &set_up,
        &settings,
        &[
            "backup_cipher_pass",
            "postgres_password",
            "files_copy_cipher_pass",
        ],
    );
    let check = || {
        as_deploy(
            &server,
            &format!("sh {RELEASE}/deploy.sh check-images {api} {postgres}"),
        )
    };

    // The copy's bucket keeps versions, as Backblaze B2's every bucket does, with no rule that
    // expires them, then with the rule that B2's "Keep only the last version" stands for.
    configure_copy_bucket(
        &server,
        "versioning",
        "<VersioningConfiguration><Status>Enabled</Status></VersioningConfiguration>",
    );
    let keeps_versions = check();
    configure_copy_bucket(
        &server,
        "lifecycle",
        "<LifecycleConfiguration><Rule><ID>keep-only-the-last-version</ID><Filter><Prefix></Prefix>\
         </Filter><Status>Enabled</Status><NoncurrentVersionExpiration><NoncurrentDays>1\
         </NoncurrentDays></NoncurrentVersionExpiration></Rule></LifecycleConfiguration>",
    );
    let expires_them = check();
    server.shell_ok(&format!(
        "printf wrong > {DEPLOYMENT}/secrets/backup_s3_key_secret"
    ));
    let refused_key = check();
    server.shell_ok(&format!(
        "sed -i /^API_ADDRESS=/d {DEPLOYMENT}/settings.env"
    ));
    let missing_setting = check();

    assert_found(
        &keeps_versions,
        false,
        &[
            "check: FAIL  the bucket of the files' copy, files-copy, keeps versions, and no \
             enabled lifecycle rule of the whole bucket expires its noncurrent ones after a day",
            "1 of the checks on the server failed",
        ],
    );
    assert_found(
        &expires_them,
        true,
        &[
            "check: ok    the bucket of the files' copy, files-copy, keeps versions, and a \
           lifecycle rule expires its noncurrent ones after a day",
        ],
    );
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

/// Asserts that the check that wrote `output` passed, or failed, as `passed` says, and wrote each
/// of `lines`.
fn assert_found(output: &std::process::Output, passed: bool, lines: &[&str]) {
    assert_eq!(output.status.success(), passed, "{}", lossy(output));
    for line in lines {
        assert!(lossy(output).contains(line), "{line}: {}", lossy(output));
    }
}

/// Sends the stand-in's bucket `files-copy` the configuration `body` of its `query`, such as
/// `versioning`, as S3's PUT of a bucket's configuration does.
fn configure_copy_bucket(server: &Server, query: &str, body: &str) {
    server.shell_ok(&format!(
        "docker exec bucket curl --silent --show-error --fail --aws-sigv4 aws:amz:us-east-1:s3 \
         --user e2e:e2e-secret -X PUT --data '{body}' 'http://localhost:8333/files-copy?{query}'"
    ));
}
