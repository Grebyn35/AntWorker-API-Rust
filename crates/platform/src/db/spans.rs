//! The spans of the database's transactions (PLATFORM-104, OBS-06): one for each unit of work,
//! and one for each statement that runs on the pool by itself and commits at once. Each is a
//! child of the span it opens in. Outside every span, neither exists: its span would have no
//! parent, and so would start a trace of its own, as at every claim of a dispatcher.

use tracing::Span;

use super::Database;
use crate::telemetry::names;

/// What the semantic conventions name Postgres, as a span's `db.system.name`.
const SYSTEM_NAME: &str = "postgresql";

impl Database {
    /// The span of a unit of work that opens now, which lasts until its last holder drops it,
    /// or no span outside every span.
    pub(super) fn transaction(&self) -> Span {
        if Span::current().is_none() {
            return Span::none();
        }
        tracing::info_span!(
            "transaction",
            { names::OTEL_KIND } = "client",
            { names::DB_SYSTEM_NAME } = SYSTEM_NAME,
            { names::DB_NAMESPACE } = self.options.get_database().unwrap_or_default(),
        )
    }

    /// The span of a statement that commits by itself, named by `summary`, the statement's
    /// operation and its table, such as `INSERT rate_limit_buckets`, as the conventions name a
    /// database client's span, or no span outside every span.
    pub(super) fn statement(&self, summary: &'static str) -> Span {
        if Span::current().is_none() {
            return Span::none();
        }
        tracing::info_span!(
            "statement",
            { names::OTEL_NAME } = summary,
            { names::OTEL_KIND } = "client",
            { names::DB_SYSTEM_NAME } = SYSTEM_NAME,
            { names::DB_NAMESPACE } = self.options.get_database().unwrap_or_default(),
            { names::DB_QUERY_SUMMARY } = summary,
        )
    }
}
