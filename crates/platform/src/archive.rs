//! The layout of a ZIP file whose entries' bytes it doesn't hold: from each entry's path, sizes and
//! checksum, it gives the archive's size and, for any range of it, the pieces it is made of, the
//! headers and the directory it generates, and ranges of the entries' stored bytes, so a download
//! serves an archive that is never stored whole (PLATFORM-083, D126, the platform spec's
//! "Archives"). It follows APPNOTE, the format's specification, version 6.3.10.

use std::ops::Range;

use bytes::Bytes;
use time::OffsetDateTime;

/// The signature of a local file header.
const LOCAL_HEADER: u32 = 0x0403_4b50;
/// The signature of a central directory's header.
const DIRECTORY_HEADER: u32 = 0x0201_4b50;
/// The signature of the ZIP64 end of central directory record.
const ZIP64_END: u32 = 0x0606_4b50;
/// The signature of the ZIP64 end of central directory locator.
const ZIP64_LOCATOR: u32 = 0x0706_4b50;
/// The signature of the end of central directory record.
const END: u32 = 0x0605_4b50;
/// The ID of the ZIP64 extended information extra field.
const ZIP64_EXTRA: u16 = 0x0001;
/// The flag of a name and a comment in UTF-8.
const UTF_8: u16 = 0x0800;
/// The version needed to extract an entry that needs no ZIP64 field: 2.0, which deflate needs.
const VERSION: u16 = 20;
/// The version needed to extract an entry that needs ZIP64's fields: 4.5.
const VERSION_ZIP64: u16 = 45;
/// The upper byte of "version made by" that names Unix, whose external attributes give a file's
/// mode.
const MADE_ON_UNIX: u16 = 3 << 8;
/// A regular file that its owner may write and anyone may read, as Unix's mode gives it, in the
/// upper half of the external attributes.
const FILE_MODE: u32 = 0o100_644 << 16;
/// The method of a deflated entry.
const DEFLATED: u16 = 8;
/// The method of an entry stored as it is.
const STORED: u16 = 0;
/// The size of a local file header without its name and extra field.
const LOCAL_HEADER_SIZE: u64 = 30;

/// An entry of an archive: what is stored of it, but none of its bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// Its path in the archive, its folders separated by `/`.
    pub path: String,
    /// The size of its bytes.
    pub size: u64,
    /// The size of the bytes it stores: its deflated bytes, or its bytes themselves.
    pub stored_size: u64,
    /// The CRC-32 of its bytes.
    pub crc32: u32,
    /// Whether the bytes it stores are deflated.
    pub deflated: bool,
}

/// A piece of a range of an archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Piece {
    /// Bytes the layout generates: of a header, or of the central directory.
    Bytes(Bytes),
    /// A range of the stored bytes of the entry at `index`, offsets of those bytes.
    Entry {
        /// The entry's index among those the layout was given.
        index: usize,
        /// The offsets of its stored bytes that the range covers.
        offsets: Range<u64>,
    },
}

/// The values past which the format's own fields hold no value, and ZIP64's stand.
#[derive(Clone, Copy, Debug)]
struct Limits {
    /// The most a size or an offset's own field holds, which is the value that marks ZIP64's.
    field: u64,
    /// The most entries the end record's own field counts, which is the value that marks ZIP64's.
    count: u64,
}

impl Limits {
    /// The format's.
    const FORMAT: Self = Self {
        field: 0xFFFF_FFFF,
        count: 0xFFFF,
    };
}

/// The CRC-32 of `bytes`, as an archive's entry gives it, which a caller computes on a turn of
/// the workers for bytes as long as an upload's (CODE-05).
pub fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

/// The layout of an archive of its entries, in their order, at a time.
#[derive(Clone, Debug)]
pub struct Layout {
    entries: Vec<Entry>,
    /// The offset of each entry's local header.
    offsets: Vec<u64>,
    /// The central directory, and the records that end the archive.
    directory: Bytes,
    /// The offset of the central directory.
    directory_at: u64,
    /// The time of every entry, in MS-DOS's format: its time, then its date.
    time: (u16, u16),
    limits: Limits,
}

impl Layout {
    /// The layout of an archive of `entries`, in their order, each modified at `time`.
    pub fn new(entries: Vec<Entry>, time: OffsetDateTime) -> Self {
        Self::with_limits(entries, time, Limits::FORMAT)
    }

    fn with_limits(entries: Vec<Entry>, time: OffsetDateTime, limits: Limits) -> Self {
        let time = dos_time(time);
        let mut offsets = Vec::with_capacity(entries.len());
        let mut at = 0;
        for entry in &entries {
            offsets.push(at);
            at += local_header_size(entry, limits) + entry.stored_size;
        }
        let mut layout = Self {
            entries,
            offsets,
            directory: Bytes::new(),
            directory_at: at,
            time,
            limits,
        };
        layout.directory = layout.directory();
        layout
    }

    /// The archive's size, in bytes.
    pub fn size(&self) -> u64 {
        self.directory_at + self.directory.len() as u64
    }

    /// The pieces of `range`, offsets of the archive, in their order: joined, with the stored
    /// bytes of each entry's range that a piece names in its place, they are the archive's bytes
    /// in `range`.
    pub fn pieces(&self, range: Range<u64>) -> Vec<Piece> {
        let mut pieces = Vec::new();
        let end = range.end.min(self.size());
        if range.start >= end {
            return pieces;
        }
        let first = self
            .offsets
            .partition_point(|offset| *offset <= range.start)
            .saturating_sub(1);
        for (index, entry) in self.entries.iter().enumerate().skip(first) {
            let header_at = self.offsets[index];
            if header_at >= end {
                break;
            }
            let header = self.local_header(entry);
            let bytes_at = header_at + header.len() as u64;
            if let Some(part) = overlap(header_at..bytes_at, range.start..end) {
                pieces.push(Piece::Bytes(header.slice(to_usize(part))));
            }
            if let Some(part) = overlap(bytes_at..bytes_at + entry.stored_size, range.start..end) {
                pieces.push(Piece::Entry {
                    index,
                    offsets: part,
                });
            }
        }
        if let Some(part) = overlap(self.directory_at..self.size(), range.start..end) {
            pieces.push(Piece::Bytes(self.directory.slice(to_usize(part))));
        }
        pieces
    }

    /// The local header of `entry`: with ZIP64's extra field, which gives both sizes, when either
    /// passes its own field, as the format asks of a local header.
    fn local_header(&self, entry: &Entry) -> Bytes {
        let zip64 = entry.size >= self.limits.field || entry.stored_size >= self.limits.field;
        let mut header = Vec::new();
        put_u32(&mut header, LOCAL_HEADER);
        put_u16(&mut header, if zip64 { VERSION_ZIP64 } else { VERSION });
        self.common(&mut header, entry);
        if zip64 {
            put_u32(&mut header, 0xFFFF_FFFF);
            put_u32(&mut header, 0xFFFF_FFFF);
        } else {
            put_u32(&mut header, low(entry.stored_size));
            put_u32(&mut header, low(entry.size));
        }
        put_u16(&mut header, short(entry.path.len()));
        put_u16(&mut header, if zip64 { 20 } else { 0 });
        header.extend_from_slice(entry.path.as_bytes());
        if zip64 {
            put_u16(&mut header, ZIP64_EXTRA);
            put_u16(&mut header, 16);
            put_u64(&mut header, entry.size);
            put_u64(&mut header, entry.stored_size);
        }
        Bytes::from(header)
    }

    /// The fields that a local header and a directory's header share, from the flags to the
    /// checksum.
    fn common(&self, header: &mut Vec<u8>, entry: &Entry) {
        put_u16(header, UTF_8);
        put_u16(header, if entry.deflated { DEFLATED } else { STORED });
        put_u16(header, self.time.0);
        put_u16(header, self.time.1);
        put_u32(header, entry.crc32);
    }

    /// The central directory, with ZIP64's end records when a field of the end record can't
    /// hold its value, then the end record.
    fn directory(&self) -> Bytes {
        let mut directory = Vec::new();
        for (entry, offset) in self.entries.iter().zip(&self.offsets) {
            self.directory_header(&mut directory, entry, *offset);
        }
        let count = self.entries.len() as u64;
        let size = directory.len() as u64;
        let at = self.directory_at;
        let limits = self.limits;
        if count >= limits.count || size >= limits.field || at >= limits.field {
            let record_at = at + size;
            put_u32(&mut directory, ZIP64_END);
            put_u64(&mut directory, 44);
            put_u16(&mut directory, MADE_ON_UNIX | VERSION_ZIP64);
            put_u16(&mut directory, VERSION_ZIP64);
            put_u32(&mut directory, 0);
            put_u32(&mut directory, 0);
            put_u64(&mut directory, count);
            put_u64(&mut directory, count);
            put_u64(&mut directory, size);
            put_u64(&mut directory, at);
            put_u32(&mut directory, ZIP64_LOCATOR);
            put_u32(&mut directory, 0);
            put_u64(&mut directory, record_at);
            put_u32(&mut directory, 1);
        }
        let count = if count >= limits.count {
            0xFFFF
        } else {
            short(to_usize(0..count).end)
        };
        put_u32(&mut directory, END);
        put_u16(&mut directory, 0);
        put_u16(&mut directory, 0);
        put_u16(&mut directory, count);
        put_u16(&mut directory, count);
        put_u32(&mut directory, self.field(size));
        put_u32(&mut directory, self.field(at));
        put_u16(&mut directory, 0);
        Bytes::from(directory)
    }

    /// Appends the directory's header of `entry`, whose local header is at `offset`: with ZIP64's
    /// extra field, which gives, in the format's order, each of its size, stored size and offset
    /// that passes its own field, and those alone.
    fn directory_header(&self, directory: &mut Vec<u8>, entry: &Entry, offset: u64) {
        let large: Vec<u64> = [entry.size, entry.stored_size, offset]
            .into_iter()
            .filter(|value| *value >= self.limits.field)
            .collect();
        let version = if large.is_empty() {
            VERSION
        } else {
            VERSION_ZIP64
        };
        put_u32(directory, DIRECTORY_HEADER);
        put_u16(directory, MADE_ON_UNIX | version);
        put_u16(directory, version);
        self.common(directory, entry);
        put_u32(directory, self.field(entry.stored_size));
        put_u32(directory, self.field(entry.size));
        put_u16(directory, short(entry.path.len()));
        put_u16(
            directory,
            if large.is_empty() {
                0
            } else {
                short(4 + 8 * large.len())
            },
        );
        put_u16(directory, 0);
        put_u16(directory, 0);
        put_u16(directory, 0);
        put_u32(directory, FILE_MODE);
        put_u32(directory, self.field(offset));
        directory.extend_from_slice(entry.path.as_bytes());
        if !large.is_empty() {
            put_u16(directory, ZIP64_EXTRA);
            put_u16(directory, short(8 * large.len()));
            for value in large {
                put_u64(directory, value);
            }
        }
    }

    /// `value` in a field of four bytes: itself, or `0xFFFFFFFF` once it passes the field, for
    /// ZIP64's to give.
    fn field(&self, value: u64) -> u32 {
        if value >= self.limits.field {
            0xFFFF_FFFF
        } else {
            low(value)
        }
    }
}

/// The size of the local header of `entry`, under `limits`.
fn local_header_size(entry: &Entry, limits: Limits) -> u64 {
    let zip64 = entry.size >= limits.field || entry.stored_size >= limits.field;
    LOCAL_HEADER_SIZE + entry.path.len() as u64 + if zip64 { 20 } else { 0 }
}

/// The offsets of `range` that `within` covers, relative to `range`'s start, if any.
fn overlap(range: Range<u64>, within: Range<u64>) -> Option<Range<u64>> {
    let start = range.start.max(within.start);
    let end = range.end.min(within.end);
    (start < end).then(|| start - range.start..end - range.start)
}

/// `range` as offsets of bytes held in memory, which a header's or the directory's always are.
fn to_usize(range: Range<u64>) -> Range<usize> {
    let convert = |value: u64| usize::try_from(value).unwrap_or(usize::MAX);
    convert(range.start)..convert(range.end)
}

/// The low four bytes of `value`, which its field holds once the layout has found that it fits.
fn low(value: u64) -> u32 {
    u32::try_from(value & 0xFFFF_FFFF).unwrap_or(u32::MAX)
}

/// `value` in a field of two bytes: a name's length, which an entry's path keeps short, or a
/// count that fits.
fn short(value: usize) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

/// `time` in MS-DOS's format, in UTC, to the even second: its time, then its date. A time before
/// 1980, which the format can't hold, is 1980's first moment.
fn dos_time(time: OffsetDateTime) -> (u16, u16) {
    let time = time.to_offset(time::UtcOffset::UTC);
    if time.year() < 1980 {
        return (0, (1 << 5) | 1);
    }
    let (hour, minute, second) = time.to_hms();
    let year = u16::try_from(time.year() - 1980).unwrap_or(127).min(127);
    let clock = (u16::from(hour) << 11) | (u16::from(minute) << 5) | u16::from(second / 2);
    let date = (year << 9) | (u16::from(u8::from(time.month())) << 5) | u16::from(time.day());
    (clock, date)
}

fn put_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Write};

    use flate2::{Compression, write::DeflateEncoder};

    use super::*;

    /// An entry of `path`, whose bytes are `bytes`, deflated or stored as they are, with what it
    /// stores.
    fn entry(path: &str, bytes: &[u8], deflated: bool) -> (Entry, Vec<u8>) {
        let stored = if deflated {
            let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(bytes).unwrap();
            encoder.finish().unwrap()
        } else {
            bytes.to_vec()
        };
        let entry = Entry {
            path: path.to_owned(),
            size: bytes.len() as u64,
            stored_size: stored.len() as u64,
            crc32: crc32fast::hash(bytes),
            deflated,
        };
        (entry, stored)
    }

    /// The bytes of `range` of the archive that `layout` lays out, with `stored` the stored
    /// bytes of each entry.
    fn bytes(layout: &Layout, stored: &[Vec<u8>], range: Range<u64>) -> Vec<u8> {
        let mut bytes = Vec::new();
        for piece in layout.pieces(range) {
            match piece {
                Piece::Bytes(generated) => bytes.extend_from_slice(&generated),
                Piece::Entry { index, offsets } => {
                    bytes.extend_from_slice(&stored[index][to_usize(offsets)]);
                }
            }
        }
        bytes
    }

    /// Three entries, of which one deflated and one empty, a name outside ASCII among them.
    fn three() -> (Vec<Entry>, Vec<Vec<u8>>, Vec<Vec<u8>>) {
        let contents: Vec<Vec<u8>> = vec![
            b"[\n{\"id\":1},\n{\"id\":2}\n]\n".repeat(50),
            vec![0x89, b'P', b'N', b'G', 1, 2, 3, 4, 5],
            Vec::new(),
        ];
        let paths = [
            "accounts/profile.json",
            "files/own/1/Résumé 1.png",
            "empty.json",
        ];
        let (entries, stored) = paths
            .iter()
            .zip(&contents)
            .enumerate()
            .map(|(index, (path, bytes))| entry(path, bytes, index != 1))
            .unzip();
        (entries, stored, contents)
    }

    fn at() -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(1_794_300_005).unwrap()
    }

    /// Reads every entry of `archive` back, as `zip` reads it, by its name.
    fn read_back(archive: Vec<u8>) -> Vec<(String, Vec<u8>, u32)> {
        let mut zip = zip::ZipArchive::new(Cursor::new(archive)).unwrap();
        (0..zip.len())
            .map(|index| {
                let mut file = zip.by_index(index).unwrap();
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes).unwrap();
                (file.name().unwrap().into_owned(), bytes, file.crc32())
            })
            .collect()
    }

    #[test]
    fn platform_083_a_zip_reader_reads_every_entry_back() {
        let (entries, stored, contents) = three();
        let layout = Layout::new(entries.clone(), at());
        let archive = bytes(&layout, &stored, 0..layout.size());
        assert_eq!(archive.len() as u64, layout.size());
        let read = read_back(archive);
        assert_eq!(read.len(), 3);
        for ((name, bytes, crc32), (entry, content)) in
            read.iter().zip(entries.iter().zip(&contents))
        {
            assert_eq!(name, &entry.path);
            assert_eq!(bytes, content);
            assert_eq!(*crc32, entry.crc32);
        }
    }

    #[test]
    fn platform_083_every_range_is_those_bytes_of_the_whole() {
        let (entries, stored, _) = three();
        let layout = Layout::new(entries, at());
        let whole = bytes(&layout, &stored, 0..layout.size());
        let size = layout.size();
        for start in (0..size).step_by(7) {
            for length in [1, 2, 29, 31, 64, 300, size] {
                let end = (start + length).min(size);
                assert_eq!(
                    bytes(&layout, &stored, start..end),
                    whole[to_usize(start..end)],
                    "{start}..{end}"
                );
            }
        }
        assert!(layout.pieces(size..size + 10).is_empty());
    }

    #[test]
    fn platform_083_the_same_entries_at_the_same_time_give_the_same_bytes() {
        let (entries, stored, _) = three();
        let first = Layout::new(entries.clone(), at());
        let second = Layout::new(entries, at());
        assert_eq!(
            bytes(&first, &stored, 0..first.size()),
            bytes(&second, &stored, 0..second.size())
        );
    }

    #[test]
    fn platform_083_names_are_flagged_as_utf_8_at_the_time_given_in_utc() {
        let (entries, stored, _) = three();
        let layout = Layout::new(entries, at());
        let archive = bytes(&layout, &stored, 0..layout.size());
        // The flags of the first local header.
        assert_eq!(u16::from_le_bytes([archive[6], archive[7]]), UTF_8);
        let mut zip = zip::ZipArchive::new(Cursor::new(archive)).unwrap();
        let file = zip.by_index(1).unwrap();
        assert_eq!(file.name().unwrap(), "files/own/1/Résumé 1.png");
        let modified = file.last_modified().unwrap();
        // The time given, in UTC, which the format holds to the even second.
        let given = at();
        assert_eq!(given.second() % 2, 1);
        assert_eq!(
            (
                modified.year(),
                modified.month(),
                modified.day(),
                modified.hour(),
                modified.minute(),
                modified.second()
            ),
            (
                u16::try_from(given.year()).unwrap(),
                u8::from(given.month()),
                given.day(),
                given.hour(),
                given.minute(),
                given.second() - 1
            )
        );
        assert_eq!(file.unix_mode(), Some(0o100_644));
    }

    #[test]
    fn platform_083_zip64_s_records_stand_where_the_format_s_fields_pass() {
        // Limits small enough that the three entries' sizes, offsets and count pass them, as an
        // archive of more than 4 GiB, or of more than 65,534 entries, would pass the format's.
        let (entries, stored, contents) = three();
        let limits = Limits {
            field: 100,
            count: 2,
        };
        let layout = Layout::with_limits(entries.clone(), at(), limits);
        let archive = bytes(&layout, &stored, 0..layout.size());
        let read = read_back(archive.clone());
        assert_eq!(
            read.iter()
                .map(|(_, bytes, _)| bytes.clone())
                .collect::<Vec<_>>(),
            contents
        );
        let signature = ZIP64_END.to_le_bytes();
        assert!(archive.windows(4).any(|window| window == signature));
        // Every range still joins into the whole.
        for start in (0..layout.size()).step_by(11) {
            let end = (start + 50).min(layout.size());
            assert_eq!(
                bytes(&layout, &stored, start..end),
                archive[to_usize(start..end)]
            );
        }
        // Under the format's limits, no ZIP64 record stands.
        let plain = Layout::new(entries, at());
        let plain = bytes(&plain, &stored, 0..plain.size());
        assert!(!plain.windows(4).any(|window| window == signature));
    }
}
