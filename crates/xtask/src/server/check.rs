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

/// Whether the server's port `port` answered at `address`: a connection, or its refusal, comes
/// from the server, while no answer at all means a firewall drops the port, or the server is
/// down.
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
/// SSH reaches at `host` needs, and changes nothing.
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
    fn ops_22_an_address_that_points_elsewhere_or_nowhere_fails_the_check() {
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
    fn ops_22_a_port_that_answers_or_refuses_passes_and_one_that_doesn_t_answer_fails() {
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
}
