//! An update's precondition: the versions its `If-Match` names (API-13, PLATFORM-071).

use std::convert::Infallible;

use axum::{
    extract::FromRequestParts,
    http::{HeaderMap, header, request::Parts},
};

use crate::types::Version;

/// The `If-Match` of an update: the versions of the resource it may change, or `None` when the
/// request sent no `If-Match`, or one line that holds `*` alone, which any version matches. A tag
/// names the version whose `ETag` it is, character for character, so a weak tag, one the API
/// never sent, such as `"03"`, and a `*` in a list or beside another line name none, and a
/// request that sent only such members names an empty set, which no version matches
/// (PLATFORM-071).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IfMatch(Option<Vec<Version>>);

impl IfMatch {
    /// The versions the update may change, or `None` when it may change any.
    pub fn versions(&self) -> Option<&[Version]> {
        self.0.as_deref()
    }

    fn read(headers: &HeaderMap) -> Self {
        let values: Vec<_> = headers.get_all(header::IF_MATCH).iter().collect();
        match values.as_slice() {
            [] => return Self(None),
            [only] if only.as_bytes().trim_ascii() == b"*" => return Self(None),
            _ => {}
        }
        // A line that holds a byte outside ASCII counts too, read as UTF-8, with the replacement
        // character for what isn't, and only ASCII's white space trimmed: a member that holds
        // such a byte is so no tag the API sent (PLATFORM-092).
        let lines: Vec<_> = values
            .iter()
            .map(|value| String::from_utf8_lossy(value.as_bytes()))
            .collect();
        let versions = lines
            .iter()
            .flat_map(|text| text.split(','))
            .filter_map(|tag| Version::from_etag(tag.trim_ascii()))
            .collect();
        Self(Some(versions))
    }
}

impl<S: Send + Sync> FromRequestParts<S> for IfMatch {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::read(&parts.headers))
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn read(values: &[&'static str]) -> IfMatch {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(header::IF_MATCH, HeaderValue::from_static(value));
        }
        IfMatch::read(&headers)
    }

    fn versions(values: &[i64]) -> Option<Vec<Version>> {
        Some(values.iter().copied().map(Version::from_database).collect())
    }

    #[test]
    fn platform_071_an_etag_quotes_the_version() {
        assert_eq!(Version::from_database(3).etag(), "\"3\"");
        assert_eq!(read(&["\"3\""]).0, versions(&[3]));
    }

    #[test]
    fn platform_071_no_if_match_or_a_star_lets_any_version_through() {
        assert_eq!(read(&[]).versions(), None);
        assert_eq!(read(&["*"]).versions(), None);
    }

    #[test]
    fn platform_071_a_star_in_a_list_or_beside_another_line_names_no_version() {
        assert_eq!(read(&["\"1\", *"]).0, versions(&[1]));
        assert_eq!(read(&["*, \"1\""]).0, versions(&[1]));
        assert_eq!(read(&["\"1\"", "*"]).0, versions(&[1]));
        assert_eq!(read(&["*", "*"]).0, versions(&[]));
        assert_eq!(read(&["*, *"]).0, versions(&[]));
    }

    #[test]
    fn platform_092_the_tags_of_an_if_match_line_that_holds_a_byte_outside_ascii_still_count() {
        let mut headers = HeaderMap::new();
        let line =
            HeaderValue::from_bytes(b"\"5\", \"caf\xc3\xa9\", \xc2\xa0\"6\", \"\xff\"").unwrap();
        headers.append(header::IF_MATCH, line);
        headers.append(header::IF_MATCH, HeaderValue::from_static("\"7\""));

        let versions_named = IfMatch::read(&headers).0;

        assert_eq!(versions_named, versions(&[5, 7]));
    }

    #[test]
    fn platform_071_every_strong_tag_of_every_header_is_named() {
        assert_eq!(read(&["\"1\", \"4\"", "\"7\""]).0, versions(&[1, 4, 7]));
    }

    #[test]
    fn platform_071_a_weak_tag_or_one_the_api_never_sent_names_no_version() {
        for value in [
            "W/\"3\"",
            "\"abc\"",
            "3",
            "\"\"",
            "\"-1\"",
            "\"03\"",
            "\"+3\"",
            "\"99999999999999999999\"",
        ] {
            assert_eq!(read(&[value]).0, versions(&[]), "{value}");
        }
        assert_eq!(read(&["W/\"3\", \"5\""]).0, versions(&[5]));
    }
}
