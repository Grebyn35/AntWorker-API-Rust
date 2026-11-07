//! The settings of the bucket that holds files' bytes (PLATFORM-076), which `serve` reads with
//! the rest of its settings.

use std::{ffi::OsString, io, path::Path};

use url::Url;

use super::{ConfigError, Environment, Settings};
use crate::{storage::StorageConfig, types::Secret};

const ENDPOINT_EXPECTED: &str = "an https URL with a host and no path, user, password, query or \
     fragment, or http in development and test";

impl<E, F> Settings<E, F>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> io::Result<String>,
{
    /// The bucket's settings: every one required but `S3_URI_STYLE`, and an `https` endpoint
    /// outside development and test (SEC-10).
    pub(super) fn storage(&self, environment: Environment) -> Result<StorageConfig, ConfigError> {
        let plain_allowed = matches!(environment, Environment::Development | Environment::Test);
        let endpoint = self.with("S3_ENDPOINT", ENDPOINT_EXPECTED, |value| {
            Url::parse(value).ok().filter(|url| {
                let scheme_allowed =
                    url.scheme() == "https" || (plain_allowed && url.scheme() == "http");
                scheme_allowed
                    && url.host_str().is_some()
                    && url.username().is_empty()
                    && url.password().is_none()
                    && url.path() == "/"
                    && url.query().is_none()
                    && url.fragment().is_none()
            })
        })?;
        let text = |value: &str| Some(value.to_owned());
        let path_style = self
            .optional_with("S3_URI_STYLE", "host or path", |value| match value {
                "host" => Some(false),
                "path" => Some(true),
                _ => None,
            })?
            .unwrap_or(false);
        Ok(StorageConfig {
            endpoint,
            region: self.with("S3_REGION", "a region's name", text)?,
            bucket: self.with("S3_BUCKET", "a bucket's name", text)?,
            access_key_id: self.with("S3_ACCESS_KEY_ID", "an access key's ID", text)?,
            secret_access_key: self.with(
                "S3_SECRET_ACCESS_KEY",
                "an access key's secret",
                |value| Some(Secret::new(value.to_owned())),
            )?,
            path_style,
            ca_path: self.optional_with("S3_CA_PATH", "a file's path", |value| {
                Some(std::path::PathBuf::from(value))
            })?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    /// The bucket's settings in `environment`, from `variables`.
    fn read(environment: Environment, variables: &[(&str, &str)]) -> Result<StorageConfig, String> {
        let variables: HashMap<String, String> = variables
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect();
        let settings = Settings::new(
            |name: &str| variables.get(name).map(OsString::from),
            |_: &Path| Err(io::Error::other("no files")),
        );
        settings
            .storage(environment)
            .map_err(|error| error.to_string())
    }

    const VALID: [(&str, &str); 5] = [
        ("S3_ENDPOINT", "https://s3.eu-central-003.backblazeb2.com"),
        ("S3_REGION", "eu-central-003"),
        ("S3_BUCKET", "files"),
        ("S3_ACCESS_KEY_ID", "key"),
        ("S3_SECRET_ACCESS_KEY", "secret"),
    ];

    /// `VALID` without `name`, and with `extra`.
    fn changed<'a>(name: &str, extra: &[(&'a str, &'a str)]) -> Vec<(&'a str, &'a str)> {
        VALID
            .iter()
            .copied()
            .filter(|(each, _)| *each != name)
            .chain(extra.iter().copied())
            .collect()
    }

    #[test]
    fn platform_076_every_bucket_setting_but_the_uri_style_is_required() {
        let config = read(Environment::Production, &VALID).unwrap();
        assert!(!config.path_style);
        assert_eq!(config.bucket, "files");
        for (name, _) in VALID {
            let error = read(Environment::Production, &changed(name, &[])).unwrap_err();
            assert!(error.contains(name), "{error}");
        }
    }

    #[test]
    fn platform_076_the_endpoint_is_https_outside_development_and_test() {
        let plain = changed("S3_ENDPOINT", &[("S3_ENDPOINT", "http://localhost:8333")]);
        assert!(read(Environment::Production, &plain).is_err());
        assert!(read(Environment::Development, &plain).is_ok());
        assert!(read(Environment::Test, &plain).is_ok());
        for endpoint in [
            "https://s3.example.com/bucket",
            "https://key:secret@s3.example.com",
            "https://s3.example.com?x=1",
            "s3.example.com",
        ] {
            let other = changed("S3_ENDPOINT", &[("S3_ENDPOINT", endpoint)]);
            assert!(read(Environment::Production, &other).is_err(), "{endpoint}");
        }
    }

    #[test]
    fn platform_076_the_uri_style_is_host_or_path() {
        let path = changed("", &[("S3_URI_STYLE", "path")]);
        assert!(read(Environment::Production, &path).unwrap().path_style);
        let other = changed("", &[("S3_URI_STYLE", "virtual")]);
        assert!(read(Environment::Production, &other).is_err());
    }
}
