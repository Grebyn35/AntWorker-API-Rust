//! `server-check`: runs deploy.sh's check of a release on the server, then checks from the
//! developer's machine what only the outside sees: where each of the deployment's addresses
//! points, and whether the server's ports 80 and 443 can be reached.

use std::{
    io,
    net::{IpAddr, SocketAddr, TcpStream, ToSocketAddrs},
    process::{Command, ExitCode},
    time::Duration,
};

use super::{
    DEPLOYMENT, on_server_output, send_release,
    settings::{get, settings_of},
};
use crate::deploy::{quoted, refuse, released, root};

/// How long a connection to one of the server's ports may take before the check counts the port
/// as unreachable.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

/// A line of the check's report, in the form deploy.sh's check writes.
#[derive(Debug, PartialEq, Eq)]
enum Finding {
    Passed(String),
    Failed(String),
}

impl Finding {
    fn line(&self) -> String {
        match self {
            Self::Passed(text) => format!("check: ok    {text}"),
            Self::Failed(text) => format!("check: FAIL  {text}"),
        }
    }
}

/// Whether the deployment's address `name`, which DNS resolves to `resolved`, points to the
/// server, which SSH reaches at `server`.
fn address_finding(name: &str, resolved: &[IpAddr], server: &[IpAddr]) -> Finding {
    let shown = |addresses: &[IpAddr]| {
        addresses
            .iter()
            .map(IpAddr::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };
    if resolved.is_empty() {
        return Finding::Failed(format!(
            "{name} resolves to no address; its DNS records must point to the server, {}, before \
             Let's Encrypt can issue its certificate",
            shown(server)
        ));
    }
    if resolved.iter().any(|address| server.contains(address)) {
        Finding::Passed(format!("{name} points to the server, {}", shown(resolved)))
    } else {
        Finding::Failed(format!(
            "{name} points to {}, not to the server, {}; Let's Encrypt can't issue its \
             certificate unless the address reaches the server",
            shown(resolved),
            shown(server)
        ))
    }
}

/// Whether the server answered at `address`, on its port, as `connected`, the attempt to connect
/// there, tells: a connection, or its refusal, comes from the server, while no answer at all means
/// a firewall drops the port, or the server is down.
fn port_finding(address: SocketAddr, connected: &io::Result<TcpStream>) -> Finding {
    match connected {
        Ok(_) => Finding::Passed(format!("port {} of the server answers", address.port())),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => Finding::Passed(format!(
            "port {} of the server can be reached; nothing listens there until the first deploy \
             starts Caddy",
            address.port()
        )),
        Err(error) => Finding::Failed(format!(
            "port {} of the server, at {address}, doesn't answer ({error}): a firewall must \
             admit ports 80 and 443 from anywhere (docs/operations/deploy.md#what-a-server-needs)",
            address.port()
        )),
    }
}

/// The addresses that `name` resolves to.
fn resolve(name: &str) -> Vec<IpAddr> {
    let mut addresses: Vec<IpAddr> = (name, 443)
        .to_socket_addrs()
        .map(|resolved| resolved.map(|address| address.ip()).collect())
        .unwrap_or_default();
    addresses.sort();
    addresses.dedup();
    addresses
}

/// The address that SSH reaches `host` at, as `ssh -G` names it.
fn server_name(host: &str) -> Result<String, String> {
    let output = Command::new("ssh")
        .args(["-G", host])
        .output()
        .map_err(|error| format!("can't run ssh: {error}"))?;
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .find_map(|line| line.strip_prefix("hostname ").map(str::to_owned))
        .ok_or_else(|| format!("ssh -G names no address for {host}"))
}

/// Checks what a deploy of the release `version`, or of the latest release, to the server that
/// SSH reaches at `host` needs, and changes nothing of the deployment but an object that it writes
/// to each bucket and deletes (OPS-22).
#[expect(clippy::print_stdout, reason = "the findings are the task's output")]
pub fn check(host: &str, version: Option<&str>) -> ExitCode {
    let root = root();
    let version = match released(&root, version) {
        Ok(version) => version,
        Err(refused) => return refuse(&refused),
    };
    println!("Checking what a deploy of {version} to {host} needs.");
    let directory = match send_release(&root, host, &version) {
        Ok(directory) => directory,
        Err(error) => return refuse(&format!("Can't copy deploy/ to {host}: {error}")),
    };
    let directory = quoted(&directory);
    let on_server = Command::new("ssh")
        .arg(host)
        .arg(format!(
            "DEPLOYMENT={DEPLOYMENT} sh {directory}/deploy.sh check {}; status=$?; rm -rf {directory}; exit $status",
            quoted(&version)
        ))
        .status();
    let mut failed = !matches!(on_server, Ok(status) if status.success());

    let settings = on_server_output(host, &format!("cat {DEPLOYMENT}/settings.env 2> /dev/null"))
        .map(|text| settings_of(&text))
        .unwrap_or_default();
    let server = match server_name(host) {
        Ok(name) => resolve(&name),
        Err(error) => {
            println!("{}", Finding::Failed(error).line());
            return ExitCode::FAILURE;
        }
    };
    let mut findings = Vec::new();
    let mut addresses = vec![get(&settings, "API_ADDRESS")];
    if matches!(get(&settings, "TELEMETRY"), "self-hosted" | "") {
        addresses.push(get(&settings, "GRAFANA_ADDRESS"));
    }
    for name in addresses.into_iter().filter(|name| !name.is_empty()) {
        findings.push(address_finding(name, &resolve(name), &server));
    }
    if let Some(&ip) = server.first() {
        for port in [80, 443] {
            let address = SocketAddr::new(ip, port);
            findings.push(port_finding(
                address,
                &TcpStream::connect_timeout(&address, CONNECT_TIMEOUT),
            ));
        }
    }
    for finding in &findings {
        println!("{}", finding.line());
        failed |= matches!(finding, Finding::Failed(_));
    }
    if failed {
        println!("Some checks failed; each says what a deploy needs.");
        ExitCode::FAILURE
    } else {
        println!("Every check passed: deploy {version} with just deploy {host} {version}.");
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_19_an_address_that_points_elsewhere_or_nowhere_fails_the_check() {
        let server: Vec<IpAddr> = vec!["203.0.113.7".parse().unwrap()];
        let elsewhere: Vec<IpAddr> = vec!["198.51.100.1".parse().unwrap()];

        assert!(matches!(
            address_finding("api.example.com", &server, &server),
            Finding::Passed(_)
        ));
        assert!(matches!(
            address_finding("api.example.com", &elsewhere, &server),
            Finding::Failed(text) if text.contains("198.51.100.1")
        ));
        assert!(matches!(
            address_finding("api.example.com", &[], &server),
            Finding::Failed(text) if text.contains("resolves to no address")
        ));
    }

    #[test]
    fn ops_19_a_port_that_answers_or_refuses_passes_and_one_that_doesn_t_answer_fails() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let open = listener.local_addr().unwrap();
        let address = SocketAddr::from(([127, 0, 0, 1], 443));
        let refused = Err(io::Error::from(io::ErrorKind::ConnectionRefused));
        let dropped = Err(io::Error::from(io::ErrorKind::TimedOut));

        assert!(matches!(
            port_finding(address, &refused),
            Finding::Passed(text) if text.contains("nothing listens")
        ));
        assert!(matches!(
            port_finding(address, &dropped),
            Finding::Failed(text) if text.contains("firewall")
        ));
        assert!(matches!(
            port_finding(open, &TcpStream::connect_timeout(&open, CONNECT_TIMEOUT)),
            Finding::Passed(_)
        ));
    }

    /// What deploy.sh's check on the server reads of a bucket's lifecycle (OPS-22), in
    /// deploy/tasks/buckets.sh. The tests run the script in a POSIX shell, as the server does,
    /// which Windows doesn't provide, so they compile only on Unix.
    #[cfg(unix)]
    mod lifecycle {
        use super::*;

        /// Whether `function` of deploy/tasks/buckets.sh, `expiring_rules` or `only_expiry`,
        /// prints an expiry of noncurrent versions after `days` days for the lifecycle
        /// configuration that holds `rules`.
        fn expires(function: &str, rules: &str, days: &str) -> bool {
            let lifecycle = format!("<LifecycleConfiguration>{rules}</LifecycleConfiguration>");
            let output = Command::new("sh")
                .args(["-c", ". \"$1\" && \"$2\" \"$3\" \"$4\"", "sh"])
                .arg(root().join("deploy/tasks/buckets.sh"))
                .args([function, &lifecycle, days])
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
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
        /// The filters of a rule of some of the objects under every prefix: those that hold a
        /// tag, those larger, or smaller, than a size, and those that hold a tag, with an empty
        /// prefix.
        const OTHERS: [&str; 4] = [
            "<Filter><Tag><Key>kind</Key><Value>upload</Value></Tag></Filter>",
            "<Filter><ObjectSizeGreaterThan>1048576</ObjectSizeGreaterThan></Filter>",
            "<Filter><ObjectSizeLessThan>1048576</ObjectSizeLessThan></Filter>",
            "<Filter><And><Prefix></Prefix><Tag><Key>kind</Key><Value>upload</Value></Tag></And>\
             </Filter>",
        ];
        /// An expiry of noncurrent versions a day after they became noncurrent.
        const AFTER_A_DAY: &str = "<NoncurrentVersionExpiration><NoncurrentDays>1</NoncurrentDays>\
                                   </NoncurrentVersionExpiration>";
        /// One 7 days after.
        const AFTER_A_WEEK: &str = "<NoncurrentVersionExpiration><NoncurrentDays>7\
                                    </NoncurrentDays></NoncurrentVersionExpiration>";

        #[test]
        fn ops_22_the_check_takes_only_a_rule_that_deletes_noncurrent_versions_after_the_days_asked()
         {
            let expiring = |rules: &str| expires("expiring_rules", rules, "1");

            // Backblaze B2's "Keep only the last version of the file".
            assert!(expiring(&rule(WHOLE, "Enabled", AFTER_A_DAY)));
            assert!(!expiring(&rule(WHOLE, "Disabled", AFTER_A_DAY)));
            for filter in [PREFIX].iter().chain(&OTHERS) {
                assert!(!expiring(&rule(filter, "Enabled", AFTER_A_DAY)), "{filter}");
            }
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
    }
}
