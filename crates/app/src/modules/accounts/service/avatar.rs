//! Setting and removing the caller's avatar, whose image `files` keeps, the avatars' links in
//! the records that `accounts` answers, and the copy of the picture that a provider gives for a
//! new account as its avatar (the accounts spec's "Avatars", D120).

use std::collections::HashMap;

use bytes::Bytes;
use platform::{
    authn::Principal,
    blocking::Workers,
    db::{Database, UnitOfWork},
    errors::{self, Internal, RequestError},
    jobs::{JobContext, JobFailure},
    outbound::{FetchError, Untrusted},
    types::{Client, Version},
};

use super::{current, lock_caller, publish_changed, publish_updated};
use crate::modules::{
    access,
    accounts::{
        audit,
        error::AccountsError,
        jobs::CopyPicture,
        model::{Current, PublicProfile},
        repo,
    },
    audit::{self as log, Entry},
    files::{self, Files, NewAvatar},
};

/// The longest address of a picture that a copy takes (ACCOUNTS-028).
const PICTURE_MAX_CHARS: usize = 2_048;

/// The name of the failure that leaves an account without the picture that a provider gave for
/// it, as the logs and `platform.errors_ignored` carry it (ACCOUNTS-028, ERR-11).
const NOT_COPIED: &str = "picture_not_copied";

/// What the avatar's routes and the copy of a provider's picture take: the database, `files`,
/// which keeps the images, the turns of the instance's CPU-heavy work, which `Upload::read` takes
/// (PLATFORM-074), and the client that fetches a picture from the address a provider gives
/// (SEC-10).
#[derive(Clone)]
pub(crate) struct Avatars {
    pub(in crate::modules::accounts) database: Database,
    pub(in crate::modules::accounts) files: Files,
    pub(in crate::modules::accounts) workers: Workers,
    untrusted: Untrusted,
}

impl Avatars {
    /// The avatars' service, on `database`, keeping images in `files`, with `workers`, fetching
    /// a provider's picture through `untrusted`.
    pub(crate) fn new(
        database: Database,
        files: Files,
        workers: Workers,
        untrusted: Untrusted,
    ) -> Self {
        Self {
            database,
            files,
            workers,
            untrusted,
        }
    }

    /// Copies the picture at `url` as the avatar of the user that `context`'s job runs for,
    /// unless they have one by then, or their account is gone: fetched through the untrusted
    /// client, then checked, scanned and stored as an avatar's upload is, and recorded in a unit
    /// of work of the job's. A picture that the client refuses, that isn't an image an avatar may
    /// be, or that holds malware is ignored, and so is one that the `last` attempt fails to
    /// fetch, scan or store; an earlier attempt's failure is one that the job retries
    /// (ACCOUNTS-028).
    pub(in crate::modules::accounts) async fn copy(
        &self,
        context: &JobContext,
        url: &str,
        last: bool,
    ) -> Result<(), JobFailure> {
        let picture = match self.untrusted.get(url, files::AVATAR_MAX_BYTES).await {
            Ok(picture) => picture,
            Err(FetchError::Failed(cause)) if !last => {
                return Err(JobFailure::Transient(Internal::new(cause)));
            }
            Err(error) => {
                errors::ignore(&error, NOT_COPIED);
                return Ok(());
            }
        };
        let stored = files::upload_avatar(&self.files, picture, async |avatar: NewAvatar| {
            self.adopt(context, avatar).await
        })
        .await;
        match stored {
            Ok(()) => Ok(()),
            Err(AccountsError::AvatarUnavailable(cause)) if !last => Err(JobFailure::Transient(
                Internal::new(AccountsError::AvatarUnavailable(cause)),
            )),
            Err(
                error @ (AccountsError::AvatarNotImage
                | AccountsError::AvatarMalwareDetected
                | AccountsError::AvatarUnavailable(_)),
            ) => {
                errors::ignore(&error, NOT_COPIED);
                Ok(())
            }
            Err(error) => Err(JobFailure::Transient(Internal::new(error))),
        }
    }

    /// Makes `avatar`, a picture that a provider gave, the avatar of the user that `context`'s
    /// job runs for, in a unit of work of the job's, as [`set`] makes an upload a caller's:
    /// unless they have one, or their account is gone, which leaves the picture's object to the
    /// deletion of unreferenced objects (FILES-011).
    async fn adopt(&self, context: &JobContext, avatar: NewAvatar) -> Result<(), AccountsError> {
        let (Some(user), Some(scope)) = (context.scope().user(), context.audit_scope()) else {
            return Err(Internal::new("a picture's job runs for no one").into());
        };
        let mut uow = self.database.for_job(context).await?;
        // The user's row first, as every change of their avatar locks it (DATA-20).
        if !repo::lock(&mut uow, user).await?
            || !files::avatar_urls(&mut uow, &[user]).await?.is_empty()
        {
            return Ok(());
        }
        repo::touch(&mut uow, user, None).await?;
        let id = files::record_avatar(&mut uow, user, avatar).await?;
        let entry = Entry::new(audit::AVATAR_COPIED, audit::AVATAR, id);
        log::record(&mut uow, scope, entry).await?;
        let workspaces = access::workspace_ids_of(&mut uow, user).await?;
        publish_changed(&mut uow, user, workspaces).await?;
        Ok(uow.commit().await?)
    }
}

/// Enqueues in `uow` the job that copies the picture at `url`, which a provider gave for the new
/// account that `principal` proves, as the account's avatar. An address of more than 2,048
/// characters enqueues nothing (ACCOUNTS-028).
pub(crate) async fn copy_picture(
    uow: &mut UnitOfWork,
    principal: &Principal,
    url: &str,
) -> Result<(), AccountsError> {
    if url.chars().count() <= PICTURE_MAX_CHARS {
        let job = CopyPicture {
            url: url.to_owned(),
        };
        uow.enqueue(principal.job_scope(), &job).await?;
    }
    Ok(())
}

/// Makes the image whose `bytes` the request sent the caller's avatar, once `files` has checked,
/// scanned and written it, when the record's version is one of `versions`, or whatever it is when
/// there are none, then answers the record as it then is: the lock of the user's row, their new
/// version, the avatar's row, the audit entry in their own log and the events commit together, in
/// a unit of work of `client`'s request (ACCOUNTS-008, ACCOUNTS-009, PLATFORM-071).
pub(in crate::modules::accounts) async fn set(
    avatars: &Avatars,
    client: &Client,
    principal: &Principal,
    bytes: Bytes,
    versions: Option<&[Version]>,
) -> Result<Current, AccountsError> {
    files::upload_avatar(&avatars.files, bytes, async |avatar: NewAvatar| {
        let mut uow = avatars.database.begin(client).await?;
        let user = principal.user_id();
        lock_caller(&mut uow, principal).await?;
        if !repo::touch(&mut uow, user, versions).await? {
            return Err(RequestError::PreconditionFailed.into());
        }
        let id = files::record_avatar(&mut uow, user, avatar).await?;
        let entry = Entry::new(audit::AVATAR_UPLOADED, audit::AVATAR, id);
        log::record(&mut uow, principal.audit_scope(), entry).await?;
        publish_updated(&mut uow, principal).await?;
        let current = current(&mut uow, principal).await?;
        uow.commit().await?;
        Ok(current)
    })
    .await
}

/// Removes the caller's avatar, when they have one, and the record's version is one of
/// `versions`, or whatever it is when there are none, in a unit of work of `client`'s request: the
/// lock of the user's row, the avatar row's deletion, their new version, the audit entry in their
/// own log and the events commit together. Without one, nothing changes (ACCOUNTS-010,
/// PLATFORM-071).
pub(in crate::modules::accounts) async fn remove(
    avatars: &Avatars,
    client: &Client,
    principal: &Principal,
    versions: Option<&[Version]>,
) -> Result<(), AccountsError> {
    let mut uow = avatars.database.begin(client).await?;
    let user = principal.user_id();
    lock_caller(&mut uow, principal).await?;
    if !repo::touch(&mut uow, user, versions).await? {
        return Err(RequestError::PreconditionFailed.into());
    }
    // Without an avatar, the unit of work rolls back as it is dropped, the version's move with
    // it.
    let Some(id) = files::delete_avatar(&mut uow, user).await? else {
        return Ok(());
    };
    let entry = Entry::new(audit::AVATAR_DELETED, audit::AVATAR, id);
    log::record(&mut uow, principal.audit_scope(), entry).await?;
    publish_updated(&mut uow, principal).await?;
    Ok(uow.commit().await?)
}

/// `current`, with the link of the user's avatar, if they have one (ACCOUNTS-011).
pub(in crate::modules::accounts) async fn with_link(
    uow: &mut platform::db::UnitOfWork,
    mut current: Current,
) -> Result<Current, AccountsError> {
    current.profile.avatar_url = files::avatar_urls(uow, &[current.profile.id])
        .await?
        .into_iter()
        .next()
        .map(|(_, url)| url);
    Ok(current)
}

/// `profiles`, each with the link of its user's avatar, if they have one (ACCOUNTS-011).
pub(in crate::modules::accounts) async fn with_links(
    uow: &mut platform::db::UnitOfWork,
    mut profiles: Vec<PublicProfile>,
) -> Result<Vec<PublicProfile>, AccountsError> {
    let ids: Vec<_> = profiles.iter().map(|profile| profile.id).collect();
    let mut urls: HashMap<_, _> = files::avatar_urls(uow, &ids).await?.into_iter().collect();
    for profile in &mut profiles {
        profile.avatar_url = urls.remove(&profile.id);
    }
    Ok(profiles)
}
