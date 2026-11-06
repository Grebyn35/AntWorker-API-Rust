//! A route's resource: the ID of a row that its path names, and the response that answers the
//! resource, with the headers that describe it (API-04, API-13, PLATFORM-071, PLATFORM-072).

use axum::{
    extract::RawPathParams,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;

use crate::{
    errors::{Internal, Problem},
    types::Version,
};

/// The ID that the path parameter `name` holds, or `None` when the path holds no such parameter
/// or its value isn't a UUID: an ID that isn't one names no row, so the route answers it as a row
/// it doesn't find, once it has authorized the caller, never as malformed input (PLATFORM-072).
pub fn path_id(params: &RawPathParams, name: &str) -> Option<uuid::Uuid> {
    named_id(params.iter(), name)
}

/// The ID among `params`, a path's parameters by name, that the parameter `name` holds, as
/// [`path_id`] gives it.
fn named_id<'a>(
    mut params: impl Iterator<Item = (&'a str, &'a str)>,
    name: &str,
) -> Option<uuid::Uuid> {
    params
        .find(|(parameter, _)| *parameter == name)
        .and_then(|(_, value)| uuid::Uuid::parse_str(value).ok())
}

/// The response that answers a resource: its body as JSON, `200`, or `201` with the `Location` of
/// a resource just created (API-04), and, for an editable resource, the `ETag` that names its
/// version, which every response that answers it carries (PLATFORM-071).
#[derive(Debug)]
pub struct ResourceResponse<T> {
    body: T,
    status: StatusCode,
    location: Option<String>,
    version: Option<Version>,
}

impl<T: Serialize> ResourceResponse<T> {
    /// `200` with `body`.
    pub fn ok(body: T) -> Self {
        Self {
            body,
            status: StatusCode::OK,
            location: None,
            version: None,
        }
    }

    /// `201` with `body`, the resource just created, and `location`, the path that reads it.
    pub fn created(body: T, location: String) -> Self {
        Self {
            body,
            status: StatusCode::CREATED,
            location: Some(location),
            version: None,
        }
    }

    /// The response with the `ETag` of `version`, the version of the editable resource it answers.
    #[must_use]
    pub fn tagged(mut self, version: Version) -> Self {
        self.version = Some(version);
        self
    }
}

impl<T: Serialize> IntoResponse for ResourceResponse<T> {
    fn into_response(self) -> Response {
        let mut headers = Vec::new();
        if let Some(location) = self.location {
            headers.push((header::LOCATION, location));
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

    #[test]
    fn platform_072_a_path_id_that_isn_t_a_uuid_names_no_row() {
        let id = "019a0000-0000-7000-8000-000000000000";
        let params = [("workspace_id", id), ("user_id", "not-a-uuid")];

        assert_eq!(
            named_id(params.into_iter(), "workspace_id"),
            Some(uuid::Uuid::parse_str(id).unwrap())
        );
        assert_eq!(named_id(params.into_iter(), "user_id"), None);
        assert_eq!(named_id(params.into_iter(), "file_id"), None);
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
