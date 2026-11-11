//! Exporting a user's data (the accounts spec's "Exporting", D31, D126): requesting an export,
//! reading the caller's exports, building an export's archive in its job, and downloading it.

use std::{ops::Range, sync::Arc, time::Duration};

use bytes::Bytes;
use futures_util::{
    StreamExt,
    stream::{self, BoxStream},
};
use platform::{
    archive::{Layout, Piece},
    authn::{AuditScope, BoxFuture, Principal},
    blocking::Workers,
    db::{Database, UnitOfWork},
    errors::{self, Internal},
    jobs::{JobContext, JobFailure},
    lists::{List, Page, key},
    mail::Mail,
    personal_data::{ExportArchive, ExportStore, PersonalData},
    ranges::{Served, Wanted},
    rate_limit::{Bucket, RateLimits},
    types::{Client, Timestamp, UserId},
};

use crate::modules::{
    PRODUCT,
    accounts::{
        audit, emails,
        error::AccountsError,
        events as published,
        model::{Export, ExportState},
        repo::{self, exports as stored},
    },
    audit::{self as log, AuditError, Entry},
};
// part: files
use crate::modules::files::{self, ExportedFile, Files};
// end part: files

/// The bucket of each user's exports: 1 in 24 hours, fixed (ACCOUNTS-021).
pub(crate) const EXPORT_BUCKET: Bucket = Bucket {
    name: "export",
    limit: 1,
    window: Duration::from_secs(86_400),
};

/// How long an export lasts, from its request and again from the moment it is ready (D127).
const LIFETIME: Duration = Duration::from_secs(7 * 86_400);

/// How many chunks of a document a download reads at once, each read in a transaction of its own.
const CHUNKS_AT_ONCE: i64 = 4;

/// The bytes of an archive, or of a range of it, as a download streams them.
pub(crate) type ArchiveBytes = BoxStream<'static, Result<Bytes, Internal>>;

/// What a download of an export serves: its archive's size, name and entity tag, what of its
/// bytes `Wanted` asked for, and those bytes, or none when the client holds them (ACCOUNTS-026).
pub(crate) struct Download {
    pub(crate) size: u64,
    pub(crate) name: String,
    pub(crate) tag: String,
    pub(crate) served: Served,
    pub(crate) bytes: Option<ArchiveBytes>,
}

/// The state of exports: the database, the email that tells a user their export is ready, the
/// workers that deflate its documents, and every module's personal data contract, in the order
/// the composition root registers them, that of `accounts` last.
#[derive(Clone)]
pub(crate) struct Exports {
    pub(in crate::modules::accounts) database: Database,
    mail: Mail,
    workers: Workers,
    contracts: Arc<[Arc<dyn PersonalData>]>,
    // part: files
    files: Files,
    // end part: files
}

impl Exports {
    /// Exports on `database`, sending their email through `mail`, deflating through `workers`,
    /// through `contracts`, in their order.
    pub(crate) fn new(
        database: Database,
        mail: Mail,
        workers: Workers,
        contracts: Vec<Arc<dyn PersonalData>>,
        // part: files
        files: Files,
        // end part: files
    ) -> Self {
        Self {
            database,
            mail,
            workers,
            contracts: contracts.into(),
            // part: files
            files,
            // end part: files
        }
    }

    /// Requests an export of the caller's data, once their bucket counts it, in a unit of work
    /// of `client`'s request that records it in their own log and enqueues its job
    /// (ACCOUNTS-020, ACCOUNTS-021). `auth` calls it, once the caller's session has proved who
    /// they are just before (D137).
    pub(crate) async fn request(
        &self,
        limits: &RateLimits,
        client: &Client,
        principal: &Principal,
    ) -> Result<Export, AccountsError> {
        let user = principal.user_id();
        limits.check(EXPORT_BUCKET, user.uuid().as_bytes()).await?;
        let mut uow = self.database.begin(client).await?;
        let export = stored::insert(&mut uow, user, LIFETIME).await?;
        let entry = Entry::new(audit::EXPORT_REQUESTED, audit::EXPORT, export.id);
        log::record(&mut uow, principal.audit_scope(), entry).await?;
        uow.enqueue(
            principal.job_scope(),
            &crate::modules::accounts::jobs::Build {
                export_id: export.id,
            },
        )
        .await?;
        uow.commit().await?;
        Ok(export)
    }

    /// A page of the caller's exports, the latest first (ACCOUNTS-022).
    pub(in crate::modules::accounts) async fn list(
        &self,
        client: &Client,
        principal: &Principal,
        page: &Page,
    ) -> Result<List<Export>, AccountsError> {
        let mut uow = self.database.begin(client).await?;
        let rows = stored::page(&mut uow, principal.user_id(), page).await?;
        Ok(page.list(rows, |export| key(export.created_at, export.id)))
    }

    /// The caller's export `id` (ACCOUNTS-022).
    pub(in crate::modules::accounts) async fn read(
        &self,
        client: &Client,
        principal: &Principal,
        id: uuid::Uuid,
    ) -> Result<Export, AccountsError> {
        let mut uow = self.database.begin(client).await?;
        stored::read(&mut uow, principal.user_id(), id)
            .await?
            .ok_or(AccountsError::ExportNotFound)
    }

    /// What a download of the caller's export `id` serves, as `wanted` asks for it: its archive,
    /// laid out from its documents and any other entries a module holds for it, recording
    /// `export.downloaded` in the caller's own log when it serves bytes, in a unit of work
    /// of `client`'s request that commits before the bytes stream (ACCOUNTS-026, D126).
    pub(in crate::modules::accounts) async fn download(
        &self,
        client: &Client,
        principal: &Principal,
        id: uuid::Uuid,
        wanted: &Wanted,
    ) -> Result<Download, AccountsError> {
        let user = principal.user_id();
        let mut uow = self.database.begin(client).await?;
        let export = stored::read(&mut uow, user, id)
            .await?
            .ok_or(AccountsError::ExportNotFound)?;
        let ready_at = match (export.state, export.ready_at) {
            (ExportState::Ready, Some(ready_at)) => ready_at,
            _ => return Err(AccountsError::ExportNotReady),
        };
        let (positions, entries): (Vec<u32>, Vec<_>) = stored::entries(&mut uow, user, id)
            .await?
            .into_iter()
            .unzip();
        // part: files
        let held = files::exported(&mut uow, user, id).await?;
        let entries = entries
            .into_iter()
            .chain(held.iter().map(ExportedFile::entry))
            .collect();
        // end part: files
        let layout = Layout::new(entries, ready_at.at());
        let tag = id.to_string();
        let served = wanted.serve(layout.size(), &tag);
        // A `HEAD` serves no bytes, so it records no download (PLATFORM-081).
        if wanted.serves_bytes() && served != Served::NotModified {
            let entry = Entry::new(audit::EXPORT_DOWNLOADED, audit::EXPORT, id);
            log::record(&mut uow, principal.audit_scope(), entry).await?;
        }
        uow.commit().await?;
        let range = served.offsets().unwrap_or(0..layout.size());
        let sources = Sources {
            database: self.database.clone(),
            client: client.clone(),
            user,
            export: id,
            positions: positions.into(),
            // part: files
            files: self.files.clone(),
            held: held.into(),
            // end part: files
        };
        let bytes = (served != Served::NotModified).then(|| sources.stream(&layout, range));
        Ok(Download {
            size: layout.size(),
            name: format!("{PRODUCT}-export-{}.zip", day(ready_at)),
            tag,
            served,
            bytes,
        })
    }

    /// Builds the archive of the export `id`, in a run of its job for the user it runs for, in
    /// one unit of work under `REPEATABLE READ`, and makes it ready; a run of its last attempt
    /// that fails makes it failed instead (ACCOUNTS-023).
    pub(in crate::modules::accounts) async fn build(
        &self,
        context: &JobContext,
        id: uuid::Uuid,
        last_attempt: bool,
    ) -> Result<(), JobFailure> {
        let (Some(user), Some(scope)) = (context.scope().user(), context.audit_scope()) else {
            return Err(JobFailure::Permanent(Internal::new(
                "an export's job runs for no one",
            )));
        };
        let built = self.archive(context, scope, user, id).await;
        if built.is_err()
            && last_attempt
            && let Err(error) = self.fail(context, scope, user, id).await
        {
            // The attempt's own failure is the run's, which the dispatcher writes; the export
            // stays pending, which the expiry makes failed (ACCOUNTS-027).
            errors::ignore(&error, "export_failure_not_recorded");
        }
        built
    }

    /// Writes the archive of `user`'s export `id`, while it is pending, then makes it ready,
    /// records `export.completed` in the user's own log through `scope`, enqueues its email and
    /// publishes `export.completed`, all in one unit of work.
    async fn archive(
        &self,
        context: &JobContext,
        scope: AuditScope,
        user: UserId,
        id: uuid::Uuid,
    ) -> Result<(), JobFailure> {
        let mut uow = self.database.for_snapshot_job(context).await?;
        // The first statement, which locks the export's row before any other write (DATA-20).
        if stored::lock(&mut uow, user, id).await? != Some(ExportState::Pending) {
            return Ok(());
        }
        let store: Arc<dyn ExportStore> = Arc::new(Store { user });
        let mut archive = ExportArchive::new(id, user, store, self.workers.clone());
        for contract in self.contracts.iter() {
            contract.export(&mut uow, &mut archive).await?;
        }
        let entries: Vec<_> = stored::entries(&mut uow, user, id)
            .await?
            .into_iter()
            .map(|(_, entry)| entry)
            .collect();
        // part: files
        let held = files::exported(&mut uow, user, id).await?;
        let entries = entries
            .into_iter()
            .chain(held.iter().map(ExportedFile::entry))
            .collect();
        // end part: files
        // An entry's time takes as many bytes whatever it is, so no time changes the size.
        let size = Layout::new(entries, time::OffsetDateTime::UNIX_EPOCH).size();
        let export = stored::ready(&mut uow, user, id, size, LIFETIME).await?;
        let entry = Entry::new(audit::EXPORT_COMPLETED, audit::EXPORT, id);
        log::record(&mut uow, scope, entry)
            .await
            .map_err(internal)?;
        let to = repo::profile(&mut uow, user)
            .await?
            .ok_or_else(|| Internal::new("the export's user doesn't exist"))?
            .profile
            .email;
        let email = emails::ExportReady {
            to,
            expires_at: export.expires_at,
        }
        .render()?;
        self.mail.send(&mut uow, context.scope(), email).await?;
        let payload = published::ExportChanged { export_id: id };
        uow.publish_to_user(&published::EXPORT_COMPLETED, user, &payload)
            .await?;
        uow.commit().await?;
        Ok(())
    }

    /// Makes `user`'s export `id` failed, if it is still pending, records `export.failed` in the
    /// user's own log through `scope`, and publishes `export.failed`, in a unit of work of its
    /// own, once the run's has rolled back (ACCOUNTS-023, ARCH-11).
    async fn fail(
        &self,
        context: &JobContext,
        scope: AuditScope,
        user: UserId,
        id: uuid::Uuid,
    ) -> Result<(), Internal> {
        let mut uow = self.database.for_job(context).await?;
        if stored::fail(&mut uow, user, id).await? {
            let entry = Entry::new(audit::EXPORT_FAILED, audit::EXPORT, id);
            log::record(&mut uow, scope, entry)
                .await
                .map_err(internal)?;
            let payload = published::ExportChanged { export_id: id };
            uow.publish_to_user(&published::EXPORT_FAILED, user, &payload)
                .await?;
        }
        uow.commit().await
    }
}

/// An audit entry's failure, as the internal error it is in a job's run.
fn internal(error: AuditError) -> Internal {
    match error {
        AuditError::Internal(error) => error,
    }
}

/// `at`'s day, in UTC, as `YYYY-MM-DD`, which an archive's name holds.
fn day(at: Timestamp) -> String {
    let at = at.at().to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}",
        at.year(),
        u8::from(at.month()),
        at.day()
    )
}

/// The store of an export's documents, in the unit of work of the job of `user`'s export, which
/// filters every statement by them (DATA-16).
struct Store {
    user: UserId,
}

impl ExportStore for Store {
    fn chunk<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        export: uuid::Uuid,
        position: u32,
        offset: u64,
        bytes: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(async move {
            stored::insert_chunk(uow, self.user, export, position, offset, &bytes).await
        })
    }

    fn entry<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        export: uuid::Uuid,
        position: u32,
        entry: platform::archive::Entry,
    ) -> BoxFuture<'a, Result<(), Internal>> {
        Box::pin(
            async move { stored::insert_entry(uow, self.user, export, position, &entry).await },
        )
    }
}

/// Where a download's bytes come from: the documents of `user`'s export, in the database, and
/// any other entries a module holds for it.
#[derive(Clone)]
struct Sources {
    database: Database,
    client: Client,
    user: UserId,
    export: uuid::Uuid,
    /// The position of each of the archive's documents, which come first, in their order.
    positions: Arc<[u32]>,
    // part: files
    files: Files,
    held: Arc<[ExportedFile]>,
    // end part: files
}

impl Sources {
    /// The archive's bytes in `range`, piece by piece, as `layout` lays them out.
    fn stream(self, layout: &Layout, range: Range<u64>) -> ArchiveBytes {
        stream::iter(layout.pieces(range))
            .flat_map(move |piece| match piece {
                Piece::Bytes(bytes) => stream::once(async move { Ok(bytes) }).boxed(),
                Piece::Entry { index, offsets } => self.clone().entry(index, offsets),
            })
            .boxed()
    }

    /// The stored bytes at `offsets` of the entry at `index`: a document's, which come first,
    /// or another entry's that a module holds.
    fn entry(self, index: usize, offsets: Range<u64>) -> ArchiveBytes {
        // part: files
        if let Some(file) = index.checked_sub(self.positions.len()) {
            return self.file(file, offsets);
        }
        // end part: files
        self.document(index, offsets)
    }

    /// The deflated bytes at `offsets` of the archive's document at `index`, read a few chunks at
    /// a time, each read in a transaction of its own, so a slow client holds no transaction open.
    fn document(self, index: usize, offsets: Range<u64>) -> ArchiveBytes {
        let Some(&position) = self.positions.get(index) else {
            return stream::once(async {
                Err(Internal::new("an archive's entry that no document is"))
            })
            .boxed();
        };
        stream::unfold(Some(offsets), move |next| {
            let sources = self.clone();
            async move {
                let offsets = next?;
                if offsets.is_empty() {
                    return None;
                }
                match sources.read(position, offsets.clone()).await {
                    Ok((bytes, end)) => Some((Ok(bytes), Some(end..offsets.end))),
                    Err(error) => Some((Err(error), None)),
                }
            }
        })
        .boxed()
    }

    /// The bytes at `offsets` of the document at `position` that the next few of its chunks
    /// hold, joined, with the offset after them.
    async fn read(&self, position: u32, offsets: Range<u64>) -> Result<(Bytes, u64), Internal> {
        let mut uow = self.database.begin(&self.client).await?;
        let chunks = stored::chunks(
            &mut uow,
            self.user,
            self.export,
            position,
            offsets.clone(),
            CHUNKS_AT_ONCE,
        )
        .await?;
        uow.commit().await?;
        let mut joined = Vec::new();
        let mut at = offsets.start;
        for (start, bytes) in chunks {
            let end = start + bytes.len() as u64;
            if start > at || end <= at {
                return Err(Internal::new(
                    "an export's document lacks a chunk of its bytes",
                ));
            }
            let from = usize::try_from(at - start).map_err(Internal::new)?;
            let to = usize::try_from(end.min(offsets.end) - start).map_err(Internal::new)?;
            joined.extend_from_slice(&bytes[from..to]);
            at = end.min(offsets.end);
            if at == offsets.end {
                break;
            }
        }
        if at == offsets.start {
            return Err(Internal::new(
                "an export's document lacks a chunk of its bytes",
            ));
        }
        Ok((Bytes::from(joined), at))
    }

    // part: files
    /// The bytes at `offsets` of the file at `index` among those the archive holds, from the
    /// bucket.
    fn file(self, index: usize, offsets: Range<u64>) -> ArchiveBytes {
        stream::once(async move {
            let file = self
                .held
                .get(index)
                .ok_or_else(|| Internal::new("an archive's entry that no file is"))?;
            files::exported_bytes(&self.files, file, offsets).await
        })
        .flat_map(|bytes| match bytes {
            Ok(bytes) => bytes,
            Err(error) => stream::once(async move { Err(error) }).boxed(),
        })
        .boxed()
    }
    // end part: files
}
