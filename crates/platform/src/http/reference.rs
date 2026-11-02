//! The API reference that development serves (PLATFORM-075): a page that shows the OpenAPI
//! document of the running code through Swagger UI, and the document itself. Both are on the
//! API's own origin, so the page's requests carry the session's cookies and need no CORS.

use std::sync::Arc;

use axum::{
    Router,
    http::{HeaderValue, header},
    response::IntoResponse,
    routing::get,
};

/// The page's path.
pub(super) const PAGE: &str = "/docs";

/// The document's path, from which the page loads it.
pub(super) const DOCUMENT: &str = "/docs/openapi.json";

/// The page. Swagger UI comes from the jsDelivr content delivery network, pinned by its version
/// and by the Subresource Integrity hash of each file, which are those of the package that npm
/// publishes, so the browser refuses any other bytes. `validatorUrl: null` keeps the page from
/// sending the document to Swagger's online validator.
const HTML: &str = r##"<!doctype html>
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
<script>
SwaggerUIBundle({ url: "/docs/openapi.json", dom_id: "#reference", validatorUrl: null, deepLinking: true });
</script>
</body>
</html>
"##;

/// `router` with the page and `document`, the OpenAPI document as JSON.
#[expect(
    clippy::disallowed_methods,
    reason = "the API reference is outside the documented routes, which the builders register \
              (API-14, PLATFORM-075)"
)]
pub(super) fn mount(router: Router, document: Arc<str>) -> Router {
    router
        .route(
            PAGE,
            get(|| async {
                (
                    [(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("text/html; charset=utf-8"),
                    )],
                    HTML,
                )
                    .into_response()
            }),
        )
        .route(
            DOCUMENT,
            get(move || {
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
            }),
        )
}
