//! The subcommand `restore-files`, which an operator runs against the bucket that holds files'
//! bytes (ARCH-14, FILES-028): what it takes, and what it does, which `main.rs` runs with the
//! bucket's settings and logs.

use platform::{
    clock::SharedClock,
    errors::Internal,
    storage::{Storage, StorageConfig},
    types::Timestamp,
};

use crate::modules::files;

/// What the subcommand `restore-files` takes.
#[derive(clap::Args)]
pub struct RestoreFiles {
    /// The moment, in UTC, as `YYYY-MM-DDTHH:MM:SSZ`, with up to 6 digits of a second's fraction
    /// before the `Z`: each object that was deleted or replaced since then is written again as
    /// the bucket held it then.
    moment: String,
}

/// Why the subcommand `restore-files` wrote nothing, or stopped before its end.
#[derive(Debug, thiserror::Error)]
pub enum RestoreRefused {
    /// What the operator gave isn't a moment.
    #[error(
        "the moment given isn't one, written YYYY-MM-DDTHH:MM:SSZ, in UTC, such as \
         2026-10-25T10:30:00Z: nothing changed"
    )]
    NotAMoment,
    /// The bucket's client couldn't be built.
    #[error(transparent)]
    Client(#[from] Internal),
    /// The run stopped before its end, with objects written again by then.
    #[error("stopped once it had written {written} objects again; a run after it writes the rest")]
    Stopped {
        /// How many objects it had written again.
        written: u64,
        /// Why it stopped.
        #[source]
        cause: Internal,
    },
}

impl RestoreFiles {
    /// Writes again each object of the bucket that `bucket` describes whose version at the
    /// moment isn't its current one, from the version that the bucket kept, waiting for the
    /// bucket's answers on `clock`, and answers what it did, for the subcommand's log
    /// (FILES-028, FILES-029).
    pub async fn run(
        self,
        bucket: StorageConfig,
        clock: SharedClock,
    ) -> Result<String, RestoreRefused> {
        let moment = Timestamp::read(&self.moment).map_err(|_| RestoreRefused::NotAMoment)?;
        let storage = Storage::new(&bucket, clock)?;
        let files::Restored {
            seen,
            written,
            left,
        } = files::restore(&storage, moment)
            .await
            .map_err(|files::Stopped { done, cause }| RestoreRefused::Stopped {
                written: done.written,
                cause,
            })?;
        Ok(format!(
            "wrote again what the bucket held at the moment and holds no more: objects seen \
             {seen}, written again {written}, left as they are {left}"
        ))
    }
}
