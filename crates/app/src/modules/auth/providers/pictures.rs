//! Where the pictures of the providers' users come from (AUTH-097): each provider's entry names
//! the hosts, and the copy of a new account's picture fetches from the hosts of the providers
//! that the deployment configures, and from no other (ACCOUNTS-028, SEC-10).

use platform::config::{ConfigError, Settings};

use super::{Entry, Identity, On, Providers};

/// Where the pictures of a provider's users come from, as its entry states it (AUTH-097).
pub(super) struct Pictures {
    /// The hosts that the provider's file names, when the provider serves its users' pictures
    /// from hosts of its own.
    pub(super) hosts: &'static [&'static str],
    /// The setting that lists the hosts, for a provider whose hosts differ between deployments,
    /// such as `STAND_IN_PICTURE_HOSTS`: a deployment may set it while it sets a client of the
    /// provider's.
    pub(super) setting: Option<&'static str>,
}

const HOSTS_EXPECTED: &str = "the names of hosts, separated by commas, each in lower case, with \
     no scheme, port or path (AUTH-097)";

/// The longest a host's name is, and the longest each of its labels, in characters, as the
/// Domain Name System bounds them.
const HOST_MAX_CHARS: usize = 253;
const LABEL_MAX_CHARS: usize = 63;

/// The hosts that the pictures of the users of `entry`'s provider come from: those that its file
/// names, and those that its setting lists. The setting is the deployment's to leave unset, and
/// one that is set while no client of the provider's is, which `configured` says, asks for the
/// client, as the provider's other settings do (AUTH-081).
pub(super) fn hosts<E, F>(
    settings: &Settings<E, F>,
    entry: &Entry,
    configured: bool,
) -> Result<Vec<String>, ConfigError>
where
    E: Fn(&str) -> Option<std::ffi::OsString>,
    F: Fn(&std::path::Path) -> std::io::Result<String>,
{
    let named = entry.pictures.hosts.iter().map(|host| (*host).to_owned());
    let mut hosts: Vec<String> = named.collect();
    let Some(setting) = entry.pictures.setting else {
        return Ok(hosts);
    };
    let Some(listed) = settings.text_setting(setting)? else {
        return Ok(hosts);
    };
    if let Some(prefix) = entry.clients.first().filter(|_| !configured) {
        return Err(ConfigError::missing(&format!("{prefix}CLIENT_ID")));
    }
    for name in listed.split(',').map(str::trim) {
        if !is_host(name) {
            return Err(ConfigError::invalid(setting, HOSTS_EXPECTED));
        }
        hosts.push(name.to_owned());
    }
    Ok(hosts)
}

/// Whether `name` is the name of a host as a URL holds it, which the untrusted client compares
/// it with: labels of lower-case letters, digits and hyphens, separated by dots, none empty,
/// none of more than 63 characters and none starting or ending with a hyphen, of 253 characters
/// at most together, that a URL's parser reads as a name, and not as an IP address.
fn is_host(name: &str) -> bool {
    let labels = name.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= LABEL_MAX_CHARS
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    });
    labels
        && name.len() <= HOST_MAX_CHARS
        && matches!(url::Host::parse(name), Ok(url::Host::Domain(domain)) if domain == name)
}

impl On {
    /// The address of the picture that the provider gave in `identity`, unless the provider
    /// names no host that its users' pictures come from, so no copy could fetch it (AUTH-092).
    pub(in crate::modules::auth) fn picture<'a>(&self, identity: &'a Identity) -> Option<&'a str> {
        identity
            .picture
            .as_deref()
            .filter(|_| !self.picture_hosts.is_empty())
    }
}

impl Providers {
    /// The hosts that the pictures of the users of the providers that the deployment configures
    /// come from, each once (AUTH-097).
    pub(crate) fn picture_hosts(&self) -> Vec<String> {
        let each = self.on.iter().flat_map(|provider| &provider.picture_hosts);
        let mut hosts: Vec<String> = each.cloned().collect();
        hosts.sort_unstable();
        hosts.dedup();
        hosts
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::auth::providers::REGISTRY;

    #[test]
    fn auth_097_a_listed_host_is_a_name_in_lower_case_as_a_url_holds_it() {
        for name in [
            "pictures.example.com",
            "lh3.googleusercontent.com",
            "xn--bcher-kva.example",
            "a-b.example",
            "localhost",
        ] {
            assert!(is_host(name), "{name}");
        }
        for other in [
            "",
            "Pictures.example.com",
            "pictures.example.com.",
            ".example.com",
            "pictures..example.com",
            "*.example.com",
            "-a.example.com",
            "a-.example.com",
            "pictures_1.example.com",
            "bücher.example",
            "https://pictures.example.com",
            "pictures.example.com:443",
            "pictures.example.com/a",
            "pictures.example.com?a",
            "ada@pictures.example.com",
            "pictures example.com",
            "8.8.8.8",
            "127.1",
            "0x7f.1",
            "[::1]",
            "example.123",
        ] {
            assert!(!is_host(other), "{other}");
        }
        assert!(is_host(&format!("{}.example", "a".repeat(63))));
        assert!(!is_host(&format!("{}.example", "a".repeat(64))));
        let long = [["a".repeat(63).as_str(); 3].join("."), "a".repeat(61)].join(".");
        assert_eq!(long.len(), 253);
        assert!(is_host(&long));
        assert!(!is_host(&format!("a{long}")));
    }

    #[test]
    fn auth_097_each_host_that_a_provider_s_file_names_is_one_the_client_can_match() {
        for entry in REGISTRY {
            for host in entry.pictures.hosts {
                assert!(is_host(host), "{}: {host}", entry.key);
            }
        }
    }
}
