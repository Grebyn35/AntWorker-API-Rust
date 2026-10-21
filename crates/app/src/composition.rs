//! The composition root: builds every service once and mounts every route (ARCH-12).

use axum::Router;

/// The application's router.
pub fn router() -> Router {
    Router::new().merge(platform::lifecycle::probe_routes())
}
