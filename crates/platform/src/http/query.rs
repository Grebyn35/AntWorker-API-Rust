//! A request's query parameters, which a list's extractors read (API-09, PLATFORM-035).

use std::collections::HashMap;

use axum::{
    extract::{FromRequestParts, MatchedPath},
    http::request::Parts,
};

use super::FieldErrors;
use crate::types::Invalid;

/// The JSON Pointer of the query parameter `name`, as `errors` names it (API-07).
pub(crate) fn parameter_pointer(name: &str) -> String {
    format!("/{}", name.replace('~', "~0").replace('/', "~1"))
}

/// A request's query parameters, by name, and the route that serves it. The route group has
/// already refused a name its documentation doesn't list (PLATFORM-035).
#[derive(Debug)]
pub struct Query {
    values: HashMap<String, String>,
    route: String,
}

impl Query {
    /// The route that serves the request, as its pattern, such as `/v1/workspaces`.
    pub(crate) fn route(&self) -> &str {
        &self.route
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Query {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let values = parts
            .uri
            .query()
            .map(|query| {
                url::form_urlencoded::parse(query.as_bytes())
                    .into_owned()
                    .collect()
            })
            .unwrap_or_default();
        let route = parts
            .extensions
            .get::<MatchedPath>()
            .map(|route| route.as_str().to_owned())
            .unwrap_or_default();
        Ok(Self { values, route })
    }
}

impl Query {
    /// The value of the parameter `name`, the last one when the query gives it more than once.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    /// Reads the parameter `name` through `parse`, recording why `errors` refuses it.
    pub fn read<T>(
        &self,
        errors: &mut FieldErrors,
        name: &str,
        parse: impl FnOnce(&str) -> Result<T, Invalid>,
    ) -> Option<Option<T>> {
        match self.get(name) {
            None => Some(None),
            Some(value) => errors
                .check(&parameter_pointer(name), parse(value))
                .map(Some),
        }
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
