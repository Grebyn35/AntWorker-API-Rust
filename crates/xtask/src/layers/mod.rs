//! `check-layers`: each layer of a feature module keeps to its job, and the API's code builds
//! HTTP clients only where SEC-10 allows.
//! - A module's routes name no `sqlx`, and a handler calls `check` on no `RateLimits` it
//!   extracts: it hands them to the service, which counts the per-action limit; and it makes at
//!   most one of the calls that the check counts as a service function's, which ARCH-08's Check
//!   lists. A function of the routes that no `routes!` registers makes none, so a helper can't
//!   make a handler's second call, and the routes call no `begin`: the service opens a unit of
//!   work, or `Authorize` on a workspace route (ARCH-08).
//! - A module's service names no HTTP types: none of `axum`, `http` and the other crates of
//!   HTTP, none of `platform::http` and its `Problem` response, and none of its routes' request
//!   and response types (ARCH-09).
//! - Every type in a module's routes that derives `Deserialize` carries
//!   `#[serde(deny_unknown_fields)]`, the query types among them (API-08, API-09).
//! - No signature in a module's service or routes names an error of a library that reaches
//!   another system, such as `sqlx::Error` (ERR-04).
//! - The code of `platform` and `app` names an HTTP client library, or a TCP stream through
//!   which it could write HTTP itself, only in `platform::outbound`, the telemetry adapters and
//!   the health subcommand's probe, and the other places that [`CLIENTS_ALLOWED`] and
//!   [`TCP_ALLOWED`] list (SEC-10).
//!
//! It reads each file as a token stream (`rust`), and ARCH-08's calls as [`limits_counted`] says.

use std::collections::BTreeSet;

use crate::{
    Finding, Outcome,
    repo::Repo,
    rust::{self, Named, Source},
};

mod handlers;

use handlers::{handlers, limits_counted};

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
const INFRASTRUCTURE: [&str; 13] = [
    "sqlx",
    "sqlx_core",
    "sqlx_postgres",
    "tokio_postgres",
    "reqwest",
    "hyper",
    "hyper_util",
    "lettre",
    "rusty_s3",
    "object_store",
    "aws_sdk_s3",
    "aws_smithy_runtime_api",
    "opentelemetry_otlp",
];

/// The libraries whose only job is to make HTTP requests (SEC-10).
const HTTP_CLIENTS: [&str; 7] = [
    "reqwest",
    "ureq",
    "isahc",
    "surf",
    "attohttpc",
    "curl",
    "awc",
];

/// The code the API runs, which SEC-10 covers.
const CODE: [&str; 2] = ["crates/platform/src/", "crates/app/src/"];

/// Where SEC-10 allows an HTTP client: the two clients of `platform::outbound`, and the adapters
/// that use their libraries' own clients, such as the telemetry adapters.
const CLIENTS_ALLOWED: &[&str] = &[
    "crates/platform/src/outbound.rs",
    "crates/platform/src/outbound/",
    // part: files
    "crates/platform/src/storage.rs",
    "crates/platform/src/storage/",
    // end part: files
    "crates/platform/src/telemetry/",
];

/// Where SEC-10 allows a TCP stream beside the places of `CLIENTS_ALLOWED`, such as the health
/// subcommand's probe, which writes its request to the instance's own address itself.
const TCP_ALLOWED: &[&str] = &[
    "crates/platform/src/lifecycle.rs",
    // part: files
    "crates/platform/src/scanner.rs",
    // end part: files
];

/// Checks every Rust file of `platform` and `app`.
pub fn check(repo: &Repo) -> Outcome {
    let mut findings = Vec::new();
    let handlers = handlers(repo);
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
                    let registered = handlers.get(module).cloned().unwrap_or_default();
                    findings.extend(limits_counted(file, &registered));
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

/// SEC-10: an HTTP client library, or a TCP stream, named outside the places that may build an
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
            ["std" | "tokio" | "async_std", "net", "TcpStream", ..] => {
                !TCP_ALLOWED.contains(&source.path)
            }
            _ => false,
        };
        if client {
            findings.push(Finding::new(
                source.path,
                line,
                format!(
                    "`{}` can make HTTP requests outside the places that SEC-10 allows, such as `platform::outbound` (SEC-10)",
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
    fn arch_08_a_handler_hands_its_rate_limits_to_the_service() {
        let code = r#"
            async fn share(limits: RateLimits, errors: FieldErrors) -> Result<(), E> {
                errors.check("/email", parse());
                limits.check(bucket, b"key").await?;
                service::share(&limits).await
            }
        "#;
        assert_eq!(
            findings(ROUTES, code),
            [
                "a handler counts a per-action limit: it hands its `RateLimits` to the service, \
              which counts it (ARCH-08)"
            ]
        );
        assert_eq!(findings(SERVICE, code), Vec::<String>::new());
    }

    #[test]
    fn arch_08_a_helper_that_no_route_registers_calls_no_service_function() {
        let code = r#"
            fn routes() -> Routes<Files> { Routes::new().user(routes!(download), &[]) }
            async fn download(State(files): State<Files>) -> R {
                let file = service::download(&files).await?;
                bytes(&files, file).await
            }
            async fn bytes(files: &Files, file: Serving) -> R { service::bytes(files, &file).await }
            fn shape(file: Serving) -> R { R::from(file) }
        "#;
        assert_eq!(
            findings(ROUTES, code),
            [
                "`bytes`, which no route registers, calls the service: a handler calls its one \
                 service function itself (ARCH-08)"
            ]
        );
    }

    #[test]
    fn arch_08_a_module_s_routes_open_no_unit_of_work() {
        let code = r#"
            async fn list(State(files): State<Files>, client: Client) -> R {
                let uow = files.database.begin(&client).await?;
                service::list(uow).await
            }
        "#;
        assert_eq!(
            findings(ROUTES, code),
            [
                "a module's routes open a unit of work: the service opens it, or `Authorize` on a \
                 workspace route (ARCH-08)"
            ]
        );
    }

    #[test]
    fn arch_08_a_module_s_routes_open_no_unit_of_work_through_a_path_either() {
        let code = r#"
            async fn list(State(files): State<Files>, client: Client) -> R {
                let uow = Database::begin(&files.database, &client).await?;
                service::list(uow).await
            }
            async fn read(State(files): State<Files>, client: Client) -> R {
                service::read(<Database>::begin(&files.database, &client).await?).await
            }
        "#;
        let opens = "a module's routes open a unit of work: the service opens it, or `Authorize` \
                     on a workspace route (ARCH-08)";
        assert_eq!(findings(ROUTES, code), [opens, opens]);
    }

    #[test]
    fn arch_08_a_handler_calls_one_service_function() {
        let code = r#"
            async fn one(State(auth): State<Arc<Auth>>, Json(request): Json<R>) -> R {
                let input = Input::try_from(request)?;
                created(auth.sign_in(input).await?)
            }
            async fn two(State(auth): State<Arc<Auth>>) -> R {
                let name = auth.cookie(&email);
                auth.sign_in(name).await
            }
            async fn both(State(database): State<Database>) -> R {
                service::me(&database).await?;
                service::list(&database).await
            }
            fn helper(auth: &Auth) { auth.a(); auth.b(); }
        "#;
        assert_eq!(
            findings(ROUTES, code),
            [
                "a handler calls a second service function: it calls one (ARCH-08)",
                "a handler calls a second service function: it calls one (ARCH-08)",
            ]
        );
    }

    #[test]
    fn arch_08_a_handler_calls_one_service_function_through_a_state_it_takes_apart() {
        let code = r#"
            async fn one(State(Routes { auth, .. }): State<Routes>) -> R {
                auth.sign_in().await
            }
            async fn named(State(Routes { auth, providers }): State<Routes>) -> R {
                providers.find("google");
                auth.sign_in().await
            }
            async fn renamed(State(Routes { auth: signing, .. }): State<Routes>) -> R {
                signing.start().await;
                signing.finish().await
            }
        "#;
        assert_eq!(
            findings(ROUTES, code),
            [
                "a handler calls a second service function: it calls one (ARCH-08)",
                "a handler calls a second service function: it calls one (ARCH-08)",
            ]
        );
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
    fn sec_10_http_clients_and_tcp_streams_are_built_only_where_sec_10_allows() {
        let code = r#"
            use std::net::TcpStream;
            fn f() { let client = reqwest::Client::builder(); }
        "#;
        assert_eq!(
            findings("crates/app/src/modules/auth/providers/example.rs", code),
            [
                "`std::net::TcpStream` can make HTTP requests outside the places that SEC-10 allows, such as `platform::outbound` (SEC-10)",
                "`reqwest::Client::builder` can make HTTP requests outside the places that SEC-10 allows, such as `platform::outbound` (SEC-10)",
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
        for place in TCP_ALLOWED {
            assert_eq!(
                findings(place, code),
                [
                    "`reqwest::Client::builder` can make HTTP requests outside the places that SEC-10 allows, such as `platform::outbound` (SEC-10)"
                ],
                "{place}"
            );
        }
        assert_eq!(
            findings("crates/xtask/src/load.rs", code),
            Vec::<String>::new(),
            "the checker isn't the API's code"
        );
    }
}
