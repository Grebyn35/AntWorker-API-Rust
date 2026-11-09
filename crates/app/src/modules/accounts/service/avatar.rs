//! Setting and removing the caller's avatar, whose image `files` keeps, and the avatars' links in
//! the records that `accounts` answers (the accounts spec's "Avatars", D120).

use std::collections::HashMap;

use bytes::Bytes;
use platform::{
    authn::Principal,
    blocking::Workers,
    db::Database,
    errors::RequestError,
    types::{Client, Version},
};

use super::{current, publish_updated};
use crate::modules::{
    accounts::{
        audit,
        error::AccountsError,
        model::{Current, PublicProfile},
        repo,
    },
    audit::{self as log, Entry},
    files::{self, Files, NewAvatar},
};

/// What the avatar's routes take: the database, `files`, which keeps the images, and the turns
/// of the instance's CPU-heavy work, which reading a keyed upload's body takes (PLATFORM-074).
#[derive(Clone)]
pub(crate) struct Avatars {
    pub(in crate::modules::accounts) database: Database,
    pub(in crate::modules::accounts) files: Files,
    pub(in crate::modules::accounts) workers: Workers,
}

impl Avatars {
    /// The avatar routes' state, on `database`, keeping images in `files`, with `workers`.
    pub(crate) fn new(database: Database, files: Files, workers: Workers) -> Self {
        Self {
            database,
            files,
            workers,
        }
    }
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
        if !repo::lock(&mut uow, user, versions).await? {
            return Err(RequestError::PreconditionFailed.into());
        }
        repo::touch(&mut uow, user).await?;
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
    if !repo::lock(&mut uow, user, versions).await? {
        return Err(RequestError::PreconditionFailed.into());
    }
    // Without an avatar, the unit of work rolls back as it is dropped.
    let Some(id) = files::delete_avatar(&mut uow, user).await? else {
        return Ok(());
    };
    repo::touch(&mut uow, user).await?;
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
