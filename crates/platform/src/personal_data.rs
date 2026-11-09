//! The personal data contract (D31): what each module that keeps personal data declares, how it
//! exports a user's into the archive of their export, and how it erases them when their account is
//! deleted, which the composition root registers in the order a deletion runs them (the platform
//! spec's "Personal data"). `platform` implements it for its own tables, as [`Contract`].

use std::{io::Write, mem, sync::Arc};

use flate2::{Compression, write::DeflateEncoder};
use serde::Serialize;
use uuid::Uuid;

use crate::{
    archive,
    authn::{BoxFuture, Principal},
    blocking::Workers,
    db::UnitOfWork,
    errors::Internal,
    jobs,
    types::UserId,
};

/// How many records a contract reads at once of a kind whose number grows with what the user
/// does, and writes before it reads the next (the platform spec's "Personal data").
pub const PAGE: i64 = 1_000;

/// The last record of `page`, a page of at most [`PAGE`] that a contract read, when the page is
/// full, so the next one starts after it, or `None` once it is the last.
pub fn next_after<T>(page: &[T]) -> Option<&T> {
    i64::try_from(page.len())
        .is_ok_and(|length| length >= PAGE)
        .then(|| page.last())
        .flatten()
}

/// `items`, such as the IDs of the workspaces whose only member is the user, in slices of at most
/// [`PAGE`], which a contract reads a slice at a time.
pub fn pages<T>(items: &[T]) -> impl Iterator<Item = &[T]> {
    items.chunks(usize::try_from(PAGE).unwrap_or(1))
}

/// The most bytes of a chunk of a document's deflated bytes, which the store gets as each fills
/// (PLATFORM-084).
pub const CHUNK: usize = 1 << 20;

/// A column that references `users`, or that its migration comments `personal_data` (DATA-23).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Column {
    /// Its table.
    pub table: &'static str,
    /// Its name.
    pub name: &'static str,
}

impl Column {
    /// The column `name` of `table`.
    pub const fn new(table: &'static str, name: &'static str) -> Self {
        Self { table, name }
    }
}

/// Why an erasure stopped, which rolls its unit of work back.
#[derive(Debug, thiserror::Error)]
pub enum ErasureError {
    /// A lock that the erasure waited for longer than its unit of work allows, or a row that a
    /// request of the user's added while it ran, interrupted it, so the deletion is sent again
    /// (ACCOUNTS-016).
    #[error("the account changed while it was being deleted")]
    Interrupted,
    /// Anything else (ERR-04).
    #[error(transparent)]
    Internal(#[from] Internal),
}

/// A module's personal data contract (D31), in its `personal_data.rs`.
pub trait PersonalData: Send + Sync {
    /// Every column of the module's tables that references `users`, or that its migration
    /// comments `personal_data`.
    fn columns(&self) -> &'static [Column];

    /// Adds what the module keeps of the user whose export `export` is to its archive, in the
    /// unit of work of the export's job, as the accounts spec's "What an export holds" says.
    fn export<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        export: &'a mut ExportArchive,
    ) -> BoxFuture<'a, Result<(), Internal>>;

    /// Erases what the module keeps of the user that `principal` proves, in the unit of work of
    /// their account's deletion, as the module's spec says: it deletes rows, or leaves them to
    /// the foreign keys that delete them, or set to null what names the user, as the user's row
    /// goes.
    fn erase<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        principal: &'a Principal,
    ) -> BoxFuture<'a, Result<(), ErasureError>>;
}

/// The contract of `platform`'s own tables: the jobs enqueued for the user, whose payloads may
/// hold personal data, such as an email's address, which its erasure deletes, and their events
/// and idempotency records, which go with the user's row through their foreign keys (DATA-23).
pub struct Contract;

impl PersonalData for Contract {
    fn columns(&self) -> &'static [Column] {
        const COLUMNS: &[Column] = &[
            Column::new("events", "user_id"),
            Column::new("idempotency_records", "user_id"),
            Column::new("idempotency_records", "result"),
            Column::new("jobs", "payload"),
        ];
        COLUMNS
    }

    fn export<'a>(
        &'a self,
        _: &'a mut UnitOfWork,
        _: &'a mut ExportArchive,
    ) -> BoxFuture<'a, Result<(), Internal>> {
        // Its tables are bookkeeping, of data that the modules that own it export (DATA-23).
        Box::pin(async { Ok(()) })
    }

    fn erase<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        principal: &'a Principal,
    ) -> BoxFuture<'a, Result<(), ErasureError>> {
        Box::pin(async move { Ok(jobs::forget_user(uow, principal.user_id()).await?) })
    }
}

/// What keeps an export's documents, in the unit of work of its job, which `accounts` implements.
pub trait ExportStore: Send + Sync {
    /// Stores `bytes`, a chunk of the deflated bytes of the document at `position` of the export
    /// `export`, at `offset` among them.
    fn chunk<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        export: Uuid,
        position: u32,
        offset: u64,
        bytes: Vec<u8>,
    ) -> BoxFuture<'a, Result<(), Internal>>;

    /// Stores `entry`, the document at `position` of the export `export`, once every chunk of it
    /// is stored.
    fn entry<'a>(
        &'a self,
        uow: &'a mut UnitOfWork,
        export: Uuid,
        position: u32,
        entry: archive::Entry,
    ) -> BoxFuture<'a, Result<(), Internal>>;
}

/// The archive of a user's export, which each contract's `export` adds its documents to
/// (PLATFORM-084).
pub struct ExportArchive {
    id: Uuid,
    user: UserId,
    store: Arc<dyn ExportStore>,
    workers: Workers,
    next: u32,
}

/// A document that a contract is writing: its position and path, its encoder until it is finished,
/// and the deflated bytes that the store doesn't have yet, with what its entry gives.
pub struct Document {
    position: u32,
    path: String,
    encoder: Option<DeflateEncoder<Vec<u8>>>,
    deflated: Vec<u8>,
    crc: crc32fast::Hasher,
    size: u64,
    stored: u64,
    records: u64,
}

impl ExportArchive {
    /// The archive of the export `id`, of `user`'s data, whose documents `store` keeps, and which
    /// deflates them through `workers` (CODE-05).
    pub fn new(id: Uuid, user: UserId, store: Arc<dyn ExportStore>, workers: Workers) -> Self {
        Self {
            id,
            user,
            store,
            workers,
            next: 0,
        }
    }

    /// The export's ID.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// The user whose data it exports, whom the job runs for, by whose ID a contract's queries
    /// filter (DATA-16).
    pub fn user(&self) -> UserId {
        self.user
    }

    /// Starts the document at `path`, the next in the archive.
    pub fn document(&mut self, path: impl Into<String>) -> Document {
        let position = self.next;
        self.next += 1;
        Document {
            position,
            path: path.into(),
            encoder: Some(DeflateEncoder::new(Vec::new(), Compression::default())),
            deflated: Vec::new(),
            crc: crc32fast::Hasher::new(),
            size: 0,
            stored: 0,
            records: 0,
        }
    }

    /// Writes the document at `path`, whose records are `records`, whole.
    pub async fn whole<T: Serialize>(
        &mut self,
        uow: &mut UnitOfWork,
        path: impl Into<String>,
        records: &[T],
    ) -> Result<(), Internal> {
        let mut document = self.document(path);
        self.write(uow, &mut document, records).await?;
        self.finish(uow, document).await
    }

    /// Adds `records`, a page of them, to `document`, each on a line of its own, deflated through
    /// the workers, and hands the store each chunk of its deflated bytes that is full.
    pub async fn write<T: Serialize>(
        &self,
        uow: &mut UnitOfWork,
        document: &mut Document,
        records: &[T],
    ) -> Result<(), Internal> {
        let mut text = Vec::new();
        for record in records {
            text.extend_from_slice(if document.records == 0 {
                b"[\n"
            } else {
                b",\n"
            });
            serde_json::to_writer(&mut text, record).map_err(Internal::new)?;
            document.records += 1;
        }
        if text.is_empty() {
            return Ok(());
        }
        self.deflate(document, text, false).await?;
        self.hand_over(uow, document, CHUNK).await
    }

    /// Ends `document`, `]` after its last record or `[]` for none, then hands the store the rest
    /// of its deflated bytes, and its entry.
    pub async fn finish(
        &self,
        uow: &mut UnitOfWork,
        mut document: Document,
    ) -> Result<(), Internal> {
        let end: &[u8] = if document.records == 0 {
            b"[]\n"
        } else {
            b"\n]\n"
        };
        self.deflate(&mut document, end.to_vec(), true).await?;
        self.hand_over(uow, &mut document, 1).await?;
        let entry = archive::Entry {
            path: document.path,
            size: document.size,
            stored_size: document.stored,
            crc32: document.crc.finalize(),
            deflated: true,
        };
        self.store
            .entry(uow, self.id, document.position, entry)
            .await
    }

    /// Adds `text` to `document`'s checksum and size, and deflates it, on a turn of the workers,
    /// which computes the checksum too, onto the deflated bytes that the store doesn't have yet;
    /// `last` ends the deflated stream, and the encoder with it (CODE-05).
    async fn deflate(
        &self,
        document: &mut Document,
        text: Vec<u8>,
        last: bool,
    ) -> Result<(), Internal> {
        document.size += text.len() as u64;
        let mut encoder = document
            .encoder
            .take()
            .ok_or_else(|| Internal::new("a finished document was written to"))?;
        let mut crc = mem::take(&mut document.crc);
        let (encoder, deflated, crc) = self
            .workers
            .run("export deflate", move || -> std::io::Result<_> {
                crc.update(&text);
                encoder.write_all(&text)?;
                if last {
                    return Ok((None, encoder.finish()?, crc));
                }
                let deflated = mem::take(encoder.get_mut());
                Ok((Some(encoder), deflated, crc))
            })
            .await
            .map_err(Internal::new)?
            .map_err(Internal::new)?;
        document.crc = crc;
        document.encoder = encoder;
        document.deflated.extend_from_slice(&deflated);
        Ok(())
    }

    /// Hands the store each chunk of `document`'s deflated bytes that holds `at_least` bytes or
    /// more, of at most [`CHUNK`] bytes each, keeping the rest.
    async fn hand_over(
        &self,
        uow: &mut UnitOfWork,
        document: &mut Document,
        at_least: usize,
    ) -> Result<(), Internal> {
        loop {
            let buffer = &mut document.deflated;
            if buffer.len() < at_least || buffer.is_empty() {
                return Ok(());
            }
            let rest = buffer.split_off(buffer.len().min(CHUNK));
            let chunk = mem::replace(buffer, rest);
            let offset = document.stored;
            document.stored += chunk.len() as u64;
            self.store
                .chunk(uow, self.id, document.position, offset, chunk)
                .await?;
        }
    }
}
