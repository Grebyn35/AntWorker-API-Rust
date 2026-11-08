//! What a read of a stored file asks of its bytes, and what it serves of them: every byte, one
//! range of them, or none, as the conditions of RFC 9110 decide (PLATFORM-081, D112). A
//! download's handler reads one from its request, and the service that reads the file decides,
//! once it knows the file's size, so the service names no HTTP type (ARCH-09).

/// What a read of a stored file's bytes asks for: whether its request is a `GET`, and its
/// `Range`, `If-Range` and `If-None-Match`, as RFC 9110 defines them, which [`Wanted::serve`]
/// reads once the file's size and entity tag are known (PLATFORM-081). `http` reads one from a
/// download's request.
#[derive(Clone, Debug, Default)]
pub struct Wanted {
    get: bool,
    range: Option<String>,
    if_range: Option<String>,
    if_none_match: Option<String>,
}

/// The bytes of a file, from its first to its last, both included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ByteRange {
    /// The offset of its first byte.
    pub first: u64,
    /// The offset of its last byte.
    pub last: u64,
}

impl ByteRange {
    /// The offsets it covers, as a read of the bucket takes them.
    pub fn offsets(self) -> std::ops::Range<u64> {
        self.first..self.last + 1
    }

    /// How many bytes it covers.
    pub(crate) fn len(self) -> u64 {
        self.last - self.first + 1
    }
}

/// What a download serves of a file's bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Served {
    /// Nothing: the client holds the bytes its `If-None-Match` names (304).
    NotModified,
    /// Every byte (200).
    Whole,
    /// One range of them (206).
    Range(ByteRange),
}

impl Served {
    /// The range that a read of the bucket asks for: `None` for every byte.
    pub fn offsets(self) -> Option<std::ops::Range<u64>> {
        match self {
            Self::Range(range) => Some(range.offsets()),
            Self::NotModified | Self::Whole => None,
        }
    }
}

impl Wanted {
    /// What a request asks for: `get` when it is a `GET`, with the text of its `Range`,
    /// `If-Range` and `If-None-Match`, if it carries them.
    pub(crate) fn new(
        get: bool,
        range: Option<String>,
        if_range: Option<String>,
        if_none_match: Option<String>,
    ) -> Self {
        Self {
            get,
            range,
            if_range,
            if_none_match,
        }
    }

    /// What to serve of a file of `size` bytes whose entity tag is `tag`, as RFC 9110 orders
    /// the conditions: an `If-None-Match` that is `*`, or names `tag`, weakly or strongly, serves
    /// nothing; then a `Range` of `GET` that holds one range starting within the file serves it,
    /// unless an `If-Range` names something other than `tag`, strongly; and every other request
    /// serves every byte, a `Range` that can't be served being ignored, as RFC 9110 allows.
    pub fn serve(&self, size: u64, tag: &str) -> Served {
        if self
            .if_none_match
            .as_deref()
            .is_some_and(|value| names_tag(value, tag))
        {
            return Served::NotModified;
        }
        if !self.get {
            return Served::Whole;
        }
        let Some(range) = self
            .range
            .as_deref()
            .and_then(|value| one_range(value, size))
        else {
            return Served::Whole;
        };
        match self.if_range.as_deref() {
            Some(value) if value.trim() != quoted(tag) => Served::Whole,
            _ => Served::Range(range),
        }
    }
}

/// `tag` as an entity tag: quoted, and strong.
pub(crate) fn quoted(tag: &str) -> String {
    format!("\"{tag}\"")
}

/// Whether `value`, an `If-None-Match`, is `*` or names `tag` among its entity tags, by the weak
/// comparison it takes.
fn names_tag(value: &str, tag: &str) -> bool {
    let value = value.trim();
    value == "*"
        || value.split(',').any(|named| {
            let named = named.trim();
            named.strip_prefix("W/").unwrap_or(named) == quoted(tag)
        })
}

/// The one range of bytes that `value`, a `Range`, asks for of a file of `size` bytes:
/// `bytes=first-last`, its last clamped to the file's, `bytes=first-`, or `bytes=-suffix`, the
/// file's last `suffix` bytes, or all of them for a longer suffix. `None` for a value of several
/// ranges, of another unit, that isn't one, or whose range starts past the file's last byte.
fn one_range(value: &str, size: u64) -> Option<ByteRange> {
    let (unit, set) = value.trim().split_once('=')?;
    if !unit.trim().eq_ignore_ascii_case("bytes") || set.contains(',') || size == 0 {
        return None;
    }
    let (first, last) = set.trim().split_once('-')?;
    let number = |text: &str| -> Option<u64> {
        let text = text.trim();
        (!text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| text.parse().ok())
            .flatten()
    };
    let end = size - 1;
    if first.trim().is_empty() {
        let suffix = number(last).filter(|suffix| *suffix > 0)?;
        return Some(ByteRange {
            first: size.saturating_sub(suffix),
            last: end,
        });
    }
    let first = number(first).filter(|first| *first <= end)?;
    let last = if last.trim().is_empty() {
        end
    } else {
        number(last).filter(|last| *last >= first)?.min(end)
    };
    Some(ByteRange { first, last })
}

#[cfg(test)]
mod tests {
    use axum::http::{HeaderName, Method, header};

    use super::*;

    /// A request of `method` with the headers `headers`, as a download reads it.
    fn request(method: Method, headers: &[(HeaderName, &str)]) -> Wanted {
        let text = |name: &HeaderName| {
            headers
                .iter()
                .find(|(named, _)| named == name)
                .map(|(_, value)| (*value).to_owned())
        };
        Wanted::new(
            method == Method::GET,
            text(&header::RANGE),
            text(&header::IF_RANGE),
            text(&header::IF_NONE_MATCH),
        )
    }

    fn range(value: &str) -> Served {
        request(Method::GET, &[(header::RANGE, value)]).serve(1_000, "tag")
    }

    fn bytes(first: u64, last: u64) -> Served {
        Served::Range(ByteRange { first, last })
    }

    #[test]
    fn platform_081_one_range_of_bytes_is_served_in_each_of_its_forms() {
        assert_eq!(range("bytes=0-499"), bytes(0, 499));
        assert_eq!(range("bytes=500-"), bytes(500, 999));
        assert_eq!(range("bytes=-200"), bytes(800, 999));
        assert_eq!(range(" Bytes = 10 - 20 "), bytes(10, 20));
        // A last byte past the file's ends at its last, and a suffix longer than it is the file.
        assert_eq!(range("bytes=990-5000"), bytes(990, 999));
        assert_eq!(range("bytes=-5000"), bytes(0, 999));
        assert_eq!(range("bytes=999-999"), bytes(999, 999));
    }

    #[test]
    fn platform_081_a_range_that_can_t_be_served_serves_every_byte() {
        for value in [
            "bytes=1000-",
            "bytes=2000-3000",
            "bytes=-0",
            "bytes=20-10",
            "bytes=0-10, 20-30",
            "items=0-10",
            "bytes=a-b",
            "bytes=+1-2",
            "bytes",
            "bytes=-",
        ] {
            assert_eq!(range(value), Served::Whole, "{value}");
        }
        assert_eq!(
            request(Method::GET, &[(header::RANGE, "bytes=0-0")]).serve(0, "tag"),
            Served::Whole
        );
    }

    #[test]
    fn platform_081_if_range_serves_the_range_only_for_the_same_strong_tag() {
        let with = |if_range: &str| {
            request(
                Method::GET,
                &[(header::RANGE, "bytes=10-"), (header::IF_RANGE, if_range)],
            )
            .serve(1_000, "tag")
        };
        assert_eq!(with("\"tag\""), bytes(10, 999));
        assert_eq!(with("\"other\""), Served::Whole);
        assert_eq!(with("W/\"tag\""), Served::Whole);
        assert_eq!(with("Sat, 07 Nov 2026 10:00:00 GMT"), Served::Whole);
    }

    #[test]
    fn platform_081_if_none_match_of_the_tag_serves_nothing_and_head_ignores_range() {
        let none_match = |value: &str| {
            request(
                Method::GET,
                &[(header::IF_NONE_MATCH, value), (header::RANGE, "bytes=0-1")],
            )
            .serve(1_000, "tag")
        };
        assert_eq!(none_match("\"tag\""), Served::NotModified);
        assert_eq!(none_match("W/\"tag\""), Served::NotModified);
        assert_eq!(none_match("\"a\", \"tag\""), Served::NotModified);
        assert_eq!(none_match("*"), Served::NotModified);
        assert_eq!(none_match("\"other\""), bytes(0, 1));
        assert_eq!(
            request(Method::HEAD, &[(header::RANGE, "bytes=0-1")]).serve(1_000, "tag"),
            Served::Whole
        );
    }
}
