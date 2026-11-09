//! The file's domain types (D25), what the API records of a file, and what its routes answer.

mod exported;
mod kinds;

use std::borrow::Cow;

use platform::{
    errors::Internal,
    types::{Invalid, Timestamp, UserId, WorkspaceId, one_line},
};
use serde::{Deserialize, Serialize};
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{ObjectBuilder, RefOr, Schema, Type},
};

/// The most characters a file's name has (FILES-002).
const NAME_MAX_CHARS: usize = 255;

pub(crate) use exported::ExportedFile;
pub(in crate::modules::files) use exported::{
    AvatarRecord, OwnRecord, WorkspaceRecord, path_segment,
};
pub(crate) use kinds::Kind;

/// A file's name: one line of text of 1 to 255 characters, normalized as `one_line` says, with no
/// control character (FILES-002). A name is personal data, so `Debug` doesn't show it (OBS-04).
/// `Deserialize` only reads back a stored result that an idempotency key replays (PLATFORM-036),
/// which held a parsed name.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct FileName(String);

impl std::fmt::Debug for FileName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FileName([redacted])")
    }
}

impl FileName {
    /// The name in `value`, normalized, or why it isn't one.
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        let name = one_line(
            value,
            NAME_MAX_CHARS,
            "Name the file.",
            "A file's name has at most 255 characters.",
        )?;
        if name.chars().any(char::is_control) {
            return Err(Invalid::new(
                "control_character",
                "A file's name holds no control character.",
            ));
        }
        Ok(Self(name))
    }

    /// A name the database holds, which was normalized when it was stored.
    pub(super) fn from_stored(name: String) -> Self {
        Self(name)
    }

    /// The normalized name.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartialSchema for FileName {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .min_length(Some(1))
            .max_length(Some(NAME_MAX_CHARS))
            .into()
    }
}

impl ToSchema for FileName {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("FileName")
    }
}

/// The settings of `files` (OPS-01).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Settings {
    /// The most bytes an upload holds: `FILE_MAX_BYTES` (FILES-004).
    pub(crate) max_bytes: usize,
    /// The days an object that no row names waits before its bytes go:
    /// `FILE_DELETION_DELAY_DAYS` (FILES-011).
    pub(crate) deletion_delay_days: u32,
}

/// Who may download a file: those who may read it, or anyone (FILES-020).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Visibility {
    /// Only those who may read it: the workspace's members with `files.read`, or the user whose
    /// file it is.
    #[default]
    Private,
    /// Anyone, without signing in, by its public download (FILES-023).
    Public,
}

impl Visibility {
    /// The visibility's name, as the database's `file_visibility` and the API write it.
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Public => "public",
        }
    }

    /// The visibility named `name`, which a query read.
    pub(super) fn from_name(name: &str) -> Result<Self, Internal> {
        match name {
            "private" => Ok(Self::Private),
            "public" => Ok(Self::Public),
            _ => Err(Internal::new("a visibility that files doesn't know")),
        }
    }

    /// The visibility an upload's query names in `value`, or why it isn't one (FILES-020).
    pub(crate) fn parse(value: &str) -> Result<Self, Invalid> {
        Self::from_name(value)
            .map_err(|_| Invalid::new("invalid", "A visibility is private or public."))
    }
}

/// The workspace a file belongs to, and the member who uploaded it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InWorkspace {
    /// The workspace.
    pub(crate) workspace_id: WorkspaceId,
    /// The member who uploaded it, until their account's deletion clears it.
    pub(crate) uploader_id: Option<UserId>,
}

/// The user whose own file it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct OfUser {
    /// The user.
    pub(crate) user_id: UserId,
}

/// What the API records of a file, a workspace's or a user's as `O` says, without its object's
/// key or its links. An upload with an `Idempotency-Key` stores it as its result, which every
/// replay links anew (FILES-021, DATA-10).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Recorded<O> {
    /// The file's ID.
    pub(crate) id: uuid::Uuid,
    /// Whose file it is.
    #[serde(flatten)]
    pub(crate) owner: O,
    /// Its name.
    pub(crate) name: FileName,
    /// The media type of its kind (FILES-003).
    pub(crate) media_type: String,
    /// Its size, in bytes.
    pub(crate) size_bytes: u64,
    /// Who may download it (FILES-020).
    pub(crate) visibility: Visibility,
    /// When it was uploaded.
    pub(crate) created_at: Timestamp,
}

/// A file as the routes that serve its bytes need it: the file, and the key of its object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stored<O> {
    /// The file.
    pub(crate) file: Recorded<O>,
    /// The key of its object in the bucket.
    pub(crate) object_key: String,
}

/// A file's links to its bytes (FILES-021).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Link {
    /// The path of its bytes: a public file's public download, or a private file's download,
    /// signed for the caller's session.
    pub(crate) content_url: String,
    /// When a signed path stops working, or `None` for a public download's, which never does.
    pub(crate) content_url_expires_at: Option<Timestamp>,
}

/// A file of a workspace, as its routes answer it (FILES-001, FILES-008).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub(crate) struct File {
    /// The file's ID.
    pub(crate) id: uuid::Uuid,
    /// The workspace it belongs to.
    pub(crate) workspace_id: WorkspaceId,
    /// Its name.
    pub(crate) name: FileName,
    /// The media type of its kind (FILES-003).
    pub(crate) media_type: String,
    /// Its size, in bytes.
    pub(crate) size_bytes: u64,
    /// Who may download it (FILES-020).
    pub(crate) visibility: Visibility,
    /// The member who uploaded it, until their account's deletion clears it.
    pub(crate) uploader_id: Option<UserId>,
    /// The path of its bytes, after the API's address: its public download for a public file,
    /// and its download, signed for the caller's session, for a private one (FILES-021).
    pub(crate) content_url: String,
    /// When `content_url` stops working, an hour after the response was made, or null for a
    /// public file's, which never does (FILES-021).
    pub(crate) content_url_expires_at: Option<Timestamp>,
    /// When it was uploaded.
    pub(crate) created_at: Timestamp,
}

impl File {
    /// The workspace's file `file`, with its links `link`.
    pub(crate) fn linked(file: Recorded<InWorkspace>, link: Link) -> Self {
        Self {
            id: file.id,
            workspace_id: file.owner.workspace_id,
            name: file.name,
            media_type: file.media_type,
            size_bytes: file.size_bytes,
            visibility: file.visibility,
            uploader_id: file.owner.uploader_id,
            content_url: link.content_url,
            content_url_expires_at: link.content_url_expires_at,
            created_at: file.created_at,
        }
    }
}

/// A file of the caller's own, as its routes answer it (FILES-016, FILES-018).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub(crate) struct UserFile {
    /// The file's ID.
    pub(crate) id: uuid::Uuid,
    /// The user whose file it is: the caller.
    pub(crate) user_id: UserId,
    /// Its name.
    pub(crate) name: FileName,
    /// The media type of its kind (FILES-003).
    pub(crate) media_type: String,
    /// Its size, in bytes.
    pub(crate) size_bytes: u64,
    /// Who may download it (FILES-020).
    pub(crate) visibility: Visibility,
    /// The path of its bytes, after the API's address: its public download for a public file,
    /// and its download, signed for the caller's session, for a private one (FILES-021).
    pub(crate) content_url: String,
    /// When `content_url` stops working, an hour after the response was made, or null for a
    /// public file's, which never does (FILES-021).
    pub(crate) content_url_expires_at: Option<Timestamp>,
    /// When it was uploaded.
    pub(crate) created_at: Timestamp,
}

impl UserFile {
    /// The user's file `file`, with its links `link`.
    pub(crate) fn linked(file: Recorded<OfUser>, link: Link) -> Self {
        Self {
            id: file.id,
            user_id: file.owner.user_id,
            name: file.name,
            media_type: file.media_type,
            size_bytes: file.size_bytes,
            visibility: file.visibility,
            content_url: link.content_url,
            content_url_expires_at: link.content_url_expires_at,
            created_at: file.created_at,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_020_a_visibility_is_private_or_public() {
        assert_eq!(Visibility::parse("private").unwrap(), Visibility::Private);
        assert_eq!(Visibility::parse("public").unwrap(), Visibility::Public);
        assert_eq!(Visibility::parse("Public").unwrap_err().code, "invalid");
        assert_eq!(Visibility::default(), Visibility::Private);
    }

    #[test]
    fn files_002_a_name_is_trimmed_collapsed_and_normalized() {
        let name = FileName::parse("  Blue \t\n  one  ").unwrap();
        assert_eq!(name.as_str(), "Blue one");
        // "e" followed by a combining acute accent becomes one character in form C.
        let composed = FileName::parse("Caf\u{65}\u{301}").unwrap();
        assert_eq!(composed.as_str(), "Caf\u{e9}");
    }

    #[test]
    fn files_002_a_name_holds_no_control_character() {
        assert_eq!(
            FileName::parse("a\u{7}b.txt").unwrap_err().code,
            "control_character"
        );
        assert_eq!(
            FileName::parse("a\u{202e}b.txt").unwrap().as_str(),
            "a\u{202e}b.txt"
        );
    }

    #[test]
    fn files_002_a_name_has_1_to_255_characters() {
        assert_eq!(FileName::parse(" \t ").unwrap_err().code, "required");
        assert!(FileName::parse(&"é".repeat(255)).is_ok());
        assert_eq!(
            FileName::parse(&"a".repeat(256)).unwrap_err().code,
            "too_long"
        );
    }
}
