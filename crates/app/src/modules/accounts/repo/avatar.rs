//! The SQL of a change of a user's avatar, whose row `files` keeps: the user's own part of it,
//! the move of their record's version (the accounts spec's "Avatars").

use platform::{
    db::UnitOfWork,
    errors::Internal,
    types::{UserId, Version},
};

use super::values;

/// Moves the version of the user `id`, for a change of their avatar, when it is one of
/// `versions`, or whatever it is when there are none, and returns whether it did (API-13,
/// DATA-18).
pub(in crate::modules::accounts) async fn touch(
    uow: &mut UnitOfWork,
    id: UserId,
    versions: Option<&[Version]>,
) -> Result<bool, Internal> {
    // The row's trigger moves its version and its update time (DATA-02).
    let versions = values(versions);
    let touched = sqlx::query!(
        "UPDATE users SET version = version
        WHERE id = $1 AND ($2::bigint[] IS NULL OR version = ANY($2))",
        id.uuid(),
        versions.as_deref(),
    )
    .execute(uow.executor())
    .await?;
    Ok(touched.rows_affected() > 0)
}
