//! The metrics `files` declares (OBS-11).

use platform::telemetry::{metrics::Counter, names};

/// Uploads that reached the check of their bytes, by how each ended.
static UPLOADS: Counter<1> = Counter::new(
    "files.uploads",
    "{upload}",
    "Uploads that reached the check of their bytes, by how each ended: stored, \
     unsupported_type, malware_detected, or failed, which a scanner that doesn't answer, and the \
     route's timeout, count in too (FILES-013).",
    [names::FILES_OUTCOME],
);

/// How an upload that reached the check of its bytes ended.
#[derive(Clone, Copy, Debug)]
pub(super) enum Upload {
    /// Its row committed (FILES-001).
    Stored,
    /// Its bytes aren't of an accepted kind's (FILES-003).
    UnsupportedType,
    /// The scanner refused its bytes (FILES-005).
    MalwareDetected,
    /// Anything else stopped it: the scanner, the storage, the database, a refusal of its unit of
    /// work, or its route's timeout.
    Failed,
}

/// An upload that reached the check of its bytes, which counts once: as the outcome that `end`
/// gives it, or as failed when it is dropped before, as when its route's timeout ends it.
#[must_use = "an upload counts as failed once this is dropped"]
pub(super) struct Counted {
    ended: bool,
}

impl Counted {
    /// An upload that has reached the check of its bytes.
    pub(super) fn start() -> Self {
        Self { ended: false }
    }

    /// Counts the upload as `outcome`.
    pub(super) fn end(mut self, outcome: Upload) {
        self.ended = true;
        count(outcome);
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        if !self.ended {
            count(Upload::Failed);
        }
    }
}

/// Counts an upload that ended as `outcome`.
fn count(outcome: Upload) {
    let label = match outcome {
        Upload::Stored => "stored",
        Upload::UnsupportedType => "unsupported_type",
        Upload::MalwareDetected => "malware_detected",
        Upload::Failed => "failed",
    };
    UPLOADS.add(1, [label.into()]);
}
