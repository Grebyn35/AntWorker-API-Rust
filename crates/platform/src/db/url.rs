//! Connection URLs.

use std::{fmt, str::FromStr};

use percent_encoding::percent_decode_str;
use sqlx::{ConnectOptions, postgres::PgConnectOptions};
use url::Url;

use crate::types::Secret;

/// The query parameters a database URL may set. The user, password, host, port and database
/// come from the URL itself, with port 5432 when it leaves the port out, so a parameter that
/// would replace one of them is refused, and so is any other parameter.
const QUERY_PARAMETERS: [&str; 5] = [
    "sslmode",
    "sslrootcert",
    "sslcert",
    "sslkey",
    "application_name",
];

/// A PostgreSQL connection URL, such as `postgres://app:password@localhost:5432/antworker`.
///
/// It names a user, a password, a host and a database, and a port unless it is 5432, and sets
/// no query parameter outside `QUERY_PARAMETERS`. The password is a secret (SEC-01): `Debug`
/// leaves it out.
#[derive(Clone)]
pub struct DatabaseUrl {
    options: PgConnectOptions,
    user: String,
    password: Secret<String>,
    database: String,
}

impl DatabaseUrl {
    /// The database the URL connects to.
    pub fn database(&self) -> &str {
        &self.database
    }

    /// The same URL, connecting to `database` instead.
    pub fn with_database(&self, database: &str) -> Self {
        Self {
            options: self.options.clone().database(database),
            user: self.user.clone(),
            password: self.password.clone(),
            database: database.to_owned(),
        }
    }

    /// The options to connect with.
    pub fn connect_options(&self) -> PgConnectOptions {
        self.options.clone()
    }

    pub(crate) fn password(&self) -> &Secret<String> {
        &self.password
    }
}

/// The URL isn't a PostgreSQL URL with a user, a password, a host and a database, or it sets a
/// query parameter outside `QUERY_PARAMETERS`. It says nothing more, because the URL holds a
/// password.
#[derive(Debug, PartialEq, Eq)]
pub struct InvalidDatabaseUrl;

impl FromStr for DatabaseUrl {
    type Err = InvalidDatabaseUrl;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let url = Url::parse(value).map_err(|_| InvalidDatabaseUrl)?;
        if !matches!(url.scheme(), "postgres" | "postgresql")
            || url.host_str().is_none()
            || url
                .query_pairs()
                .any(|(name, _)| !QUERY_PARAMETERS.contains(&name.as_ref()))
        {
            return Err(InvalidDatabaseUrl);
        }
        let user = decode(url.username())?;
        let password = decode(url.password().unwrap_or_default())?;
        let database = decode(url.path().trim_start_matches('/'))?;
        let options = PgConnectOptions::from_url(&url).map_err(|_| InvalidDatabaseUrl)?;
        Ok(Self {
            options,
            user,
            password: Secret::new(password),
            database,
        })
    }
}

/// A percent-decoded, non-empty URL component.
fn decode(component: &str) -> Result<String, InvalidDatabaseUrl> {
    let decoded = percent_decode_str(component)
        .decode_utf8()
        .map_err(|_| InvalidDatabaseUrl)?;
    if decoded.is_empty() {
        return Err(InvalidDatabaseUrl);
    }
    Ok(decoded.into_owned())
}

impl fmt::Debug for DatabaseUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DatabaseUrl")
            .field("user", &self.user)
            .field("host", &self.options.get_host())
            .field("port", &self.options.get_port())
            .field("database", &self.database)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ops_01_a_database_url_names_its_parts() {
        let url: DatabaseUrl = "postgres://app:s%40cret@db.internal:6543/antworker?sslmode=require"
            .parse()
            .unwrap();
        assert_eq!(url.user, "app");
        assert_eq!(url.password().expose(), "s@cret");
        assert_eq!(url.database(), "antworker");
        assert_eq!(url.connect_options().get_port(), 6543);
    }

    #[test]
    fn ops_01_a_database_url_needs_a_user_password_host_and_database() {
        for value in [
            "postgres://localhost:5432/antworker",
            "postgres://app@localhost:5432/antworker",
            "postgres://app:secret@localhost:5432",
            "postgres://app:secret@localhost:5432/",
            "mysql://app:secret@localhost:3306/antworker",
            "not a url",
        ] {
            assert_eq!(
                value.parse::<DatabaseUrl>().err(),
                Some(InvalidDatabaseUrl),
                "{value}"
            );
        }
    }

    #[test]
    fn ops_01_a_database_url_allows_only_its_documented_query_parameters() {
        let allowed = "postgres://app:secret@db:5432/antworker?sslmode=verify-full\
                       &sslrootcert=/run/ca.pem&sslcert=/run/c.pem&sslkey=/run/k.pem\
                       &application_name=api";
        assert!(allowed.parse::<DatabaseUrl>().is_ok());
        for parameter in [
            "dbname=other",
            "user=other",
            "password=other",
            "host=other",
            "hostaddr=10.0.0.1",
            "port=6543",
            "options=-c%20statement_timeout%3D0",
            "connect_timeout=5",
        ] {
            let url = format!("postgres://app:secret@db:5432/antworker?{parameter}");
            assert_eq!(
                url.parse::<DatabaseUrl>().err(),
                Some(InvalidDatabaseUrl),
                "{parameter}"
            );
        }
    }

    #[test]
    fn ops_02_env_example_names_every_allowed_query_parameter() {
        let example = include_str!("../../../../.env.example");
        for parameter in QUERY_PARAMETERS {
            assert!(example.contains(parameter), "{parameter}");
        }
    }

    #[test]
    fn sec_01_a_database_url_never_prints_its_password() {
        let url: DatabaseUrl = "postgres://app:hunter2@localhost:5432/antworker"
            .parse()
            .unwrap();
        let debug = format!("{url:?}");
        assert!(!debug.contains("hunter2"));
        assert!(debug.contains("antworker"));
    }
}
