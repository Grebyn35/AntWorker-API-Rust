//! A route's resource: the IDs of the rows that its path names, and the response that answers the
//! resource, with the headers that describe it (API-04, API-13, PLATFORM-071, PLATFORM-072).

use std::convert::Infallible;

use axum::{
    extract::{FromRequestParts, MatchedPath},
    http::{HeaderValue, StatusCode, header, request::Parts},
    response::{IntoResponse, Response},
};
use percent_encoding::percent_decode_str;
use serde::Serialize;

use crate::{
    errors::{Internal, Problem},
    types::{Version, read_id},
};

/// The extractor of the IDs that a request's path holds, by the names of its route's pattern,
/// which never rejects a request: a segment that percent-decodes to bytes that aren't UTF-8 holds
/// no ID, and leaves the others as they are, where axum's `Path` and `RawPathParams` refuse the
/// whole request with text that is no problem (API-07, PLATFORM-072).
#[derive(Debug, Default)]
pub struct PathIds {
    ids: Vec<(String, Option<uuid::Uuid>)>,
}

impl PathIds {
    /// The IDs of the path of `parts`, by the pattern of the route that matched it: none when no
    /// route did.
    pub(crate) fn of(parts: &Parts) -> Self {
        let pattern = parts
            .extensions
            .get::<MatchedPath>()
            .map_or("", MatchedPath::as_str);
        Self::read(pattern, parts.uri.path())
    }

    /// The IDs of `path` that the segments of `pattern` name, each segment that a `{name}` of the
    /// pattern stands for percent-decoded, as axum decodes it.
    fn read(pattern: &str, path: &str) -> Self {
        let ids = pattern
            .split('/')
            .zip(path.split('/'))
            .filter_map(|(part, segment)| {
                let name = part.strip_prefix('{')?.strip_suffix('}')?;
                // Bytes that aren't UTF-8 are a value that holds no ID, not a failure.
                let text = percent_decode_str(segment).decode_utf8().ok();
                Some((name.to_owned(), text.and_then(|text| read_id(&text))))
            })
            .collect();
        Self { ids }
    }

    /// The ID that the path parameter `name` holds, or `None` when the path holds no such
    /// parameter or its value isn't a UUID: an ID that isn't one names no row, so the route
    /// answers it as a row it doesn't find, once it has authorized the caller, never as malformed
    /// input (PLATFORM-072).
    pub fn id(&self, name: &str) -> Option<uuid::Uuid> {
        self.ids
            .iter()
            .find(|(parameter, _)| parameter == name)
            .and_then(|(_, id)| *id)
    }
}

impl<S: Send + Sync> FromRequestParts<S> for PathIds {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::of(parts))
    }
}

/// The response that answers a resource: its body as JSON, `200`, `201` with the `Location` of a
/// resource just created, or `202` with the `Location` of the work it accepted (API-04), and, for an editable resource, the `ETag` that names its
/// version, which every response that answers it carries (PLATFORM-071). The answer to a write of
/// a part of a resource, at the part's own path, names in `Content-Location` the resource that its
/// body represents (API-13). A location that no header can hold, such as one with a line break,
/// which no path the API builds from IDs has, answers the internal problem instead (ERR-08).
#[derive(Debug)]
pub struct ResourceResponse<T> {
    body: T,
    status: StatusCode,
    location: Option<String>,
    represents: Option<String>,
    version: Option<Version>,
}

impl<T: Serialize> ResourceResponse<T> {
    /// `200` with `body`.
    pub fn ok(body: T) -> Self {
        Self {
            body,
            status: StatusCode::OK,
            location: None,
            represents: None,
            version: None,
        }
    }

    /// `201` with `body`, the resource just created, and `location`, the path that reads it.
    pub fn created(body: T, location: String) -> Self {
        Self {
            body,
            status: StatusCode::CREATED,
            location: Some(location),
            represents: None,
            version: None,
        }
    }

    /// `202` with `body`, the resource of the work it accepted, and `location`, the path that
    /// reads it, as the work goes on (API-04).
    pub fn accepted(body: T, location: String) -> Self {
        Self {
            body,
            status: StatusCode::ACCEPTED,
            location: Some(location),
            represents: None,
            version: None,
        }
    }

    /// The response with the `ETag` of `version`, the version of the editable resource it answers.
    #[must_use]
    pub fn tagged(mut self, version: Version) -> Self {
        self.version = Some(version);
        self
    }

    /// The response with `path` as its `Content-Location`: the path of the resource that its body
    /// represents, where the request wrote a part of that resource at the part's own path
    /// (API-13).
    #[must_use]
    pub fn representing(mut self, path: String) -> Self {
        self.represents = Some(path);
        self
    }
}

impl<T: Serialize> IntoResponse for ResourceResponse<T> {
    fn into_response(self) -> Response {
        let mut headers = Vec::new();
        if let Some(location) = self.location {
            headers.push((header::LOCATION, location));
        }
        if let Some(path) = self.represents {
            headers.push((header::CONTENT_LOCATION, path));
        }
        if let Some(version) = self.version {
            headers.push((header::ETAG, version.etag()));
        }
        let mut response = (self.status, axum::Json(self.body)).into_response();
        for (name, value) in headers {
            match HeaderValue::try_from(value) {
                Ok(value) => {
                    response.headers_mut().insert(name, value);
                }
                Err(error) => return Problem(Internal::new(error)).into_response(),
            }
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PATTERN: &str = "/v1/workspaces/{workspace_id}/members/{user_id}/things/{thing_id}";

    #[test]
    fn platform_072_a_path_id_that_isn_t_a_uuid_names_no_row() {
        let id = "019a0000-0000-7000-8000-000000000000";
        let path = format!(
            "/v1/workspaces/{id}/members/not-a-uuid/things/019a000000007000800000000000abcd"
        );

        let ids = PathIds::read(PATTERN, &path);

        assert_eq!(ids.id("workspace_id"), Some(read_id(id).unwrap()));
        assert_eq!(ids.id("user_id"), None);
        // A UUID in another form than the one the API reads (API-11).
        assert_eq!(ids.id("thing_id"), None);
        assert_eq!(ids.id("file_id"), None);
    }

    #[test]
    fn platform_072_a_segment_that_isn_t_utf_8_holds_no_id_and_leaves_the_others() {
        let (id, other) = (
            "019a0000-0000-7000-8000-000000000000",
            "019a0000-0000-7000-8000-00000000abcd",
        );
        // The second ID percent-encoded, as a client may send any byte of a segment.
        let encoded = other.replace('-', "%2D");
        let path = format!("/v1/workspaces/{id}/members/%FF/things/{encoded}");

        let ids = PathIds::read(PATTERN, &path);

        assert_eq!(ids.id("workspace_id"), Some(read_id(id).unwrap()));
        assert_eq!(ids.id("user_id"), None);
        assert_eq!(ids.id("thing_id"), Some(read_id(other).unwrap()));
    }

    #[test]
    fn api_13_a_response_that_represents_a_resource_names_it_in_content_location() {
        let written = ResourceResponse::ok("body")
            .tagged(Version::from_database(3))
            .representing("/v1/things/1".to_owned())
            .into_response();
        let read = ResourceResponse::ok("body").into_response();

        assert_eq!(written.headers()[header::CONTENT_LOCATION], "/v1/things/1");
        assert_eq!(written.headers()[header::ETAG], "\"3\"");
        assert!(!read.headers().contains_key(header::CONTENT_LOCATION));
    }

    #[test]
    fn platform_071_a_created_resource_answers_201_with_its_location_and_etag() {
        let version = Version::from_database(3);

        let created = ResourceResponse::created("body", "/v1/things/1".to_owned())
            .tagged(version)
            .into_response();
        let read = ResourceResponse::ok("body").into_response();

        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(created.headers()[header::LOCATION], "/v1/things/1");
        assert_eq!(created.headers()[header::ETAG], "\"3\"");
        assert_eq!(read.status(), StatusCode::OK);
        assert!(read.headers().get(header::ETAG).is_none());
        assert!(read.headers().get(header::LOCATION).is_none());
    }

    #[test]
    fn err_08_a_location_that_no_header_can_hold_answers_the_fixed_internal_body() {
        let response =
            ResourceResponse::created("body", "/v1/things/\n1".to_owned()).into_response();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(response.headers().get(header::LOCATION).is_none());
        let body = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(axum::body::to_bytes(response.into_body(), usize::MAX))
            .unwrap();
        let problem: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(problem["code"], "request.internal_error");
        assert_eq!(problem["title"], "Internal error");
        assert!(problem.get("detail").is_none());
        assert!(!problem.to_string().contains("things"));
    }
}
