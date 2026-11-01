//! A request's query parameters, parsed into the type that documents them (API-09, PLATFORM-035).

use axum::{
    extract::{FromRequestParts, MatchedPath},
    http::request::Parts,
};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::errors::{FieldError, Problem, RequestError};

/// The JSON Pointer of the query parameter `name`, as `errors` names it (API-07).
pub(crate) fn parameter_pointer(name: &str) -> String {
    format!("/{}", name.replace('~', "~0").replace('/', "~1"))
}

/// A request's query parameters, parsed into `T`, and the route that serves the request. `T` is
/// the type whose `#[into_params(parameter_in = Query)]` documents the route's parameters, with
/// `#[serde(deny_unknown_fields)]`, and each field an `Option<String>`, which the route reads into
/// domain types. The route group has already refused a name its documentation doesn't list
/// (PLATFORM-035). A name given more than once keeps its last value.
#[derive(Debug)]
pub struct Query<T> {
    params: T,
    route: String,
}

impl<T: DeserializeOwned, S: Send + Sync> FromRequestParts<S> for Query<T> {
    type Rejection = Problem<RequestError>;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let values: Map<String, Value> = parts
            .uri
            .query()
            .map(|query| {
                url::form_urlencoded::parse(query.as_bytes())
                    .map(|(name, value)| (name.into_owned(), Value::String(value.into_owned())))
                    .collect()
            })
            .unwrap_or_default();
        let params = serde_path_to_error::deserialize(Value::Object(values)).map_err(|error| {
            let name = error.path().to_string();
            Problem(RequestError::InvalidQuery(vec![FieldError::new(
                parameter_pointer(&name),
                "invalid",
                "The route doesn't take this parameter as sent.",
            )]))
        })?;
        let route = parts
            .extensions
            .get::<MatchedPath>()
            .map(|route| route.as_str().to_owned())
            .unwrap_or_default();
        Ok(Self { params, route })
    }
}

impl<T> Query<T> {
    /// The parameters.
    pub fn params(&self) -> &T {
        &self.params
    }

    /// The route that serves the request, as its pattern, such as `/v1/workspaces`.
    pub fn route(&self) -> &str {
        &self.route
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_07_a_parameter_s_pointer_escapes_its_name() {
        assert_eq!(parameter_pointer("a/b~c"), "/a~1b~0c");
    }
}
