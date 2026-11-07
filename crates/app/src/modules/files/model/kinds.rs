//! The kinds of file an upload accepts, each with the extensions its names end in, the media type
//! it is stored and served as, and the check of its bytes (FILES-003, the files spec's "Accepted
//! kinds"). A fork adds a kind as one row of [`KINDS`] and its check.

/// A kind of file that an upload accepts.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Kind {
    /// The extensions its names end in, in lowercase, without the dot.
    extensions: &'static [&'static str],
    /// The media type it is stored and served as.
    media_type: &'static str,
    /// Whether a browser may show it in place: only images, which run no script (SEC-11).
    inline: bool,
    /// Whether bytes are of this kind.
    check: fn(&[u8]) -> bool,
}

/// Every kind an upload accepts.
const KINDS: &[Kind] = &[
    Kind {
        extensions: &["png"],
        media_type: "image/png",
        inline: true,
        check: |bytes| bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
    },
    Kind {
        extensions: &["jpg", "jpeg"],
        media_type: "image/jpeg",
        inline: true,
        check: |bytes| bytes.starts_with(b"\xff\xd8\xff"),
    },
    Kind {
        extensions: &["gif"],
        media_type: "image/gif",
        inline: true,
        check: |bytes| bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
    },
    Kind {
        extensions: &["webp"],
        media_type: "image/webp",
        inline: true,
        check: |bytes| bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"),
    },
    Kind {
        extensions: &["pdf"],
        media_type: "application/pdf",
        inline: false,
        check: |bytes| bytes.starts_with(b"%PDF-"),
    },
    Kind {
        extensions: &["docx"],
        media_type: "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        inline: false,
        check: |bytes| office(bytes, "word/document.xml"),
    },
    Kind {
        extensions: &["xlsx"],
        media_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        inline: false,
        check: |bytes| office(bytes, "xl/workbook.xml"),
    },
    Kind {
        extensions: &["pptx"],
        media_type: "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        inline: false,
        check: |bytes| office(bytes, "ppt/presentation.xml"),
    },
    Kind {
        extensions: &["txt"],
        media_type: "text/plain; charset=utf-8",
        inline: false,
        check: text,
    },
    Kind {
        extensions: &["csv"],
        media_type: "text/csv; charset=utf-8",
        inline: false,
        check: text,
    },
    Kind {
        extensions: &["md"],
        media_type: "text/markdown; charset=utf-8",
        inline: false,
        check: text,
    },
];

impl Kind {
    /// The kind whose extensions include that of `name`, ignoring case, if any.
    pub(crate) fn of_name(name: &str) -> Option<Self> {
        let (_, extension) = name.rsplit_once('.')?;
        let extension = extension.to_ascii_lowercase();
        KINDS
            .iter()
            .find(|kind| kind.extensions.contains(&extension.as_str()))
            .copied()
    }

    /// The kind whose media type is `media_type`, as a row stores it, if any.
    pub(crate) fn of_media_type(media_type: &str) -> Option<Self> {
        KINDS
            .iter()
            .find(|kind| kind.media_type == media_type)
            .copied()
    }

    /// Whether `bytes` are of this kind (FILES-003).
    pub(crate) fn holds(self, bytes: &[u8]) -> bool {
        (self.check)(bytes)
    }

    /// The media type it is stored and served as.
    pub(crate) fn media_type(self) -> &'static str {
        self.media_type
    }

    /// Whether a browser may show it in place (SEC-11).
    pub(crate) fn inline(self) -> bool {
        self.inline
    }
}

/// Whether `bytes` are UTF-8 text with no control character but tab, line feed and carriage
/// return.
fn text(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes).is_ok_and(|text| {
        text.chars()
            .all(|c| !c.is_control() || matches!(c, '\t' | '\n' | '\r'))
    })
}

/// The signature of a ZIP archive's local file header, with which the archive begins.
const LOCAL_HEADER: &[u8] = b"PK\x03\x04";

/// The signature of a ZIP archive's end of central directory record.
const END_OF_DIRECTORY: &[u8] = b"PK\x05\x06";

/// The signature of an entry of a ZIP archive's central directory.
const DIRECTORY_ENTRY: &[u8] = b"PK\x01\x02";

/// The length of the end of central directory record, before its comment.
const END_OF_DIRECTORY_LENGTH: usize = 22;

/// The length of a central directory entry, before its name, extra field and comment.
const DIRECTORY_ENTRY_LENGTH: usize = 46;

/// Whether `bytes` are a ZIP archive of an Office Open XML document whose main part is `part`:
/// one that begins with a local file header, and whose central directory names
/// `[Content_Types].xml` and `part`. It reads the names alone, and decompresses nothing.
fn office(bytes: &[u8], part: &str) -> bool {
    if !bytes.starts_with(LOCAL_HEADER) {
        return false;
    }
    let Some(names) = entry_names(bytes) else {
        return false;
    };
    names.contains(&b"[Content_Types].xml".as_slice()) && names.contains(&part.as_bytes())
}

/// The names of the entries of the ZIP archive `bytes`, from its central directory, or `None`
/// when it has none that can be read, as in an archive of the 64-bit format, which an Office
/// document of this size never needs.
fn entry_names(bytes: &[u8]) -> Option<Vec<&[u8]>> {
    // The record ends the archive, but for a comment of at most 65,535 bytes.
    let earliest = bytes.len().checked_sub(END_OF_DIRECTORY_LENGTH)?;
    let latest = earliest.saturating_sub(usize::from(u16::MAX));
    let end = (latest..=earliest)
        .rev()
        .find(|&at| bytes[at..].starts_with(END_OF_DIRECTORY))?;
    let entries = usize::from(u16_at(bytes, end + 10)?);
    let offset = usize::try_from(u32_at(bytes, end + 16)?).ok()?;
    let mut names = Vec::with_capacity(entries);
    let mut at = offset;
    for _ in 0..entries {
        if !bytes.get(at..)?.starts_with(DIRECTORY_ENTRY) {
            return None;
        }
        let name_length = usize::from(u16_at(bytes, at + 28)?);
        let extra_length = usize::from(u16_at(bytes, at + 30)?);
        let comment_length = usize::from(u16_at(bytes, at + 32)?);
        let name_start = at + DIRECTORY_ENTRY_LENGTH;
        names.push(bytes.get(name_start..name_start + name_length)?);
        at = name_start + name_length + extra_length + comment_length;
    }
    Some(names)
}

/// The little-endian 16-bit number at `at`, if `bytes` hold one there.
fn u16_at(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

/// The little-endian 32-bit number at `at`, if `bytes` hold one there.
fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A ZIP archive whose entries are named `names`, each stored empty, with a comment.
    fn archive(names: &[&str]) -> Vec<u8> {
        let mut bytes = Vec::new();
        let mut directory = Vec::new();
        for name in names {
            let offset = u32::try_from(bytes.len()).unwrap();
            let length = u16::try_from(name.len()).unwrap();
            bytes.extend_from_slice(LOCAL_HEADER);
            bytes.extend_from_slice(&[0; 22]);
            bytes.extend_from_slice(&length.to_le_bytes());
            bytes.extend_from_slice(&[0, 0]);
            bytes.extend_from_slice(name.as_bytes());
            directory.extend_from_slice(DIRECTORY_ENTRY);
            directory.extend_from_slice(&[0; 24]);
            directory.extend_from_slice(&length.to_le_bytes());
            directory.extend_from_slice(&[0; 12]);
            directory.extend_from_slice(&offset.to_le_bytes());
            directory.extend_from_slice(name.as_bytes());
        }
        let offset = u32::try_from(bytes.len()).unwrap();
        let size = u32::try_from(directory.len()).unwrap();
        let count = u16::try_from(names.len()).unwrap();
        bytes.extend_from_slice(&directory);
        bytes.extend_from_slice(END_OF_DIRECTORY);
        bytes.extend_from_slice(&[0; 4]);
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.extend_from_slice(&count.to_le_bytes());
        bytes.extend_from_slice(&size.to_le_bytes());
        bytes.extend_from_slice(&offset.to_le_bytes());
        let comment = b"made by a test";
        bytes.extend_from_slice(&u16::try_from(comment.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(comment);
        bytes
    }

    /// Whether a file named `name` with `bytes` is accepted.
    fn accepted(name: &str, bytes: &[u8]) -> bool {
        Kind::of_name(name).is_some_and(|kind| kind.holds(bytes))
    }

    #[test]
    fn files_003_a_kind_is_found_by_its_extension_ignoring_case() {
        assert_eq!(
            Kind::of_name("Photo.PNG").unwrap().media_type(),
            "image/png"
        );
        assert_eq!(
            Kind::of_name("a.b.jpeg").unwrap().media_type(),
            "image/jpeg"
        );
        assert!(Kind::of_name("notes").is_none());
        assert!(Kind::of_name("page.html").is_none());
        assert!(Kind::of_name("drawing.svg").is_none());
        assert!(Kind::of_name("archive.zip").is_none());
        assert!(Kind::of_name("png").is_none());
    }

    #[test]
    fn files_003_images_and_pdfs_are_checked_by_their_signatures() {
        assert!(accepted("a.png", b"\x89PNG\r\n\x1a\nrest"));
        assert!(!accepted("a.png", b"\xff\xd8\xffrest"));
        assert!(accepted("a.jpg", b"\xff\xd8\xff\xe0rest"));
        assert!(accepted("a.gif", b"GIF89a rest"));
        assert!(!accepted("a.gif", b"GIF90a rest"));
        assert!(accepted("a.webp", b"RIFF\x00\x00\x00\x00WEBPVP8 "));
        assert!(!accepted("a.webp", b"RIFF\x00\x00\x00\x00WAVEfmt "));
        assert!(accepted("a.pdf", b"%PDF-1.7\n"));
        assert!(!accepted("a.pdf", b"<html><script>"));
    }

    #[test]
    fn files_003_an_office_document_is_a_zip_archive_naming_its_main_part() {
        let word = archive(&["_rels/.rels", "word/document.xml", "[Content_Types].xml"]);
        assert!(accepted("report.docx", &word));
        assert!(!accepted("report.xlsx", &word));
        assert!(accepted(
            "sheet.xlsx",
            &archive(&["[Content_Types].xml", "xl/workbook.xml"])
        ));
        assert!(accepted(
            "slides.pptx",
            &archive(&["[Content_Types].xml", "ppt/presentation.xml"])
        ));
        // A ZIP archive of something else, and the archive behind other bytes.
        assert!(!accepted("report.docx", &archive(&["word/document.xml"])));
        let mut hidden = b"\x89PNG\r\n\x1a\n".to_vec();
        hidden.extend_from_slice(&word);
        assert!(!accepted("report.docx", &hidden));
        // Truncated archives, and bytes that only start like one.
        assert!(!accepted("report.docx", &word[..word.len() - 30]));
        assert!(!accepted("report.docx", LOCAL_HEADER));
    }

    #[test]
    fn files_003_text_is_utf_8_with_no_control_character_but_tab_and_line_ends() {
        assert!(accepted("notes.txt", "Héllo\tworld\r\n".as_bytes()));
        assert!(accepted("table.csv", b"a,b\n1,2\n"));
        assert!(accepted("readme.md", b"# Title\n"));
        assert!(!accepted("notes.txt", b"\xff\xfe"));
        assert!(!accepted("notes.txt", b"a\x00b"));
        assert!(!accepted("notes.txt", b"a\x1b[31mb"));
    }

    #[test]
    fn files_003_a_stored_media_type_names_its_kind() {
        for kind in KINDS {
            let found = Kind::of_media_type(kind.media_type).unwrap();
            assert_eq!(found.extensions, kind.extensions);
        }
        assert!(Kind::of_media_type("text/html").is_none());
    }
}
