//! Every name that logs and spans give a field, and that metrics give a label (OBS-05). Code
//! uses no other name, and `check-instrument` checks that it doesn't.
//!
//! A name that OpenTelemetry's semantic conventions define is declared with `convention`, which
//! fails compilation when the name differs from the one the conventions publish. The
//! conventions are version 1.42.0, as `opentelemetry-semantic-conventions` 0.33.1 publishes
//! them, and a name they deprecate fails clippy. Any other name is `<term>.<property>`, outside
//! every namespace the conventions define. No name is for personal data or secrets (OBS-04).

use opentelemetry_semantic_conventions::attribute as otel;

/// `name`, which compiles only when it equals `published`, the conventions' own name, such as
/// a constant of `opentelemetry_semantic_conventions`' `attribute` or `metric` module.
pub const fn convention(name: &'static str, published: &'static str) -> &'static str {
    let (a, b) = (name.as_bytes(), published.as_bytes());
    assert!(a.len() == b.len(), "the name differs from the conventions'");
    let mut i = 0;
    while i < a.len() {
        assert!(a[i] == b[i], "the name differs from the conventions'");
        i += 1;
    }
    name
}

/// The name of the service that wrote the line.
pub const SERVICE_NAME: &str = convention("service.name", otel::SERVICE_NAME);
/// The version of the service that wrote the line.
pub const SERVICE_VERSION: &str = convention("service.version", otel::SERVICE_VERSION);
/// The process that exported the telemetry: a UUID drawn when it starts.
pub const SERVICE_INSTANCE_ID: &str = convention("service.instance.id", otel::SERVICE_INSTANCE_ID);
/// Where the service runs: `development`, `test` or `production`.
pub const DEPLOYMENT_ENVIRONMENT_NAME: &str = convention(
    "deployment.environment.name",
    otel::DEPLOYMENT_ENVIRONMENT_NAME,
);
/// The request's method.
pub const HTTP_REQUEST_METHOD: &str = convention("http.request.method", otel::HTTP_REQUEST_METHOD);
/// The template of the route the request matched, such as `/v1/capabilities`.
pub const HTTP_ROUTE: &str = convention("http.route", otel::HTTP_ROUTE);
/// The response's status.
pub const HTTP_RESPONSE_STATUS_CODE: &str =
    convention("http.response.status_code", otel::HTTP_RESPONSE_STATUS_CODE);
/// The kind of a failure: an error code, such as `request.internal_error`.
pub const ERROR_TYPE: &str = convention("error.type", otel::ERROR_TYPE);
/// A failure's cause chain, or a panic's message.
pub const EXCEPTION_MESSAGE: &str = convention("exception.message", otel::EXCEPTION_MESSAGE);
/// The source file a panic happened in.
pub const CODE_FILE_PATH: &str = convention("code.file.path", otel::CODE_FILE_PATH);
/// The line a panic happened on.
pub const CODE_LINE_NUMBER: &str = convention("code.line.number", otel::CODE_LINE_NUMBER);
/// The address the server listens on.
pub const SERVER_ADDRESS: &str = convention("server.address", otel::SERVER_ADDRESS);
/// The port the server listens on.
pub const SERVER_PORT: &str = convention("server.port", otel::SERVER_PORT);
/// The name of the database a task ran against.
pub const DB_NAMESPACE: &str = convention("db.namespace", otel::DB_NAMESPACE);

/// The trace a line was written in, a top-level field of JSON logs.
pub const TRACE_ID: &str = "trace_id";
/// The span a line was written in, a top-level field of JSON logs.
pub const SPAN_ID: &str = "span_id";

/// The span's name, which `tracing-opentelemetry` reads to name it. No JSON log line carries it.
pub const OTEL_NAME: &str = "otel.name";
/// The span's kind, which `tracing-opentelemetry` reads to set it. No JSON log line carries it.
pub const OTEL_KIND: &str = "otel.kind";
/// The span's status, which `tracing-opentelemetry` reads to set it. No JSON log line carries
/// it.
pub const OTEL_STATUS_CODE: &str = convention("otel.status_code", otel::OTEL_STATUS_CODE);

/// The caller a `Principal` proved, on the request's span (OBS-09). A user ID is pseudonymous, so
/// telemetry may carry it (OBS-04).
pub const USER_ID: &str = convention("user.id", otel::USER_ID);

/// How long a request took, in seconds.
pub const REQUEST_DURATION: &str = "request.duration";
/// The kind of a rate limit's bucket, such as `ip` or `sign_in` (PLATFORM-050).
pub const RATE_LIMIT_BUCKET: &str = "rate_limit.bucket";
/// How a sign-in ended, such as `succeeded` or `invalid_credentials` (AUTH-004, AUTH-005).
pub const AUTH_OUTCOME: &str = "auth.outcome";
/// The part of startup that failed, such as `database` or `listen` (OPS-03).
pub const STARTUP_AREA: &str = "startup.area";
/// The migration versions `migrate` applied.
pub const MIGRATION_VERSIONS: &str = "migration.versions";
/// The most connections to the database that an instance opens (OPS-21).
pub const DATABASE_CONNECTIONS: &str = "database.connections";
