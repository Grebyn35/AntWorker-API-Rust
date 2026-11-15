//! Whether an account has an authenticator app, which a superadmin's routes ask of their caller
//! (SUPERADMINS-005).

use platform::{db::UnitOfWork, types::UserId};

use crate::modules::auth::{error::AuthError, repo};

/// Whether the account of `user_id` has an authenticator app, read in `uow`, the unit of work
/// of the request that a superadmin's route serves (D63).
pub(crate) async fn has_authenticator_app(
    uow: &mut UnitOfWork,
    user_id: UserId,
) -> Result<bool, AuthError> {
    Ok(repo::second_factors::has_app(uow, user_id).await?)
}
