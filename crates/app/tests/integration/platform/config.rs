use std::ffi::OsString;

use crate::support::{TestDatabase, not_unicode};

#[tokio::test]
async fn ops_01_a_setting_that_isnt_unicode_fails_startup() {
    let db = TestDatabase::migrated().await;

    for setting in ["LOG_LEVEL", "ENVIRONMENT", "DATABASE_OWNER_URL_FILE"] {
        let mut env = vec![("ENVIRONMENT", OsString::from("test"))];
        env.retain(|(name, _)| *name != setting);
        env.push((setting, not_unicode()));

        let output = db.run_api(&["migrate"], &env).await;

        let errors = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success(), "{setting}: {errors}");
        assert!(
            errors.contains(&format!("{setting} must be valid Unicode")),
            "{setting}: {errors}"
        );
    }
}
