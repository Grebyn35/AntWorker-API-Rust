//! Raw connections for the test support, which creates databases and roles and arranges tests
//! with statements no unit of work may run (TEST-06). Compiled only with the `test-support`
//! feature, which only the tests enable, so the API never hands out a connection.

use sqlx::{ConnectOptions, PgConnection};

use super::DatabaseUrl;

/// A connection with `url`, outside any unit of work.
#[expect(
    clippy::disallowed_methods,
    reason = "the test support's raw connections are the one other place DATA-21 allows"
)]
pub async fn connect(url: &DatabaseUrl) -> Result<PgConnection, sqlx::Error> {
    url.connect_options().connect().await
}

/// The host and port that `url` connects to.
pub fn address(url: &DatabaseUrl) -> (String, u16) {
    let options = url.connect_options();
    (options.get_host().to_owned(), options.get_port())
}
