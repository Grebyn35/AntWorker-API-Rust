//! A release's version, as semantic versioning orders it, and the versions that can follow it.

use std::{cmp::Ordering, fmt, str::FromStr};

/// A release's version: `X.Y.Z`, or `X.Y.Z-rc.N` for a prerelease, as a tag `vX.Y.Z` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    /// The prerelease's number, for `-rc.N`.
    rc: Option<u64>,
}

impl Version {
    /// The next patch, minor and major versions after `self`. After a prerelease, the next patch
    /// is the release the prerelease led to.
    pub fn next(self) -> [Self; 3] {
        let final_ = |major, minor, patch| Self {
            major,
            minor,
            patch,
            rc: None,
        };
        let patch = match self.rc {
            Some(_) => final_(self.major, self.minor, self.patch),
            None => final_(self.major, self.minor, self.patch + 1),
        };
        [
            patch,
            final_(self.major, self.minor + 1, 0),
            final_(self.major + 1, 0, 0),
        ]
    }

    /// The next prerelease of the same version, after a prerelease, such as 1.4.0-rc.3 after
    /// 1.4.0-rc.2.
    pub fn next_prerelease(self) -> Option<Self> {
        self.rc.map(|rc| Self {
            rc: Some(rc + 1),
            ..self
        })
    }
}

impl FromStr for Version {
    type Err = ();

    /// Reads `X.Y.Z` or `X.Y.Z-rc.N`, with an optional leading `v`, and no leading zeros.
    fn from_str(text: &str) -> Result<Self, ()> {
        let text = text.strip_prefix('v').unwrap_or(text);
        let (core, rc) = match text.split_once('-') {
            Some((core, pre)) => (core, Some(number(pre.strip_prefix("rc.").ok_or(())?)?)),
            None => (text, None),
        };
        let parts: Vec<&str> = core.split('.').collect();
        let [major, minor, patch] = parts.as_slice() else {
            return Err(());
        };
        Ok(Self {
            major: number(major)?,
            minor: number(minor)?,
            patch: number(patch)?,
            rc,
        })
    }
}

fn number(text: &str) -> Result<u64, ()> {
    let leading_zero = text.len() > 1 && text.starts_with('0');
    if text.is_empty() || leading_zero || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(());
    }
    text.parse().map_err(|_| ())
}

impl Ord for Version {
    /// Semantic versioning's order: a prerelease comes before the release it leads to.
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then(match (self.rc, other.rc) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => a.cmp(&b),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        match self.rc {
            Some(rc) => write!(f, "-rc.{rc}"),
            None => Ok(()),
        }
    }
}
