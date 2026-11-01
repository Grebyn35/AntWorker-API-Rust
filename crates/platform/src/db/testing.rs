//! Raw connections for the test support, which creates databases and roles and arranges tests
//! with statements no unit of work may run (TEST-06). Compiled only with the `test-support`
//! feature, which only the tests enable, so the API never hands out a connection.

use sqlx::{
    ConnectOptions, PgConnection,
    postgres::{PgListener, PgPoolOptions},
};

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

/// A listener with `url` on `channel`, for the tests of the signals a unit of work sends.
#[expect(
    clippy::disallowed_types,
    reason = "the test support's raw connections are the one other place DATA-21 allows"
)]
pub async fn listen(url: &DatabaseUrl, channel: &str) -> Result<PgListener, sqlx::Error> {
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(url.connect_options())
        .await?;
    let mut listener = PgListener::connect_with(&pool).await?;
    listener.listen(channel).await?;
    Ok(listener)
}
