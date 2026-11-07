//! The metrics `files` declares (OBS-11).

use platform::telemetry::{metrics::Counter, names};

/// Uploads that reached the check of their bytes, by how each ended.
static UPLOADS: Counter<1> = Counter::new(
    "files.uploads",
    "{upload}",
    "Uploads that reached the check of their bytes, by how each ended: stored, \
     unsupported_type or failed (FILES-013).",
    [names::FILES_OUTCOME],
);

/// How an upload that reached the check of its bytes ended.
#[derive(Clone, Copy, Debug)]
pub(super) enum Upload {
    /// Its row committed (FILES-001).
    Stored,
    /// Its bytes aren't of an accepted kind's (FILES-003).
    UnsupportedType,
    /// Anything else stopped it: the storage, the database, or a refusal of its unit of work.
    Failed,
}

/// Counts an upload that ended as `outcome`.
pub(super) fn upload(outcome: Upload) {
    let label = match outcome {
        Upload::Stored => "stored",
        Upload::UnsupportedType => "unsupported_type",
        Upload::Failed => "failed",
    };
    UPLOADS.add(1, [label.into()]);
}
