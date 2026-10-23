//! The composition root: builds every service once and mounts every module's routes (ARCH-12).

use platform::http::Routes;
use sqlx::migrate::Migrator;

use crate::modules::system;

/// Every migration in `migrations/`, embedded in the binary. `migrate` applies them, and
/// `serve` refuses to start until they are all applied (DATA-09).
pub static MIGRATIONS: Migrator = sqlx::migrate!("../../migrations");

/// Every module's routes.
pub fn routes() -> Routes {
    system::routes()
}
