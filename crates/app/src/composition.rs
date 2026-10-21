//! The composition root: builds every service once and mounts every route (ARCH-12).

use axum::Router;
use sqlx::migrate::Migrator;

/// Every migration in `migrations/`, embedded in the binary. `migrate` applies them, and
/// `serve` refuses to start until they are all applied (DATA-09).
pub static MIGRATIONS: Migrator = sqlx::migrate!("../../migrations");

/// The application's router.
pub fn router() -> Router {
    Router::new().merge(platform::lifecycle::probe_routes())
}
