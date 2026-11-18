//! The API reference that development serves (PLATFORM-052): a page that shows the OpenAPI
//! document of the running code through Swagger UI, and the document itself. Both are on the
//! API's own origin, so the page's requests carry the session's cookies and need no CORS.

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use axum::{
    Router,
    http::{HeaderValue, Method, header},
    response::IntoResponse,
    routing::{MethodRouter, get},
};
use base64::Engine;
use sha2::{Digest, Sha256};

use super::{docs::QueryNames, stack};

/// The page's path.
pub(super) const PAGE: &str = "/docs";

/// The document's path, from which the page loads it.
pub(super) const DOCUMENT: &str = "/docs/openapi.json";

/// The page, up to its own script. Swagger UI comes from the jsDelivr content delivery network,
/// pinned by its version and by the Subresource Integrity hash of each file, which are those of
/// the package that npm publishes, so the browser refuses any other bytes.
const HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>API reference</title>
<link rel="stylesheet" href="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.34.0/swagger-ui.css" integrity="sha384-4c3vsE5f+trhFyHrFrLM1fFGUn531NIaC7dcqEWIJdoRQJ5CkJbSuSCwXLpaS1KF" crossorigin="anonymous">
</head>
<body>
<div id="reference"></div>
<script src="https://cdn.jsdelivr.net/npm/swagger-ui-dist@5.34.0/swagger-ui-bundle.js" integrity="sha384-9Td7MKiIr2S83E1kZyyVi8KdJw/S8CpAvu0llLyMeg0xq/hDGiZJfVn0fV4llCmK" crossorigin="anonymous"></script>
"#;

/// The page's own script, which starts Swagger UI on the document. `validatorUrl: null` keeps
/// the page from sending the document to Swagger's online validator.
const START: &str = r##"
SwaggerUIBundle({ url: "/docs/openapi.json", dom_id: "#reference", validatorUrl: null, deepLinking: true });
"##;

/// The page's `Content-Security-Policy`, in place of the one that every other response carries
/// (SEC-12), which would let the page load nothing: it lets the page load Swagger UI's script
/// and style sheet from jsDelivr, run its own script, which the policy names by its hash, set
/// the styles and show the images that Swagger UI writes into the page, and send its requests
/// to the API's own origin. As every response's policy, it sets no base address, and lets no
/// page frame it.
fn policy() -> String {
    let start = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(START));
    format!(
        "default-src 'none'; script-src https://cdn.jsdelivr.net 'sha256-{start}'; \
         style-src https://cdn.jsdelivr.net 'unsafe-inline'; img-src 'self' data:; \
         connect-src 'self'; base-uri 'none'; frame-ancestors 'none'"
    )
}

/// `router` with the page and `document`, the OpenAPI document as JSON. Each route is in a route
/// group, as a builder's is, so its group's timeout and body limit apply, and it refuses any query
/// parameter (OPS-07, API-08, API-09).
#[expect(
    clippy::disallowed_methods,
    reason = "the API reference is outside the documented routes, which the builders register \
              (API-14, PLATFORM-052)"
)]
pub(super) fn mount(router: Router, document: Arc<str>) -> Router {
    let page: Arc<str> = format!("{HTML}<script>{START}</script>\n</body>\n</html>\n").into();
    // The policy holds visible ASCII alone, which a header's value takes.
    let policy = HeaderValue::from_str(&policy()).ok();
    router
        .route(
            PAGE,
            grouped(get(move || {
                let (page, policy) = (page.clone(), policy.clone());
                async move {
                    let mut response = (
                        [(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("text/html; charset=utf-8"),
                        )],
                        page.to_string(),
                    )
                        .into_response();
                    if let Some(policy) = policy {
                        response
                            .headers_mut()
                            .insert(header::CONTENT_SECURITY_POLICY, policy);
                    }
                    response
                }
            })),
        )
        .route(
            DOCUMENT,
            grouped(get(move || {
                let document = document.clone();
                async move {
                    (
                        [(
                            header::CONTENT_TYPE,
                            HeaderValue::from_static("application/json"),
                        )],
                        document.to_string(),
                    )
                        .into_response()
                }
            })),
        )
}

/// `route` in a route group that takes no query parameter.
fn grouped(route: MethodRouter) -> MethodRouter {
    let none: QueryNames = HashMap::from([
        (Method::GET, HashSet::new()),
        (Method::HEAD, HashSet::new()),
    ]);
    stack::route_group(route, none)
}
