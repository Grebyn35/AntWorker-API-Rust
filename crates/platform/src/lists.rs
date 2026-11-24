//! Lists: a page's `limit` and `cursor` (API-06, PLATFORM-046), a list's `search` (API-20,
//! PLATFORM-047), its filters that take several values (PLATFORM-088), and the body of a page of
//! items (API-05), which a route reads from the parameters of its `http::Query<T>` and hands its
//! service.

use std::borrow::Cow;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Serialize;
use sha2::{Digest, Sha256};
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{ArrayBuilder, KnownFormat, ObjectBuilder, RefOr, Schema, SchemaFormat, Type},
};

use crate::{
    http::{FieldErrors, query::parameter_pointer},
    types::{Invalid, Timestamp, one_line},
};

/// How many items a page holds when the request names no `limit` (API-06).
const DEFAULT_LIMIT: u16 = 50;
/// The most items a page holds (API-06).
const MAX_LIMIT: u16 = 100;
/// The most characters a search holds (PLATFORM-047).
const SEARCH_MAX_CHARS: usize = 100;
/// The most values that a list's filter takes (PLATFORM-088).
const MAX_VALUES: usize = 50;
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
    /// recording why, when `errors` refuses either (PLATFORM-046).
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

/// The values of the query parameter `name`, a list's filter that takes several, separated by
/// commas, each through `parse`, if it was sent, or `None`, recording why, when `errors` refuses
/// it: as `too_many` with more than 50 values, and otherwise with the fault of the first of its
/// values, in their order, that is empty, as `invalid`, or that `parse` refuses (PLATFORM-088).
pub fn values<T>(
    errors: &mut FieldErrors,
    name: &str,
    value: Option<&str>,
    parse: impl Fn(&str) -> Result<T, Invalid>,
) -> Option<Option<Vec<T>>> {
    parameter(errors, name, value, |value| {
        let values: Vec<&str> = value.split(',').collect();
        if values.len() > MAX_VALUES {
            return Err(Invalid::new(
                "too_many",
                "A filter takes at most 50 values.",
            ));
        }
        values
            .into_iter()
            .map(|value| {
                if value.is_empty() {
                    Err(Invalid::new(
                        "invalid",
                        "Separate the values with single commas, and leave none empty.",
                    ))
                } else {
                    parse(value)
                }
            })
            .collect()
    })
}

/// The `limit` that `value` names: ASCII digits, after a `+`, a `-` or neither, of a number from
/// 1 to 100, refused as `out_of_range` when they write another number, whatever its size, and any
/// other text as `invalid` (API-06).
fn parse_limit(value: &str) -> Result<u16, Invalid> {
    let digits = value.strip_prefix(['+', '-']).unwrap_or(value);
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(Invalid::new("invalid", "A limit is a whole number."));
    }
    match value.parse::<u16>() {
        Ok(limit) if (1..=MAX_LIMIT).contains(&limit) => Ok(limit),
        _ => Err(Invalid::new("out_of_range", "A limit is 1 to 100.")),
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

/// What a list's items must contain, in one of the fields its route names, ignoring case: text of
/// at most 100 characters as given, trimmed, with each run of whitespace inside collapsed to one
/// space, in Unicode Normalization Form C, of 1 to 100 characters, with no control character
/// (PLATFORM-047).
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
        one_line(
            value,
            SEARCH_MAX_CHARS,
            "Enter something to search for.",
            "A search has at most 100 characters.",
        )
        .map(Self)
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

/// The schema of a list's filter that takes several values, each of `T`'s schema: an array of 1
/// to 50, which the parameter's style, `form` without `explode`, sends separated by commas
/// (PLATFORM-088). [`filter_values!`](crate::filter_values) declares a type that gives it.
pub fn values_schema<T: PartialSchema>() -> RefOr<Schema> {
    ArrayBuilder::new()
        .items(T::schema())
        .min_items(Some(1))
        .max_items(Some(MAX_VALUES))
        .into()
}

/// Declares the type `$name`, whose schema is that of a list's filter that takes several values,
/// each of `$item`'s schema, as [`values_schema`] gives it, for a
/// query's field that names it as its `value_type`, `inline`, beside the style `form` without
/// `explode` (PLATFORM-088). A type for each filter, rather than one generic type, since utoipa
/// takes a generic `value_type` only through a trait of its own macros, and rather than the
/// field's `schema_with`, with which utoipa leaves out the parameter's description, its
/// `required` and its style.
#[macro_export]
macro_rules! filter_values {
    ($(#[$doc:meta])* $visibility:vis $name:ident($item:ty)) => {
        $(#[$doc])*
        $visibility struct $name;

        impl ::utoipa::PartialSchema for $name {
            fn schema() -> ::utoipa::openapi::RefOr<::utoipa::openapi::Schema> {
                $crate::lists::values_schema::<$item>()
            }
        }

        impl ::utoipa::ToSchema for $name {
            fn name() -> ::std::borrow::Cow<'static, str> {
                ::std::borrow::Cow::Borrowed(stringify!($name))
            }
        }
    };
}

/// The schema of a moment that a list's filter takes, for its documentation: a pattern of the
/// form that `Timestamp::read` reads, whose doc comment states it (PLATFORM-088). The pattern
/// reads digits alone, so it also takes a day that no calendar holds, which `read` refuses.
pub struct Moment;

impl PartialSchema for Moment {
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .format(Some(SchemaFormat::KnownFormat(KnownFormat::DateTime)))
            .pattern(Some(
                "^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{1,6})?Z$",
            ))
            .into()
    }
}

impl ToSchema for Moment {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("Moment")
    }
}

/// The characters of a cursor: its bytes in base64url, without padding.
const CURSOR_CHARS: usize = (4 * CURSOR_LEN).div_ceil(3);

impl PartialSchema for Cursor {
    /// The schema of the `cursor` parameter, the text a list's `next_cursor` gives.
    fn schema() -> RefOr<Schema> {
        ObjectBuilder::new()
            .schema_type(Type::String)
            .min_length(Some(CURSOR_CHARS))
            .max_length(Some(CURSOR_CHARS))
            .pattern(Some("^[A-Za-z0-9_-]+$"))
            .into()
    }
}

impl ToSchema for Cursor {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("Cursor")
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
    fn platform_047_a_search_is_trimmed_collapsed_and_normalized() {
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
    fn platform_047_a_search_holds_at_most_100_characters_as_given() {
        // Of the bound's length, then once more with white space after it, which was taken once
        // trimmed.
        let longest = "a".repeat(100);
        assert!(Search::parse(&longest).is_ok());
        assert_eq!(
            Search::parse(&format!("{longest} ")).unwrap_err().code,
            "too_long"
        );
        // Within the bound, with white space around it and inside it, which normalizing removes.
        let padded = format!(" {}  {} ", "a".repeat(48), "b".repeat(48));
        assert_eq!(padded.chars().count(), 100);
        assert_eq!(
            Search::parse(&padded).unwrap().0,
            format!("{} {}", "a".repeat(48), "b".repeat(48))
        );
    }

    #[test]
    fn platform_047_a_search_holds_no_control_character() {
        for control in ["a\u{0}b", "a\u{1b}b", "a\u{7f}b"] {
            assert_eq!(
                Search::parse(control).unwrap_err().code,
                "control_character"
            );
        }
    }

    #[test]
    fn platform_047_a_search_s_wildcards_match_only_themselves() {
        let search = Search::parse(r"50%_a\b").unwrap();
        assert_eq!(search.pattern(), r"%50\%\_a\\b%");
    }

    #[test]
    fn platform_046_a_cursor_works_only_on_the_route_that_answered_it() {
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
    fn api_17_a_cursor_s_text_has_the_length_and_characters_its_schema_documents() {
        let cursor = Cursor {
            at: time::OffsetDateTime::from_unix_timestamp_nanos(-1_000).unwrap(),
            id: uuid::Uuid::max(),
        };

        let encoded = encode(cursor, route_tag("/v1/workspaces"));

        assert_eq!(encoded.len(), CURSOR_CHARS);
        assert!(
            encoded
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
            "{encoded}"
        );
    }

    use crate::errors::ErrorCode;

    fn ids(value: &str) -> Result<Option<Vec<uuid::Uuid>>, Vec<(String, &'static str)>> {
        let mut errors = FieldErrors::new();
        let read = values(&mut errors, "resource_id", Some(value), |value| {
            crate::types::read_id(value).ok_or_else(|| Invalid::new("invalid", "Send UUIDs."))
        });
        errors.finish_query(read).map_err(|error| {
            error
                .errors()
                .unwrap_or_default()
                .into_iter()
                .map(|fault| (fault.field, fault.code))
                .collect()
        })
    }

    #[test]
    fn platform_088_a_filter_takes_1_to_50_values_separated_by_commas() {
        let one = uuid::Uuid::from_u128(1);
        let two = uuid::Uuid::from_u128(2);
        assert_eq!(ids(&one.to_string()), Ok(Some(vec![one])));
        assert_eq!(
            ids(&format!("{one},{two},{one}")),
            Ok(Some(vec![one, two, one]))
        );
        let fifty = vec![one.to_string(); 50].join(",");
        assert_eq!(
            ids(&fifty).map(|ids| ids.map(|ids| ids.len())),
            Ok(Some(50))
        );
        let refused = |value: &str| ids(value).unwrap_err();
        assert_eq!(
            refused(&format!("{fifty},{one}")),
            [("/resource_id".to_owned(), "too_many")]
        );
        for value in ["", ",", &format!("{one},"), &format!("{one},,{two}")] {
            assert_eq!(
                refused(value),
                [("/resource_id".to_owned(), "invalid")],
                "{value}"
            );
        }
        assert_eq!(
            refused(&format!("{one}, {two}")),
            [("/resource_id".to_owned(), "invalid")]
        );
        let mut errors = FieldErrors::new();
        assert_eq!(
            values(&mut errors, "resource_id", None, |_| Ok(())),
            Some(None)
        );
    }

    #[test]
    fn platform_088_a_filter_answers_the_fault_of_its_first_refused_value() {
        // A route whose parse takes `1` alone, and refuses anything else with a code of its own.
        let refused = |value: &str| {
            let mut errors = FieldErrors::new();
            let read = values(&mut errors, "role", Some(value), |value| match value {
                "1" => Ok(()),
                _ => Err(Invalid::new("unknown_role", "Send 1.")),
            });
            errors
                .finish_query(read)
                .unwrap_err()
                .errors()
                .unwrap_or_default()
                .into_iter()
                .map(|fault| fault.code)
                .collect::<Vec<_>>()
        };
        assert_eq!(refused("1,,x"), ["invalid"]);
        assert_eq!(refused("1,x,"), ["unknown_role"]);
        assert_eq!(refused("x,,1"), ["unknown_role"]);
        assert_eq!(refused(&vec!["x"; 51].join(",")), ["too_many"]);
    }

    #[test]
    fn platform_046_a_limit_is_1_to_100() {
        assert_eq!(parse_limit("1"), Ok(1));
        assert_eq!(parse_limit("100"), Ok(100));
        assert_eq!(parse_limit("0").unwrap_err().code, "out_of_range");
        assert_eq!(parse_limit("101").unwrap_err().code, "out_of_range");
        assert_eq!(parse_limit("ten").unwrap_err().code, "invalid");
    }

    #[test]
    fn api_06_a_whole_number_outside_1_to_100_is_out_of_range() {
        let codes = ["-1", "70000", "1000000000000000000000000000000", "1.5", "-"]
            .map(|value| parse_limit(value).unwrap_err().code);

        assert_eq!(
            codes,
            [
                "out_of_range",
                "out_of_range",
                "out_of_range",
                "invalid",
                "invalid"
            ]
        );
    }
}
