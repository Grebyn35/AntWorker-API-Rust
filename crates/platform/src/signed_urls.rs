//! Signed URLs: a download's path with the session it acts for, the time it stops working, and an
//! HMAC under `HMAC_KEY` of the three, which a client that can't send a credential with a request,
//! such as an `<img>`, uses in its place (PLATFORM-080, D113). A signed URL proves only its
//! session: the route still authenticates the session, which must last, and authorizes its user
//! as it does any caller. `http` reads one from a request's query.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use crate::{
    hashing::HmacKey,
    types::{SessionId, Timestamp},
};

/// The query parameter of the Unix time, in seconds, at which a signed URL stops working.
pub(crate) const EXPIRES: &str = "expires";

/// The query parameter of the session a signed URL acts for.
pub(crate) const SESSION: &str = "session";

/// The query parameter of a signed URL's signature.
pub(crate) const SIGNATURE: &str = "signature";

/// The kind of a signature's keyed hash, which keeps it apart from every other hash under
/// `HMAC_KEY` (PLATFORM-057).
const KIND: &str = "signed_url";

/// Signs and checks the URLs of downloads, under `HMAC_KEY`.
#[derive(Clone, Debug)]
pub struct SignedUrls {
    key: HmacKey,
}

/// A path signed for a session, until a time.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedUrl {
    /// The path, with the query that signs it.
    pub url: String,
    /// When it stops working: the time it was signed until, to the second.
    pub expires_at: Timestamp,
}

/// The parameters of a signed URL, as a request's query gives them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Signature {
    pub(crate) expires: i64,
    pub(crate) session: SessionId,
    pub(crate) tag: Vec<u8>,
}

impl Signature {
    /// The session the URL acts for.
    pub(crate) fn session(&self) -> SessionId {
        self.session
    }

    /// When the URL stops working, or `None` for a time no date holds.
    pub(crate) fn expires_at(&self) -> Option<time::OffsetDateTime> {
        time::OffsetDateTime::from_unix_timestamp(self.expires).ok()
    }
}

impl SignedUrls {
    /// The signer that signs under `key`.
    pub fn new(key: HmacKey) -> Self {
        Self { key }
    }

    /// `path`, which holds no query, signed for `session` until `until`, a time that the
    /// database's clock gave, to the second (DATA-03).
    pub fn sign(&self, path: &str, session: SessionId, until: Timestamp) -> SignedUrl {
        let expires = until.at().unix_timestamp();
        let tag = self.key.hash(KIND, &message(path, session, expires));
        let at = time::OffsetDateTime::from_unix_timestamp(expires).unwrap_or_else(|_| until.at());
        SignedUrl {
            url: format!(
                "{path}?{EXPIRES}={expires}&{SESSION}={}&{SIGNATURE}={}",
                session.uuid(),
                URL_SAFE_NO_PAD.encode(tag)
            ),
            expires_at: Timestamp::from_database(at),
        }
    }

    /// Whether `signature` is this signer's for `path`, compared in constant time.
    pub(crate) fn matches(&self, path: &str, signature: &Signature) -> bool {
        self.key.verify(
            KIND,
            &message(path, signature.session, signature.expires),
            &signature.tag,
        )
    }
}

/// What a signature covers: the path, the session and the time, one per line, so no two of them
/// read as another's.
fn message(path: &str, session: SessionId, expires: i64) -> Vec<u8> {
    format!("{path}\n{}\n{expires}", session.uuid()).into_bytes()
}
