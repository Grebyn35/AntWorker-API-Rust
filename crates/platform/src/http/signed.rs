//! A signed URL in a request's query, which a download route's `Principal` reads in place of a
//! credential (PLATFORM-080). [`crate::signed_urls`] signs and checks one.

use axum::http::request::Parts;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use crate::{
    signed_urls::{EXPIRES, SESSION, SIGNATURE, Signature},
    types::SessionId,
};

/// The marker that a download route's layer puts on its requests, so `Principal` reads a signed
/// URL there and nowhere else (PLATFORM-080).
#[derive(Clone, Copy, Debug)]
pub(crate) struct TakesSignedUrl;

/// What a request's query holds of a signed URL.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Presented {
    /// None of the three parameters.
    None,
    /// The three, as a signed URL gives them, which the signature may still not match.
    Signed(Signature),
    /// Some of the three, or a value that isn't one, which no signed URL gives.
    Malformed,
}

/// What the query of `parts` holds of a signed URL. A parameter given more than once keeps its
/// last value, as every query's does (PLATFORM-045).
pub(crate) fn presented(parts: &Parts) -> Presented {
    let Some(query) = parts.uri.query() else {
        return Presented::None;
    };
    let (mut expires, mut session, mut tag) = (None, None, None);
    for (name, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match name.as_ref() {
            EXPIRES => expires = Some(value.into_owned()),
            SESSION => session = Some(value.into_owned()),
            SIGNATURE => tag = Some(value.into_owned()),
            _ => {}
        }
    }
    match (expires, session, tag) {
        (None, None, None) => Presented::None,
        (Some(expires), Some(session), Some(tag)) => {
            let parsed = expires
                .parse::<i64>()
                .ok()
                .zip(uuid::Uuid::parse_str(&session).ok())
                .zip(URL_SAFE_NO_PAD.decode(tag.as_bytes()).ok());
            match parsed {
                Some(((expires, session), tag)) => Presented::Signed(Signature {
                    expires,
                    session: SessionId::from_uuid(session),
                    tag,
                }),
                None => Presented::Malformed,
            }
        }
        _ => Presented::Malformed,
    }
}

#[cfg(test)]
mod tests {
    use axum::http::Request;

    use super::*;
    use crate::{hashing::HmacKey, signed_urls::SignedUrls, types::Timestamp};

    fn signer(byte: u8) -> SignedUrls {
        SignedUrls::new(HmacKey::new(&[byte; 32]).unwrap())
    }

    fn at(seconds: i64) -> Timestamp {
        Timestamp::from_database(time::OffsetDateTime::from_unix_timestamp(seconds).unwrap())
    }

    fn query_of(url: &str) -> Presented {
        let (parts, ()) = Request::get(url).body(()).unwrap().into_parts();
        presented(&parts)
    }

    fn session() -> SessionId {
        SessionId::from_uuid(uuid::Uuid::from_u128(7))
    }

    /// The signature that `url`'s query holds.
    fn signature_of(url: &str) -> Signature {
        match query_of(url) {
            Presented::Signed(signature) => Some(signature),
            Presented::None | Presented::Malformed => None,
        }
        .expect("the URL's query holds a signature")
    }

    #[test]
    fn platform_080_a_signed_url_names_its_time_its_session_and_its_signature() {
        let signed = signer(1).sign("/v1/a/content", session(), at(1_800_000_000));
        assert!(signed.url.starts_with(
            "/v1/a/content?expires=1800000000&session=00000000-0000-0000-0000-000000000007&signature="
        ));
        assert_eq!(signed.expires_at, at(1_800_000_000));
        let signature = signature_of(&signed.url);
        assert_eq!(signature.session(), session());
        assert_eq!(signature.expires_at(), Some(at(1_800_000_000).at()));
        assert!(signer(1).matches("/v1/a/content", &signature));
    }

    #[test]
    fn platform_080_a_signature_holds_only_for_its_path_its_session_its_time_and_its_key() {
        let signed = signer(1).sign("/v1/a/content", session(), at(1_800_000_000));
        let signature = signature_of(&signed.url);
        assert!(!signer(2).matches("/v1/a/content", &signature));
        assert!(!signer(1).matches("/v1/b/content", &signature));
        let other_time = signed.url.replace("1800000000", "1800000001");
        let changed = signature_of(&other_time);
        assert!(!signer(1).matches("/v1/a/content", &changed));
        let other_session = signed.url.replace("-000000000007", "-000000000008");
        let changed = signature_of(&other_session);
        assert!(!signer(1).matches("/v1/a/content", &changed));
    }

    #[test]
    fn platform_080_a_query_with_only_some_of_the_three_is_malformed() {
        assert_eq!(query_of("/v1/a"), Presented::None);
        assert_eq!(query_of("/v1/a?limit=5"), Presented::None);
        assert_eq!(query_of("/v1/a?expires=1"), Presented::Malformed);
        assert_eq!(
            query_of("/v1/a?expires=1&session=00000000-0000-0000-0000-000000000007"),
            Presented::Malformed
        );
        assert_eq!(
            query_of(
                "/v1/a?expires=soon&session=00000000-0000-0000-0000-000000000007&signature=AA"
            ),
            Presented::Malformed
        );
        assert_eq!(
            query_of("/v1/a?expires=1&session=nobody&signature=AA"),
            Presented::Malformed
        );
        assert_eq!(
            query_of("/v1/a?expires=1&session=00000000-0000-0000-0000-000000000007&signature=%%%"),
            Presented::Malformed
        );
    }
}
