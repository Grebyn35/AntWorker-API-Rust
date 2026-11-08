//! The setting of the malware scanner (PLATFORM-077), which `serve` reads with the rest of its
//! settings.

use std::{ffi::OsString, io, path::Path};

use super::{ConfigError, Settings};
use crate::scanner::ScannerConfig;

const ADDRESS_EXPECTED: &str = "a host and a port, such as scanner:3310";

impl<E, F> Settings<E, F>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> io::Result<String>,
{
    /// The scanner's address, `CLAMD_ADDRESS`: a host, an IPv6 address in brackets among them,
    /// and a port.
    pub(super) fn scanner(&self) -> Result<ScannerConfig, ConfigError> {
        self.with("CLAMD_ADDRESS", ADDRESS_EXPECTED, address)
    }
}

/// The host and the port that `value` names, if it names both.
fn address(value: &str) -> Option<ScannerConfig> {
    let (host, port) = value.rsplit_once(':')?;
    let host = match host.strip_prefix('[') {
        Some(bracketed) => bracketed.strip_suffix(']')?,
        None if host.contains(':') => return None,
        None => host,
    };
    let port: u16 = port.parse().ok().filter(|port| *port > 0)?;
    let valid = !host.is_empty()
        && host
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || ".-_:".contains(character));
    valid.then(|| ScannerConfig {
        host: host.to_owned(),
        port,
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// The scanner's setting from `variables`.
    fn read(variables: &[(&str, &str)]) -> Result<ScannerConfig, String> {
        let variables: HashMap<String, String> = variables
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let settings = Settings::new(
            |name: &str| variables.get(name).map(OsString::from),
            |_: &Path| Err(io::Error::other("no files")),
        );
        settings.scanner().map_err(|error| error.to_string())
    }

    #[test]
    fn platform_077_the_scanner_s_address_is_required() {
        let error = read(&[]).unwrap_err();
        assert!(error.contains("CLAMD_ADDRESS"), "{error}");
    }

    #[test]
    fn platform_077_the_scanner_s_address_is_a_host_and_a_port() {
        let config = read(&[("CLAMD_ADDRESS", "scanner:3310")]).unwrap();
        assert_eq!(
            config,
            ScannerConfig {
                host: "scanner".to_owned(),
                port: 3310
            }
        );
        let config = read(&[("CLAMD_ADDRESS", "[::1]:3310")]).unwrap();
        assert_eq!(config.host, "::1");
        for address in [
            "scanner",
            "scanner:",
            ":3310",
            "scanner:0",
            "scanner:65536",
            "::1:3310",
            "tcp://scanner:3310",
            "scanner/x:3310",
        ] {
            let error = read(&[("CLAMD_ADDRESS", address)]).unwrap_err();
            assert!(error.contains("CLAMD_ADDRESS"), "{address}: {error}");
        }
    }
}
