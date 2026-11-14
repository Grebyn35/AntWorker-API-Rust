//! What an export holds of files: the records of its documents, with the path of each file's
//! bytes in the archive, and the files whose bytes it holds (the accounts spec's "What an export
//! holds", FILES-026).

use platform::{
    archive,
    types::{Timestamp, UserId, WorkspaceId},
};
use serde::Serialize;

use super::{FileName, Visibility};

/// A file whose bytes an export's archive holds: its path there, and its object's key, size and
/// CRC-32, which its row gave when the export was built (FILES-026).
#[derive(Clone, Debug)]
pub(crate) struct ExportedFile {
    pub(in crate::modules::files) path: String,
    pub(in crate::modules::files) object_key: String,
    pub(in crate::modules::files) size_bytes: u64,
    pub(in crate::modules::files) crc32: u32,
}

impl ExportedFile {
    /// Its entry in the archive: its bytes, stored as they are, since the kinds that files
    /// accepts are compressed already, or small (the accounts spec's "Exporting").
    pub(crate) fn entry(&self) -> archive::Entry {
        archive::Entry {
            path: self.path.clone(),
            size: self.size_bytes,
            stored_size: self.size_bytes,
            crc32: self.crc32,
            deflated: false,
        }
    }
}

/// A file of the user's own, as `files/files.json` holds it.
#[derive(Debug, Serialize)]
pub(in crate::modules::files) struct OwnRecord {
    pub(in crate::modules::files) id: uuid::Uuid,
    pub(in crate::modules::files) name: FileName,
    pub(in crate::modules::files) media_type: String,
    pub(in crate::modules::files) size_bytes: u64,
    pub(in crate::modules::files) visibility: Visibility,
    pub(in crate::modules::files) created_at: Timestamp,
    pub(in crate::modules::files) path: String,
}

/// The user's avatar, as `files/avatar.json` holds it.
#[derive(Debug, Serialize)]
pub(in crate::modules::files) struct AvatarRecord {
    pub(in crate::modules::files) id: uuid::Uuid,
    pub(in crate::modules::files) media_type: String,
    pub(in crate::modules::files) size_bytes: u64,
    pub(in crate::modules::files) created_at: Timestamp,
    pub(in crate::modules::files) path: String,
}

/// A file of a workspace whose only member is the user, as `files/workspace_files.json` holds it.
#[derive(Debug, Serialize)]
pub(in crate::modules::files) struct WorkspaceRecord {
    pub(in crate::modules::files) id: uuid::Uuid,
    pub(in crate::modules::files) workspace_id: WorkspaceId,
    pub(in crate::modules::files) name: FileName,
    pub(in crate::modules::files) media_type: String,
    pub(in crate::modules::files) size_bytes: u64,
    pub(in crate::modules::files) visibility: Visibility,
    pub(in crate::modules::files) uploader_id: Option<UserId>,
    pub(in crate::modules::files) created_at: Timestamp,
    pub(in crate::modules::files) path: String,
}

/// The picture of a workspace whose only member is the user, as `files/workspace_pictures.json`
/// holds it.
#[derive(Debug, Serialize)]
pub(in crate::modules::files) struct PictureRecord {
    pub(in crate::modules::files) id: uuid::Uuid,
    pub(in crate::modules::files) workspace_id: WorkspaceId,
    pub(in crate::modules::files) media_type: String,
    pub(in crate::modules::files) size_bytes: u64,
    pub(in crate::modules::files) created_at: Timestamp,
    pub(in crate::modules::files) path: String,
}

/// `name` as the last segment of a path in the archive: each character that a path or a common
/// file system refuses in a file's name replaced with `_` (the accounts spec's "What an export
/// holds"). An accepted name ends with its kind's extension (FILES-003), so it is never `.` or
/// `..`, nor ends with a dot or a space, which Windows refuses.
pub(in crate::modules::files) fn path_segment(name: &FileName) -> String {
    name.as_str()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_026_a_name_in_a_path_has_no_character_a_file_system_refuses() {
        let name = FileName::parse("../a\\b:c*d?e\"f<g>h|i/j.pdf").unwrap();
        assert_eq!(path_segment(&name), ".._a_b_c_d_e_f_g_h_i_j.pdf");
        let name = FileName::parse("Résumé 2026.pdf").unwrap();
        assert_eq!(path_segment(&name), "Résumé 2026.pdf");
    }
}
