//! Writing back what the bucket kept of the objects that were deleted or replaced in it since a
//! moment, which the subcommand `restore-files` runs (the files spec's "Restoring bytes",
//! FILES-028, FILES-029). It reads the bucket alone, writes each such object again by its key, and deletes
//! nothing.

use bytes::{Bytes, BytesMut};
use futures_util::StreamExt;
use platform::{
    errors::Internal,
    storage::{Content, Object, Storage, Version},
    types::Timestamp,
};

use super::{USER_PREFIX, WORKSPACE_PREFIX, avatar::AVATAR_PREFIX, picture::PICTURE_PREFIX};
use crate::modules::files::{error::RestoreFailure, model::LARGEST_UPLOAD_BYTES};

/// What a run of `restore-files` did (FILES-028).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Restored {
    /// How many objects the bucket holds, or keeps a version of, under the module's prefixes.
    pub(crate) seen: u64,
    /// How many of them the run wrote again.
    pub(crate) written: u64,
    /// How many it left as they are though their version at the moment isn't their current one,
    /// since the API writes none such: of more than `LARGEST_UPLOAD_BYTES`, or under a key that
    /// no request's URL names.
    pub(crate) left: u64,
}

/// A run of `restore-files` that stopped before its end: what it had done by then, and why it
/// stopped, for the subcommand's log.
#[derive(Debug)]
pub(crate) struct Stopped {
    pub(crate) done: Restored,
    pub(crate) cause: Internal,
}

/// Which of an object's versions the bucket wrote last at `moment` or before it, among those
/// seen so far.
enum Then {
    /// None seen yet: each was written after the moment.
    Later,
    /// Its current one.
    Current,
    /// This older one.
    Older(Version),
}

/// What the run has seen of one object's versions, which the bucket lists the latest first.
struct Seen {
    /// The latest: what the bucket holds of the object now.
    current: Version,
    then: Then,
}

impl Seen {
    /// An object whose first listed version is `version`, which must be its latest.
    fn first(version: Version, moment: Timestamp) -> Result<Self, RestoreFailure> {
        if !version.latest {
            return Err(RestoreFailure::Order);
        }
        let then = if version.written_at <= moment.at() {
            Then::Current
        } else {
            Then::Later
        };
        Ok(Self {
            current: version,
            then,
        })
    }

    /// Takes `version`, the next that the bucket lists of the object, which is older than those
    /// before it.
    fn older(&mut self, version: Version, moment: Timestamp) -> Result<(), RestoreFailure> {
        if version.latest {
            return Err(RestoreFailure::Order);
        }
        if matches!(self.then, Then::Later) && version.written_at <= moment.at() {
            self.then = Then::Older(version);
        }
        Ok(())
    }

    /// The version to write again, with the bytes it holds: the object's version at the moment,
    /// when it holds bytes and isn't the current one, unless the current one holds bytes of its
    /// size and its entity tag, as after a run that wrote it again.
    fn due(&self) -> Option<(&Version, &Content)> {
        let Then::Older(version) = &self.then else {
            return None;
        };
        let content = version.content.as_ref()?;
        let rewritten = self
            .current
            .content
            .as_ref()
            .is_some_and(|now| now.etag.is_some() && now == content);
        (!rewritten).then_some((version, content))
    }
}

/// Writes again each object of `storage`'s bucket, under the prefix of workspaces' files, then of
/// users', then of avatars, then of workspaces' pictures, whose version at `moment` isn't its
/// current one, from that version, and answers what it did (FILES-028). An object's version at
/// the moment is the latest that the bucket wrote then or before, by the bucket's clock; one
/// that marks a deletion, or none, leaves the object as it is.
pub(crate) async fn restore(storage: &Storage, moment: Timestamp) -> Result<Restored, Stopped> {
    let mut done = Restored::default();
    for prefix in [WORKSPACE_PREFIX, USER_PREFIX, AVATAR_PREFIX, PICTURE_PREFIX] {
        if let Err(cause) = restore_under(storage, prefix, moment, &mut done).await {
            let cause = Internal::new(cause);
            return Err(Stopped { done, cause });
        }
    }
    Ok(done)
}

/// Does under `prefix` what [`restore`] does, counting in `done`. It holds one object's current
/// version, and its version at the moment, at a time, however many versions the bucket keeps.
async fn restore_under(
    storage: &Storage,
    prefix: &str,
    moment: Timestamp,
    done: &mut Restored,
) -> Result<(), RestoreFailure> {
    let mut versions = storage.versions(prefix);
    let mut object: Option<Seen> = None;
    while let Some(version) = versions.next().await {
        let version = version?;
        match &mut object {
            Some(seen) if seen.current.key == version.key => seen.older(version, moment)?,
            _ => {
                // The object before it is whole: it is settled before this one is read.
                if let Some(seen) = object.take() {
                    settle(storage, &seen, done).await?;
                }
                object = Some(Seen::first(version, moment)?);
            }
        }
    }
    if let Some(seen) = object {
        settle(storage, &seen, done).await?;
    }
    Ok(())
}

/// Counts the object `seen` in `done`, and writes it again when its version at the moment is
/// due: reads that version whole, and writes its bytes under its key, with its media type.
async fn settle(storage: &Storage, seen: &Seen, done: &mut Restored) -> Result<(), RestoreFailure> {
    done.seen += 1;
    let Some((version, content)) = seen.due() else {
        return Ok(());
    };
    if !version.key.is_addressed() || content.size > u64::from(LARGEST_UPLOAD_BYTES) {
        done.left += 1;
        return Ok(());
    }
    let kept = storage.version(&version.key, &version.id).await?;
    let bytes = whole(kept.object, content.size).await?;
    storage.put(&version.key, bytes, &kept.media_type).await?;
    done.written += 1;
    Ok(())
}

/// The bytes of `object`, all `size` of them, which is at most `LARGEST_UPLOAD_BYTES`.
async fn whole(mut object: Object, size: u64) -> Result<Bytes, RestoreFailure> {
    let length = usize::try_from(size).map_err(|_| RestoreFailure::Size)?;
    if object.size != size {
        return Err(RestoreFailure::Size);
    }
    let mut bytes = BytesMut::with_capacity(length);
    while let Some(chunk) = object.bytes.next().await {
        let chunk = chunk?;
        if bytes.len() + chunk.len() > length {
            return Err(RestoreFailure::Size);
        }
        bytes.extend_from_slice(&chunk);
    }
    if bytes.len() == length {
        Ok(bytes.freeze())
    } else {
        Err(RestoreFailure::Size)
    }
}

#[cfg(test)]
mod tests {
    use platform::storage::ObjectKey;

    use super::*;

    /// The moment of the tests: the second 30 of a minute.
    fn moment() -> Timestamp {
        Timestamp::read("2026-11-20T12:00:30Z").unwrap()
    }

    /// A version of the object `users/aa` with the ID `id`, written at the second `second` of
    /// the moment's minute, which holds bytes with the entity tag `etag`, or marks a deletion.
    fn version(id: &str, latest: bool, second: u8, etag: Option<&str>) -> Version {
        let written = format!("2026-11-20T12:00:{second:02}Z");
        Version {
            key: ObjectKey::from_stored("users/aa"),
            id: id.to_owned(),
            written_at: Timestamp::read(&written).unwrap().at(),
            latest,
            content: etag.map(|etag| Content {
                etag: Some(etag.to_owned()),
                size: 5,
            }),
        }
    }

    /// The ID of the version to write again of an object whose versions the bucket lists as
    /// `versions`.
    fn due(versions: Vec<Version>) -> Option<String> {
        let mut versions = versions.into_iter();
        let mut seen = Seen::first(versions.next().unwrap(), moment()).unwrap();
        for version in versions {
            seen.older(version, moment()).unwrap();
        }
        seen.due().map(|(version, _)| version.id.clone())
    }

    #[test]
    fn files_028_an_object_deleted_or_replaced_since_the_moment_is_due_from_its_version_then() {
        let deleted = vec![
            version("2", true, 40, None),
            version("1", false, 20, Some("a")),
        ];
        let replaced = vec![
            version("3", true, 50, Some("c")),
            version("2", false, 40, Some("b")),
            version("1", false, 20, Some("a")),
            version("0", false, 10, Some("z")),
        ];
        // The moment's own second counts as before it, and of two versions of one second, the
        // one that the bucket lists first is the later.
        let at_the_moment = vec![
            version("3", true, 31, None),
            version("2", false, 30, Some("b")),
            version("1", false, 30, Some("a")),
        ];

        assert_eq!(due(deleted).as_deref(), Some("1"));
        assert_eq!(due(replaced).as_deref(), Some("1"));
        assert_eq!(due(at_the_moment).as_deref(), Some("2"));
    }

    #[test]
    fn files_028_an_object_is_left_when_its_version_then_is_current_a_deletion_or_none() {
        let unchanged = vec![version("1", true, 20, Some("a"))];
        let written_since = vec![version("1", true, 40, Some("a"))];
        let deleted_since_its_writing = vec![
            version("2", true, 50, None),
            version("1", false, 40, Some("a")),
        ];
        let deleted_before = vec![
            version("3", true, 40, Some("b")),
            version("2", false, 20, None),
            version("1", false, 10, Some("a")),
        ];
        let deleted_in_the_moment_s_second = vec![
            version("2", true, 30, None),
            version("1", false, 30, Some("a")),
        ];

        for versions in [
            unchanged,
            written_since,
            deleted_since_its_writing,
            deleted_before,
            deleted_in_the_moment_s_second,
        ] {
            assert_eq!(due(versions), None);
        }
    }

    #[test]
    fn files_028_an_object_whose_current_version_has_its_bytes_of_then_is_left() {
        let written_again = vec![
            version("3", true, 50, Some("a")),
            version("2", false, 40, None),
            version("1", false, 20, Some("a")),
        ];
        let mut of_another_size = written_again.clone();
        of_another_size[0].content = Some(Content {
            etag: Some("a".to_owned()),
            size: 6,
        });
        let mut untagged = written_again.clone();
        for version in [0, 2] {
            untagged[version].content = Some(Content {
                etag: None,
                size: 5,
            });
        }

        assert_eq!(due(written_again), None);
        assert_eq!(due(of_another_size).as_deref(), Some("1"));
        assert_eq!(due(untagged).as_deref(), Some("1"));
    }

    #[test]
    fn files_029_a_listing_that_isn_t_the_latest_first_stops_the_run() {
        let oldest_first = Seen::first(version("1", false, 20, Some("a")), moment());
        let mut two_latest = Seen::first(version("2", true, 40, None), moment()).unwrap();
        let second = two_latest.older(version("1", true, 20, Some("a")), moment());

        assert!(matches!(oldest_first, Err(RestoreFailure::Order)));
        assert!(matches!(second, Err(RestoreFailure::Order)));
    }
}
