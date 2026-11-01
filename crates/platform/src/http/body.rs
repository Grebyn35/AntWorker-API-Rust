//! Request bodies: the JSON extractor (PLATFORM-054, PLATFORM-055), with the idempotency key of a
//! keyed route (PLATFORM-058), and the collection of every
//! field that a request type's conversion into domain types refuses (PLATFORM-056, API-10).

use axum::{
    body::Bytes,
    extract::{FromRequest, Request},
    http::header,
};
use http_body_util::{BodyExt, LengthLimitError};
use serde::de::DeserializeOwned;
use sha2::{Digest, Sha256};

use super::docs::IDEMPOTENCY_KEY;
use crate::{
    errors::{BodyFault, FieldError, Problem, RequestError},
    idempotency::IdempotencyKey,
    types::Invalid,
};

/// A JSON body, parsed into `T`. `T` declares `#[serde(deny_unknown_fields)]`, so a field it
/// doesn't take is refused (API-08).
#[derive(Debug)]
pub struct Json<T>(pub T);

impl<T, S> FromRequest<S> for Json<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Problem<RequestError>;

    async fn from_request(request: Request, _: &S) -> Result<Self, Self::Rejection> {
        let bytes = read(request).await?;
        parse(&bytes).map(Self)
    }
}

/// The JSON body of a route that accepts an `Idempotency-Key` (API-12), and the request's key, if
/// it sent one, with the fingerprint of its method, path, query and body (PLATFORM-018).
#[derive(Debug)]
pub struct Idempotent<T> {
    /// The body, parsed as [`Json`] parses it.
    pub body: T,
    /// The key, which the operation's unit of work claims.
    pub key: Option<IdempotencyKey>,
}

impl<T, S> FromRequest<S> for Idempotent<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = Problem<RequestError>;

    async fn from_request(request: Request, _: &S) -> Result<Self, Self::Rejection> {
        let method = request.method().as_str().to_owned();
        let path = request.uri().path().to_owned();
        let query = request.uri().query().unwrap_or_default().to_owned();
        let header = request.headers().get(IDEMPOTENCY_KEY).cloned();
        let bytes = read(request).await?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        let key = header
            .map(|value| {
                IdempotencyKey::parse(value.as_bytes(), &method, &path, &query, &digest)
                    .ok_or(Problem(RequestError::InvalidIdempotencyKey))
            })
            .transpose()?;
        Ok(Self {
            body: parse(&bytes)?,
            key,
        })
    }
}

/// The body of `request`, which must be sent as JSON, read up to its route's limit
/// (PLATFORM-054, PLATFORM-055).
async fn read(request: Request) -> Result<Bytes, Problem<RequestError>> {
    if !is_json(request.headers().get(header::CONTENT_TYPE)) {
        return Err(Problem(RequestError::InvalidBody(BodyFault::ContentType)));
    }
    match request.into_body().collect().await {
        Ok(collected) => Ok(collected.to_bytes()),
        Err(error) if is_too_large(&error) => Err(Problem(RequestError::PayloadTooLarge)),
        Err(_) => Err(Problem(RequestError::InvalidBody(BodyFault::Syntax))),
    }
}

/// `bytes` parsed into `T`, or the field the parser failed at (PLATFORM-054).
fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, Problem<RequestError>> {
    let deserializer = &mut serde_json::Deserializer::from_slice(bytes);
    serde_path_to_error::deserialize(deserializer).map_err(|error| {
        let fault = if error.inner().is_data() {
            BodyFault::Shape(pointer(error.path()))
        } else {
            BodyFault::Syntax
        };
        Problem(RequestError::InvalidBody(fault))
    })
}

/// Whether a `Content-Type` is `application/json`, with no parameter but a `charset` of `utf-8`.
fn is_json(value: Option<&axum::http::HeaderValue>) -> bool {
    let Some(value) = value.and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let mut parts = value.split(';').map(str::trim);
    let media_type_ok = parts
        .next()
        .is_some_and(|media_type| media_type.eq_ignore_ascii_case("application/json"));
    media_type_ok
        && parts.all(|parameter| {
            parameter.split_once('=').is_some_and(|(name, value)| {
                name.trim().eq_ignore_ascii_case("charset")
                    && value.trim().trim_matches('"').eq_ignore_ascii_case("utf-8")
            })
        })
}

fn is_too_large(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut current = Some(error);
    while let Some(error) = current {
        if error.is::<LengthLimitError>() {
            return true;
        }
        current = error.source();
    }
    false
}

/// The JSON Pointer of the field a parser failed at, or `None` when it failed at the body's top.
fn pointer(path: &serde_path_to_error::Path) -> Option<String> {
    let segments: Vec<String> = path
        .iter()
        .filter_map(|segment| match segment {
            serde_path_to_error::Segment::Seq { index } => Some(index.to_string()),
            serde_path_to_error::Segment::Map { key } => Some(escape(key)),
            serde_path_to_error::Segment::Enum { variant } => Some(escape(variant)),
            serde_path_to_error::Segment::Unknown => None,
        })
        .collect();
    (!segments.is_empty()).then(|| format!("/{}", segments.join("/")))
}

/// A segment of a JSON Pointer, with `~` and `/` escaped (RFC 6901).
fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

/// Every field a request type's conversion into domain types refused, so one response names them
/// all (API-10).
#[derive(Debug, Default)]
pub struct FieldErrors(Vec<FieldError>);

impl FieldErrors {
    /// No refused field yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The value of the field at the JSON Pointer `field`, or `None`, recording why, when its
    /// domain type refused it.
    pub fn check<T>(&mut self, field: &str, result: Result<T, Invalid>) -> Option<T> {
        result
            .map_err(|invalid| {
                self.0
                    .push(FieldError::new(field, invalid.code, invalid.message));
            })
            .ok()
    }

    /// `value`, which the checked fields built, unless a field was refused: then the 400
    /// `request.invalid_fields` that names every refused field (PLATFORM-056).
    pub fn finish<T>(self, value: Option<T>) -> Result<T, RequestError> {
        match value {
            Some(value) if self.0.is_empty() => Ok(value),
            _ => Err(RequestError::InvalidFields(self.0)),
        }
    }

    /// `value`, which the checked query parameters built, unless one was refused: then the 400
    /// `request.invalid_query` that names every refused parameter (PLATFORM-035).
    pub fn finish_query<T>(self, value: Option<T>) -> Result<T, RequestError> {
        match value {
            Some(value) if self.0.is_empty() => Ok(value),
            _ => Err(RequestError::InvalidQuery(self.0)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_054_a_body_must_be_sent_as_json() {
        let header = |value: &'static str| axum::http::HeaderValue::from_static(value);
        assert!(is_json(Some(&header("application/json"))));
        assert!(is_json(Some(&header("Application/JSON; charset=UTF-8"))));
        assert!(!is_json(None));
        assert!(!is_json(Some(&header("text/plain"))));
        assert!(!is_json(Some(&header("application/json; charset=latin1"))));
        assert!(!is_json(Some(&header("application/json-patch+json"))));
    }

    #[test]
    fn platform_056_every_refused_field_is_named() {
        let mut errors = FieldErrors::new();
        let first = errors.check("/a", Err::<u8, _>(Invalid::new("too_long", "Too long.")));
        let second = errors.check("/b", Ok::<_, Invalid>(2));
        let third = errors.check("/c/0", Err::<u8, _>(Invalid::new("required", "Required.")));
        let error = errors.finish(first.zip(second).zip(third)).unwrap_err();
        let RequestError::InvalidFields(fields) = error else {
            unreachable!("{error:?}");
        };
        let named: Vec<(&str, &str)> = fields
            .iter()
            .map(|field| (field.field.as_str(), field.code))
            .collect();
        assert_eq!(named, [("/a", "too_long"), ("/c/0", "required")]);
    }

    #[test]
    fn platform_054_a_pointer_escapes_its_segments() {
        assert_eq!(escape("a/b~c"), "a~1b~0c");
    }
}
