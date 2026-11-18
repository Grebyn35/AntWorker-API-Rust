//! The operator's end of the sessions of one account, or of every account, which the binary's
//! subcommand `end-sessions` runs (AUTH-094).

use platform::{
    authn::AuditScope,
    db::{Database, UnitOfWork},
    errors::Internal,
    types::{EmailAddress, UserId},
};

use super::sessions::end_sessions;
use crate::modules::{
    accounts,
    auth::{error::AuthError, repo},
};

/// The most users whose IDs one read of [`end_every_session`] takes.
const BATCH: i64 = 1_000;

/// What the operator's end of sessions found (AUTH-094).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ending {
    /// It ended `sessions` sessions, of `users` users.
    Ended {
        /// How many sessions it ended.
        sessions: usize,
        /// How many users held them.
        users: usize,
    },
    /// No account holds the address.
    NoAccount,
    /// The account that holds the address, or, for every account's, each account, has no session.
    NoSession,
}

/// Ends every session of the account that holds `email`, each with its access-change signal and
/// its `session.ended` event, and recorded as `session.ended` in the account's log, with no
/// actor, in one unit of work on `database` (AUTH-094, AUTH-046). An address that no account
/// holds, or an account without a session, changes nothing. It locks the user's row, then the
/// sessions, the order of every operation that ends sessions, so it takes turns with the user's
/// own requests (DATA-20).
pub(crate) async fn end_sessions_of(
    database: &Database,
    email: &EmailAddress,
) -> Result<Ending, Internal> {
    internal(async {
        #[expect(
            clippy::disallowed_methods,
            reason = "the operator's subcommand acts for no request and no job (ARCH-14)"
        )]
        let mut uow = database.for_operator().await?;
        let Some((user_id, _)) = accounts::holder_for_update(&mut uow, email).await? else {
            return Ok(Ending::NoAccount);
        };
        let sessions = end_all(&mut uow, user_id).await?;
        if sessions == 0 {
            return Ok(Ending::NoSession);
        }
        uow.commit().await?;
        Ok(Ending::Ended { sessions, users: 1 })
    })
    .await
}

/// Ends every session of every user, as [`end_sessions_of`] ends one account's: it reads the
/// users that hold a session, by their IDs, [`BATCH`] at a time, and ends each user's sessions in
/// a unit of work of their own on `database`, which locks that user's row first, so it holds one
/// user's rows at a time, in the order of every operation that ends sessions (AUTH-094,
/// DATA-20). A session that starts once its user's unit of work has committed stays. A user that
/// a deletion of the account took since the read, or whose sessions another request ended, is
/// passed over.
pub(crate) async fn end_every_session(database: &Database) -> Result<Ending, Internal> {
    internal(async {
        let (mut sessions, mut users) = (0, 0);
        let mut after = None;
        loop {
            let holders = {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the operator's subcommand acts for no request and no job (ARCH-14)"
                )]
                let mut uow = database.for_operator().await?;
                repo::holders(&mut uow, after, BATCH).await?
            };
            let Some(&last) = holders.last() else {
                break;
            };
            for user_id in holders {
                #[expect(
                    clippy::disallowed_methods,
                    reason = "the operator's subcommand acts for no request and no job (ARCH-14)"
                )]
                let mut uow = database.for_operator().await?;
                if accounts::address_for_update(&mut uow, user_id)
                    .await?
                    .is_none()
                {
                    continue;
                }
                let ended = end_all(&mut uow, user_id).await?;
                if ended == 0 {
                    continue;
                }
                uow.commit().await?;
                sessions += ended;
                users += 1;
            }
            after = Some(last);
        }
        if sessions == 0 {
            return Ok(Ending::NoSession);
        }
        Ok(Ending::Ended { sessions, users })
    })
    .await
}

/// Deletes every session of `user_id`, whose row `uow` holds, signals and publishes each, records
/// each as `session.ended` with no actor, and answers how many there were (AUTH-094).
async fn end_all(uow: &mut UnitOfWork, user_id: UserId) -> Result<usize, AuthError> {
    let ended = repo::delete_sessions(uow, user_id).await?;
    end_sessions(uow, AuditScope::attempt(user_id), user_id, &ended).await?;
    Ok(ended.len())
}

/// What `steps` answer, whose errors are `auth`'s, which are only ever internal here.
async fn internal(
    steps: impl Future<Output = Result<Ending, AuthError>>,
) -> Result<Ending, Internal> {
    steps.await.map_err(|error| match error {
        AuthError::Internal(error) => error,
        other => Internal::new(other),
    })
}
