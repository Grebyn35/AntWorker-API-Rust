//! Builders of superadmins, through the `owner` connection, for the tests of what a superadmin's
//! routes serve (TEST-09). The tests of the subcommand make theirs through the binary.

use super::{Api, second_factors, users::SignedIn};

/// Makes `user` a superadmin, as an operator's run of `superadmin add` does.
pub async fn make(api: &Api, user: &SignedIn) {
    api.database()
        .owner_executes(&format!(
            "INSERT INTO superadmins (user_id) VALUES ('{}')",
            user.user_id
        ))
        .await;
}

/// Makes `user` a superadmin whom a superadmin's routes serve: one whose account has an
/// authenticator app (SUPERADMINS-005).
pub async fn make_with_app(api: &Api, user: &SignedIn) {
    make(api, user).await;
    second_factors::add_app(api, user).await;
}
