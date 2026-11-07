//! The settings of the bucket that holds files' bytes, and of its copy off the server
//! (PLATFORM-076, PLATFORM-077), which `serve` and `restore-files` read.

use std::{ffi::OsString, io, path::Path};

use url::Url;

use super::{ConfigError, Environment, Problem, Settings};
use crate::{
    db::DatabaseUrl,
    storage::{BucketConfig, StorageConfig},
    types::Secret,
};

const ENDPOINT_EXPECTED: &str = "an https URL with a host and no path, user, password, query or \
     fragment, or http in development and test";

/// The settings `restore-files` reads.
#[derive(Clone, Debug)]
pub struct RestoreFilesConfig {
    /// The connection it removes the lost files' rows on, as the role `app`.
    pub database_url: DatabaseUrl,
    /// The bucket of files' bytes, and its copy, which it requires (PLATFORM-077).
    pub storage: StorageConfig,
}

impl<E, F> Settings<E, F>
where
    E: Fn(&str) -> Option<OsString>,
    F: Fn(&Path) -> io::Result<String>,
{
    /// The settings for `restore-files`, whose bucket must have a copy (PLATFORM-077).
    pub fn restore_files(&self) -> Result<RestoreFilesConfig, ConfigError> {
        let environment = self.required("ENVIRONMENT", "development, test or production")?;
        let storage = self.storage(environment)?;
        if storage.copy.is_none() {
            return Err(ConfigError::new("S3_COPY_ENDPOINT", Problem::Missing));
        }
        Ok(RestoreFilesConfig {
            database_url: self.database_url("DATABASE_URL")?,
            storage,
        })
    }

    /// The bucket's settings, and its copy's, if `S3_COPY_ENDPOINT` names one (PLATFORM-077).
    pub(super) fn storage(&self, environment: Environment) -> Result<StorageConfig, ConfigError> {
        Ok(StorageConfig {
            bucket: self.bucket("S3_", environment)?,
            copy: self.copy(environment)?,
        })
    }

    /// The copy's settings, `S3_COPY_` and the name of the bucket's: none while
    /// `S3_COPY_ENDPOINT` is unset, which leaves each of the others unset too.
    fn copy(&self, environment: Environment) -> Result<Option<BucketConfig>, ConfigError> {
        if self.value("S3_COPY_ENDPOINT")?.is_some() {
            return Ok(Some(self.bucket("S3_COPY_", environment)?));
        }
        for name in [
            "REGION",
            "BUCKET",
            "ACCESS_KEY_ID",
            "SECRET_ACCESS_KEY",
            "URI_STYLE",
            "CA_PATH",
        ] {
            let name = format!("S3_COPY_{name}");
            if self.value(&name)?.is_some() {
                return Err(ConfigError::new(&name, Problem::CopyUnset));
            }
        }
        Ok(None)
    }

    /// The settings of the bucket whose names start with `prefix`: every one required but the
    /// URI style and the certificates' file, and an `https` endpoint outside development and test
    /// (SEC-10).
    fn bucket(&self, prefix: &str, environment: Environment) -> Result<BucketConfig, ConfigError> {
        let name = |setting: &str| format!("{prefix}{setting}");
        let plain_allowed = matches!(environment, Environment::Development | Environment::Test);
        let endpoint = self.with(&name("ENDPOINT"), ENDPOINT_EXPECTED, |value| {
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
            .optional_with(&name("URI_STYLE"), "host or path", |value| match value {
                "host" => Some(false),
                "path" => Some(true),
                _ => None,
            })?
            .unwrap_or(false);
        Ok(BucketConfig {
            endpoint,
            region: self.with(&name("REGION"), "a region's name", text)?,
            bucket: self.with(&name("BUCKET"), "a bucket's name", text)?,
            access_key_id: self.with(&name("ACCESS_KEY_ID"), "an access key's ID", text)?,
            secret_access_key: self.with(
                &name("SECRET_ACCESS_KEY"),
                "an access key's secret",
                |value| Some(Secret::new(value.to_owned())),
            )?,
            path_style,
            ca_path: self.optional_with(&name("CA_PATH"), "a file's path", |value| {
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
        assert!(!config.bucket.path_style);
        assert_eq!(config.bucket.bucket, "files");
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
        assert!(
            read(Environment::Production, &path)
                .unwrap()
                .bucket
                .path_style
        );
        let other = changed("", &[("S3_URI_STYLE", "virtual")]);
        assert!(read(Environment::Production, &other).is_err());
    }

    /// `VALID`, with a copy's settings, each but `skipped`.
    fn with_copy(skipped: &str) -> Vec<(&'static str, &'static str)> {
        let copy = [
            (
                "S3_COPY_ENDPOINT",
                "https://s3.eu-central-003.backblazeb2.com",
            ),
            ("S3_COPY_REGION", "eu-central-003"),
            ("S3_COPY_BUCKET", "files-copy"),
            ("S3_COPY_ACCESS_KEY_ID", "copy-key"),
            ("S3_COPY_SECRET_ACCESS_KEY", "copy-secret"),
        ];
        VALID
            .into_iter()
            .chain(copy.into_iter().filter(|(name, _)| *name != skipped))
            .collect()
    }

    #[test]
    fn platform_077_without_a_copy_endpoint_there_is_no_copy() {
        assert!(
            read(Environment::Production, &VALID)
                .unwrap()
                .copy
                .is_none()
        );
        let config = read(Environment::Production, &with_copy("")).unwrap();
        let copy = config.copy.unwrap();
        assert_eq!(copy.bucket, "files-copy");
        assert_eq!(copy.access_key_id, "copy-key");
        assert!(!copy.path_style);
    }

    #[test]
    fn platform_077_a_copy_takes_each_setting_the_bucket_requires() {
        for name in [
            "S3_COPY_REGION",
            "S3_COPY_BUCKET",
            "S3_COPY_ACCESS_KEY_ID",
            "S3_COPY_SECRET_ACCESS_KEY",
        ] {
            let error = read(Environment::Production, &with_copy(name)).unwrap_err();
            assert!(error.contains(name), "{error}");
        }
        let plain = changed("", &[("S3_COPY_ENDPOINT", "http://localhost:8333")]);
        let plain: Vec<_> = plain
            .into_iter()
            .chain(with_copy("S3_COPY_ENDPOINT").into_iter().skip(VALID.len()))
            .collect();
        assert!(read(Environment::Production, &plain).is_err());
        assert!(read(Environment::Test, &plain).is_ok());
    }

    #[test]
    fn platform_077_a_copy_setting_without_its_endpoint_refuses_startup() {
        for name in [
            "S3_COPY_REGION",
            "S3_COPY_BUCKET",
            "S3_COPY_ACCESS_KEY_ID",
            "S3_COPY_SECRET_ACCESS_KEY",
            "S3_COPY_URI_STYLE",
            "S3_COPY_CA_PATH",
        ] {
            let error = read(Environment::Production, &changed("", &[(name, "x")])).unwrap_err();
            assert!(error.starts_with(name), "{error}");
            assert!(error.contains("S3_COPY_ENDPOINT"), "{error}");
        }
    }
}
