//! Lists: a page's `limit` and `cursor` (API-06, PLATFORM-059), a list's `search` (API-20,
//! PLATFORM-060), and the body of a page of items (API-05), which a route reads from the
//! parameters of its `http::Query` and hands its service.

use std::borrow::Cow;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{ObjectBuilder, RefOr, Schema, Type},
};

use crate::{
    http::{FieldErrors, query::parameter_pointer},
    types::{Invalid, Timestamp},
};

/// How many items a page holds when the request names no `limit` (API-06).
const DEFAULT_LIMIT: u16 = 50;
/// The most items a page holds (API-06).
const MAX_LIMIT: u16 = 100;
/// The most characters a search holds (PLATFORM-060).
const SEARCH_MAX_CHARS: usize = 100;
/// The bytes of a cursor: the tag of its route, the microseconds of its sort key, and its ID.
const CURSOR_LEN: usize = 4 + 8 + 16;

/// Where a page starts, after the item that ended the last one: its sort key and its ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    /// The item's sort key.
    pub at: time::OffsetDateTime,
    /// The item's ID, which breaks a tie between items with the same key.
    pub id: uuid::Uuid,
}

/// A page of a list: how many items it holds, and the cursor it starts after (API-06).
#[derive(Clone, Debug)]
pub struct Page {
    limit: u16,
    after: Option<Cursor>,
    tag: [u8; 4],
}

impl Page {
    /// The page of `route` that `limit` and `cursor`, the query's parameters, name, or `None`,
    /// recording why, when `errors` refuses either (PLATFORM-059).
    pub fn read(
        route: &str,
        limit: Option<&str>,
        cursor: Option<&str>,
        errors: &mut FieldErrors,
    ) -> Option<Self> {
        let tag = route_tag(route);
        // Both are read before either refusal returns, so one response names them both.
        let limit = parameter(errors, "limit", limit, parse_limit);
        let after = parameter(errors, "cursor", cursor, |value| decode(value, tag));
        let (limit, after) = limit.zip(after)?;
        Some(Self {
            limit: limit.unwrap_or(DEFAULT_LIMIT),
            after,
            tag,
        })
    }

    /// The rows a query reads for the page: one more than it holds, which tells whether
    /// another page follows.
    pub fn fetch(&self) -> i64 {
        i64::from(self.limit) + 1
    }

    /// The sort key the page starts after, for a query's parameter.
    pub fn after_at(&self) -> Option<time::OffsetDateTime> {
        self.after.map(|cursor| cursor.at)
    }

    /// The ID the page starts after, for a query's parameter.
    pub fn after_id(&self) -> Option<uuid::Uuid> {
        self.after.map(|cursor| cursor.id)
    }

    /// The page of `rows`, which a query read with [`Page::fetch`], and the cursor of the next
    /// one when more rows follow, from the last item's sort key and ID.
    pub fn list<T>(&self, mut rows: Vec<T>, key: impl Fn(&T) -> Cursor) -> List<T> {
        let more = rows.len() > usize::from(self.limit);
        rows.truncate(usize::from(self.limit));
        let next_cursor = more
            .then(|| rows.last().map(|last| encode(key(last), self.tag)))
            .flatten();
        List {
            items: rows,
            next_cursor,
        }
    }
}

/// The query parameter `name`'s `value` through `parse`, if it was sent, or `None`, recording
/// why, when `errors` refuses it.
pub fn parameter<T>(
    errors: &mut FieldErrors,
    name: &str,
    value: Option<&str>,
    parse: impl FnOnce(&str) -> Result<T, Invalid>,
) -> Option<Option<T>> {
    match value {
        None => Some(None),
        Some(value) => errors
            .check(&parameter_pointer(name), parse(value))
            .map(Some),
    }
}

fn parse_limit(value: &str) -> Result<u16, Invalid> {
    match value.parse::<u16>() {
        Ok(limit) if (1..=MAX_LIMIT).contains(&limit) => Ok(limit),
        Ok(_) => Err(Invalid::new("out_of_range", "A limit is 1 to 100.")),
        Err(_) => Err(Invalid::new("invalid", "A limit is a whole number.")),
    }
}

/// The first bytes of the route's digest, so a cursor works only on the route that answered it.
fn route_tag(route: &str) -> [u8; 4] {
    let digest = Sha256::digest(route.as_bytes());
    [digest[0], digest[1], digest[2], digest[3]]
}

fn encode(cursor: Cursor, tag: [u8; 4]) -> String {
    let micros = i64::try_from(cursor.at.unix_timestamp_nanos() / 1_000).unwrap_or(i64::MAX);
    let mut bytes = Vec::with_capacity(CURSOR_LEN);
    bytes.extend_from_slice(&tag);
    bytes.extend_from_slice(&micros.to_be_bytes());
    bytes.extend_from_slice(cursor.id.as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}

fn decode(value: &str, tag: [u8; 4]) -> Result<Cursor, Invalid> {
    let invalid = || Invalid::new("invalid", "Send a cursor that this list answered.");
    let bytes = URL_SAFE_NO_PAD.decode(value).map_err(|_| invalid())?;
    let bytes: [u8; CURSOR_LEN] = bytes.try_into().map_err(|_| invalid())?;
    let (tagged, rest) = bytes.split_at(4);
    let (micros, id) = rest.split_at(8);
    if tagged != tag {
        return Err(invalid());
    }
    let micros = i64::from_be_bytes(micros.try_into().map_err(|_| invalid())?);
    let at = time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(micros) * 1_000)
        .map_err(|_| invalid())?;
    let id = uuid::Uuid::from_slice(id).map_err(|_| invalid())?;
    Ok(Cursor { at, id })
}

/// What a list's items must contain, in one of the fields its route names, ignoring case:
/// trimmed, with each run of whitespace inside collapsed to one space, in Unicode Normalization
/// Form C, of 1 to 100 characters (PLATFORM-060).
#[derive(Clone, PartialEq, Eq)]
pub struct Search(String);

impl std::fmt::Debug for Search {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // A search can hold a name or an email address (OBS-04).
        f.write_str("Search([redacted])")
    }
}

impl Search {
    /// The search in `value`, the query's `search`, if any, or `None`, recording why, when
    /// `errors` refuses it.
    pub fn read(value: Option<&str>, errors: &mut FieldErrors) -> Option<Option<Self>> {
        parameter(errors, "search", value, Self::parse)
    }

    /// The search in `value`, normalized, or why it isn't one.
    pub fn parse(value: &str) -> Result<Self, Invalid> {
        let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
        let search: String = collapsed.nfc().collect();
        if search.is_empty() {
            return Err(Invalid::new("required", "Enter something to search for."));
        }
        if search.chars().count() > SEARCH_MAX_CHARS {
            return Err(Invalid::new(
                "too_long",
                "A search has at most 100 characters.",
            ));
        }
        Ok(Self(search))
    }

    /// The pattern of an `ILIKE` that finds the search anywhere in a field, with `%`, `_` and
    /// `\` matching only themselves.
    pub fn pattern(&self) -> String {
        let mut pattern = String::with_capacity(self.0.len() + 2);
        pattern.push('%');
        for c in self.0.chars() {
            if matches!(c, '%' | '_' | '\\') {
                pattern.push('\\');
            }
            pattern.push(c);
        }
        pattern.push('%');
        pattern
    }
}

/// A page of items, and the cursor of the next page, or `null` on the last (API-05).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
pub struct List<T> {
    /// The page's items.
    pub items: Vec<T>,
    /// The cursor of the next page, which `cursor` takes, or `null` on the last page.
    #[schema(value_type = Option<String>, required = true)]
    pub next_cursor: Option<String>,
}

/// The schema of the `limit` parameter, for a list's documentation.
pub struct Limit;

impl PartialSchema for Limit {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::Integer)
            .minimum(Some(1))
            .maximum(Some(MAX_LIMIT))
            .default(Some(DEFAULT_LIMIT.into()))
            .into()
    }
}

impl ToSchema for Limit {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("Limit")
    }
}

impl PartialSchema for Search {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .min_length(Some(1))
            .max_length(Some(SEARCH_MAX_CHARS))
            .into()
    }
}

impl ToSchema for Search {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("Search")
    }
}

/// The sort key of an item that a timestamp orders.
pub fn key(at: Timestamp, id: uuid::Uuid) -> Cursor {
    Cursor { at: at.at(), id }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_060_a_search_is_trimmed_collapsed_and_normalized() {
        let search = Search::parse("  Ren\u{65}\u{301}e \t Lo  ").unwrap();
        assert_eq!(search.0, "Ren\u{e9}e Lo");
        assert_eq!(Search::parse(" \t ").unwrap_err().code, "required");
        assert!(Search::parse(&"é".repeat(100)).is_ok());
        assert_eq!(
            Search::parse(&"a".repeat(101)).unwrap_err().code,
            "too_long"
        );
    }

    #[test]
    fn platform_060_a_search_s_wildcards_match_only_themselves() {
        let search = Search::parse(r"50%_a\b").unwrap();
        assert_eq!(search.pattern(), r"%50\%\_a\\b%");
    }

    #[test]
    fn platform_059_a_cursor_works_only_on_the_route_that_answered_it() {
        let cursor = Cursor {
            at: time::OffsetDateTime::from_unix_timestamp_nanos(1_793_500_000_123_456_000).unwrap(),
            id: uuid::Uuid::from_u128(7),
        };
        let tag = route_tag("/v1/workspaces");
        let encoded = encode(cursor, tag);
        assert_eq!(decode(&encoded, tag), Ok(cursor));
        let other = route_tag("/v1/workspaces/{workspace_id}/members");
        assert_eq!(decode(&encoded, other).unwrap_err().code, "invalid");
        assert_eq!(decode("not a cursor", tag).unwrap_err().code, "invalid");
    }

    #[test]
    fn platform_059_a_limit_is_1_to_100() {
        assert_eq!(parse_limit("1"), Ok(1));
        assert_eq!(parse_limit("100"), Ok(100));
        assert_eq!(parse_limit("0").unwrap_err().code, "out_of_range");
        assert_eq!(parse_limit("101").unwrap_err().code, "out_of_range");
        assert_eq!(parse_limit("ten").unwrap_err().code, "invalid");
    }
}
