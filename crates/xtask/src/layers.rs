//! `check-layers`: each layer of a feature module keeps to its job, and the API's code builds
//! HTTP clients only where SEC-09 allows.
//! - A module's routes name no `sqlx` (ARCH-08).
//! - A module's service names no HTTP types: none of `axum`, `http` and the other crates of
//!   HTTP, none of `platform::http` and its `Problem` response, and none of its routes' request
//!   and response types (ARCH-09).
//! - Every type in a module's routes that derives `Deserialize` carries
//!   `#[serde(deny_unknown_fields)]`, the query types among them (API-08, API-09).
//! - No signature in a module's service or routes names an error of a library that reaches
//!   another system, such as `sqlx::Error` (ERR-04).
//! - The code of `platform` and `app` names an HTTP client library, or a TCP stream through
//!   which it could write HTTP itself, only in `platform::outbound`, the object storage and
//!   telemetry adapters, and the health subcommand's probe (SEC-09).
//!
//! It reads each file as a token stream, with the paths its `use` declarations import (`rust`).

use std::collections::BTreeSet;

use crate::{
    Finding, Outcome,
    repo::Repo,
    rust::{self, Named, Source},
};

/// The crates of HTTP, whose types a service never names (ARCH-09).
const HTTP_CRATES: [&str; 9] = [
    "axum",
    "axum_extra",
    "http",
    "http_body",
    "hyper",
    "hyper_util",
    "tower",
    "tower_http",
    "utoipa_axum",
];

/// The libraries that reach other systems, whose errors stay in their adapter (ERR-04): the
/// database driver, and the clients of HTTP, email and object storage.
const INFRASTRUCTURE: [&str; 12] = [
    "sqlx",
    "sqlx_core",
    "sqlx_postgres",
    "tokio_postgres",
    "reqwest",
    "hyper",
    "hyper_util",
    "lettre",
    "object_store",
    "aws_sdk_s3",
    "aws_smithy_runtime_api",
    "opentelemetry_otlp",
];

/// The libraries whose only job is to make HTTP requests (SEC-09).
const HTTP_CLIENTS: [&str; 7] = [
    "reqwest",
    "ureq",
    "isahc",
    "surf",
    "attohttpc",
    "curl",
    "awc",
];

/// The code the API runs, which SEC-09 covers.
const CODE: [&str; 2] = ["crates/platform/src/", "crates/app/src/"];

/// Where SEC-09 allows an HTTP client: the two clients of `platform::outbound`, and the object
/// storage and telemetry adapters, which use their libraries' own clients.
const CLIENTS_ALLOWED: [&str; 5] = [
    "crates/platform/src/outbound.rs",
    "crates/platform/src/outbound/",
    "crates/platform/src/storage.rs",
    "crates/platform/src/storage/",
    "crates/platform/src/telemetry/",
];

/// The health subcommand's probe, which writes its request to the instance's own address over
/// TCP itself (SEC-09).
const PROBE: &str = "crates/platform/src/lifecycle.rs";

/// Checks every Rust file of `platform` and `app`.
pub fn check(repo: &Repo) -> Outcome {
    let mut findings = Vec::new();
    for file in repo.files() {
        if !file.path.ends_with(".rs") || !CODE.iter().any(|code| file.path.starts_with(code)) {
            continue;
        }
        let source = match Source::read(file) {
            Ok(source) => source,
            Err(finding) => {
                findings.push(finding);
                continue;
            }
        };
        findings.extend(clients(&source));
        if let Some((module, parts)) = rust::feature_module(&file.path) {
            match parts.first() {
                Some(&"routes") => {
                    findings.extend(routes(&source));
                    findings.extend(infrastructure_errors(&source));
                }
                Some(&"service") => {
                    findings.extend(service(&source, module));
                    findings.extend(infrastructure_errors(&source));
                }
                _ => {}
            }
        }
    }
    dedup(&mut findings);
    Outcome {
        report: Vec::new(),
        findings,
    }
}

/// ARCH-08, API-08 and API-09: a module's routes name no `sqlx`, and each type they deserialize
/// refuses unknown fields.
fn routes(source: &Source) -> Vec<Finding> {
    let mut findings: Vec<Finding> = source
        .named()
        .into_iter()
        .filter(|named| named.segments.first().is_some_and(|root| root == "sqlx"))
        .map(|named| {
            Finding::new(
                source.path,
                named.line,
                format!(
                    "a module's routes use `{}`: SQL belongs in its repository (ARCH-08)",
                    named.segments.join("::")
                ),
            )
        })
        .collect();
    for (name, line, attributes) in rust::types_with_attributes(&source.tokens) {
        let derives = attributes.iter().any(|attribute| {
            rust::holds(attribute, "derive") && rust::holds(attribute, "Deserialize")
        });
        let strict = attributes.iter().any(|attribute| {
            rust::holds(attribute, "serde") && rust::holds(attribute, "deny_unknown_fields")
        });
        if derives && !strict {
            findings.push(Finding::new(
                source.path,
                line,
                format!(
                    "`{name}` derives `Deserialize` without `#[serde(deny_unknown_fields)]` (API-08, API-09)"
                ),
            ));
        }
    }
    findings
}

/// ARCH-09: a module's service names no HTTP types.
fn service(source: &Source, module: &str) -> Vec<Finding> {
    let own_routes = ["crate", "modules", module, "routes"];
    let mut findings = Vec::new();
    let names = source.resolved(rust::names(&source.tokens));
    for Named { segments, line } in names {
        let path: Vec<&str> = segments.iter().map(String::as_str).collect();
        let http = match path.as_slice() {
            [root, ..] if HTTP_CRATES.contains(root) => true,
            ["platform", "http", ..] | ["platform", "errors", "Problem", ..] => true,
            _ => path.starts_with(&own_routes),
        };
        if http && path.len() > 1 {
            findings.push(Finding::new(
                source.path,
                line,
                format!(
                    "a module's service uses `{}`, an HTTP type: services take domain types (ARCH-09)",
                    path.join("::")
                ),
            ));
        }
    }
    findings
}

/// ERR-04: no signature in a module's service or routes names an error of a library that reaches
/// another system.
fn infrastructure_errors(source: &Source) -> Vec<Finding> {
    let mut findings = Vec::new();
    for signature in rust::signatures(&source.tokens) {
        for Named { segments, .. } in source.resolved(rust::names(&signature.tokens)) {
            let (Some(root), Some(last)) = (segments.first(), segments.last()) else {
                continue;
            };
            if segments.len() > 1
                && INFRASTRUCTURE.contains(&root.as_str())
                && (last.ends_with("Error") || last == "Result")
            {
                findings.push(Finding::new(
                    source.path,
                    signature.line,
                    format!(
                        "a signature names `{}`, an error of a library that reaches another system: its adapter turns it into a domain outcome or `Internal` (ERR-04)",
                        segments.join("::")
                    ),
                ));
            }
        }
    }
    findings
}

/// SEC-09: an HTTP client library, or a TCP stream, named outside the places that may build an
/// HTTP client.
fn clients(source: &Source) -> Vec<Finding> {
    if CLIENTS_ALLOWED
        .iter()
        .any(|place| source.path.starts_with(place))
    {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for Named { segments, line } in source.named() {
        let path: Vec<&str> = segments.iter().map(String::as_str).collect();
        let client = match path.as_slice() {
            [root, ..] if HTTP_CLIENTS.contains(root) => true,
            ["hyper" | "hyper_util", "client", ..] => true,
            ["std" | "tokio" | "async_std", "net", "TcpStream", ..] => source.path != PROBE,
            _ => false,
        };
        if client {
            findings.push(Finding::new(
                source.path,
                line,
                format!(
                    "`{}` can make HTTP requests outside `platform::outbound`, the object storage and telemetry adapters and the health probe (SEC-09)",
                    path.join("::")
                ),
            ));
        }
    }
    findings
}

/// Removes the findings that repeat one already reported.
fn dedup(findings: &mut Vec<Finding>) {
    let mut seen = BTreeSet::new();
    findings.retain(|finding| {
        seen.insert((finding.path.clone(), finding.line, finding.message.clone()))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn findings(path: &str, code: &str) -> Vec<String> {
        let repo = Repo::of(&[(path, code)]);
        check(&repo)
            .findings
            .into_iter()
            .map(|finding| finding.message)
            .collect()
    }

    const ROUTES: &str = "crates/app/src/modules/workspaces/routes.rs";
    const SERVICE: &str = "crates/app/src/modules/workspaces/service.rs";

    #[test]
    fn arch_08_a_module_s_routes_name_no_sqlx() {
        let code = r#"
            use sqlx::Row;
            async fn create() { sqlx::query!("SELECT 1"); }
        "#;
        assert_eq!(
            findings(ROUTES, code),
            [
                "a module's routes use `sqlx::Row`: SQL belongs in its repository (ARCH-08)",
                "a module's routes use `sqlx::query`: SQL belongs in its repository (ARCH-08)",
            ]
        );
        assert_eq!(findings(SERVICE, code), Vec::<String>::new());
    }

    #[test]
    fn arch_09_a_service_names_no_http_types() {
        let code = r#"
            use axum::http::StatusCode;
            use platform::{db::UnitOfWork, errors::{Internal, Problem}, http::Json};
            use super::routes::CreateRequest;
            pub(crate) async fn create(uow: &mut UnitOfWork) -> Result<(), Internal> { Ok(()) }
        "#;
        assert_eq!(
            findings(SERVICE, code),
            [
                "a module's service uses `axum::http::StatusCode`, an HTTP type: services take domain types (ARCH-09)",
                "a module's service uses `platform::errors::Problem`, an HTTP type: services take domain types (ARCH-09)",
                "a module's service uses `platform::http::Json`, an HTTP type: services take domain types (ARCH-09)",
                "a module's service uses `crate::modules::workspaces::routes::CreateRequest`, an HTTP type: services take domain types (ARCH-09)",
            ]
        );
    }

    #[test]
    fn api_08_a_routes_type_that_derives_deserialize_refuses_unknown_fields() {
        let code = r#"
            #[derive(Deserialize, ToSchema)]
            #[serde(deny_unknown_fields)]
            pub(crate) struct Strict { name: String }
            /// A body.
            #[derive(Debug, serde::Deserialize)]
            pub(crate) struct Loose { name: String }
            #[derive(Serialize)]
            struct Answer { name: String }
        "#;
        assert_eq!(
            findings(ROUTES, code),
            [
                "`Loose` derives `Deserialize` without `#[serde(deny_unknown_fields)]` (API-08, API-09)"
            ]
        );
    }

    #[test]
    fn api_09_a_query_type_refuses_unknown_parameters() {
        let code = "#[derive(Deserialize)] #[serde(default)] enum Filter { All }";
        assert_eq!(
            findings(ROUTES, code),
            [
                "`Filter` derives `Deserialize` without `#[serde(deny_unknown_fields)]` (API-08, API-09)"
            ]
        );
    }

    #[test]
    fn err_04_no_service_or_route_signature_names_an_infrastructure_error() {
        let code = r#"
            use sqlx::Error as DatabaseError;
            pub(crate) async fn a() -> Result<(), sqlx::Error> {}
            async fn b(error: DatabaseError) {}
            fn c() -> Result<(), Internal> { let _ = sqlx::Error::RowNotFound; }
        "#;
        let message = "a signature names `sqlx::Error`, an error of a library that reaches another system: its adapter turns it into a domain outcome or `Internal` (ERR-04)";
        assert_eq!(findings(SERVICE, code), [message, message]);
        let lines: Vec<usize> = check(&Repo::of(&[(SERVICE, code)]))
            .findings
            .iter()
            .map(|finding| finding.line)
            .collect();
        assert_eq!(lines, [3, 4], "both signatures, and not the body of c");
    }

    #[test]
    fn sec_09_http_clients_are_built_only_where_sec_09_allows() {
        let code = r#"
            use std::net::TcpStream;
            fn f() { let client = reqwest::Client::builder(); }
        "#;
        assert_eq!(
            findings("crates/app/src/modules/auth/providers/google.rs", code),
            [
                "`std::net::TcpStream` can make HTTP requests outside `platform::outbound`, the object storage and telemetry adapters and the health probe (SEC-09)",
                "`reqwest::Client::builder` can make HTTP requests outside `platform::outbound`, the object storage and telemetry adapters and the health probe (SEC-09)",
            ]
        );
        assert_eq!(
            findings("crates/platform/src/telemetry/export.rs", code),
            Vec::<String>::new()
        );
        assert_eq!(
            findings("crates/platform/src/outbound/untrusted.rs", code),
            Vec::<String>::new()
        );
        assert_eq!(
            findings(PROBE, code),
            [
                "`reqwest::Client::builder` can make HTTP requests outside `platform::outbound`, the object storage and telemetry adapters and the health probe (SEC-09)"
            ]
        );
        assert_eq!(
            findings("crates/xtask/src/load.rs", code),
            Vec::<String>::new(),
            "the checker isn't the API's code"
        );
    }
}
