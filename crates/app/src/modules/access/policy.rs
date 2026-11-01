//! The `AccessPolicy` that `platform::authz` asks on every workspace request (ACCESS-003, the
//! access spec's "Policy").

use platform::{
    authn::BoxFuture,
    authz::{AccessPolicy, Decision},
    db::UnitOfWork,
    errors::Internal,
    types::{UserId, WorkspaceId},
};

use super::{permissions, repo};

/// Decides by the caller's membership and the matrix.
pub(crate) struct Policy;

impl AccessPolicy for Policy {
    fn authorize<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        user: UserId,
        workspace: WorkspaceId,
        permission: &'static str,
    ) -> BoxFuture<'a, Result<Decision, Internal>> {
        Box::pin(async move {
            Ok(match repo::role_held(uow, workspace, user).await? {
                None => Decision::NotMember,
                Some(role) if permissions::grants(role, permission) => {
                    Decision::Allowed(role.name())
                }
                Some(_) => Decision::Denied,
            })
        })
    }
}
