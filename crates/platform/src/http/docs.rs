//! What the OpenAPI document says beyond the handlers' own annotations: each operation's access
//! level, and every problem the pipeline can answer, with its code and meaning (API-17,
//! ERR-13).

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::http::Method;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{
        AllOfBuilder, Components, Content, ObjectBuilder, OpenApi, Ref, RefOr, Response, Schema,
        Type,
        header::HeaderBuilder,
        path::{Operation, ParameterIn, PathItem, Paths},
    },
};

use super::Access;
use crate::errors::{ErrorCode, ProblemBody, RequestError};

/// The query parameters each method of a path accepts (API-09).
pub(super) type QueryNames = HashMap<Method, HashSet<String>>;

/// The media type of every error response (API-07).
const PROBLEM_MEDIA_TYPE: &str = "application/problem+json";

/// Where the document lists a code that `platform` answers.
enum Place {
    /// On every operation of a route group: the route's own pipeline answers it.
    RouteGroup,
    /// Among the components' responses: the router answers it when no operation matches.
    Fallback,
}

fn place(error: &RequestError) -> Place {
    match error {
        RequestError::NotFound | RequestError::MethodNotAllowed => Place::Fallback,
        RequestError::InvalidQuery(_)
        | RequestError::PayloadTooLarge
        | RequestError::TimedOut
        | RequestError::Overloaded
        | RequestError::Panicked => Place::RouteGroup,
    }
}

/// Documents each operation's access level as the extension `x-access`, and the problems the
/// pipeline can answer on any route of the group. A route that documents its own response for
/// one of those statuses keeps it, and lists the group's codes in it itself (ERR-13). Returns
/// the query parameters that each method accepts, `HEAD` taking those of the `GET` that serves
/// it.
pub(super) fn document(paths: &mut Paths, access: Access) -> QueryNames {
    let mut queries = QueryNames::new();
    for item in paths.paths.values_mut() {
        for (method, operation) in operations(item) {
            operation.extensions = Some(
                [("x-access", serde_json::Value::from(access.name()))]
                    .into_iter()
                    .collect(),
            );
            for (status, response) in group_problems() {
                operation
                    .responses
                    .responses
                    .entry(status)
                    .or_insert(RefOr::T(response));
            }
            let names = query_names(operation);
            if method == Method::GET {
                queries.insert(Method::HEAD, names.clone());
            }
            queries.insert(method, names);
        }
    }
    queries
}

/// The schema of every problem, as `Problem` in the document's components.
pub(super) fn problem_schema() -> (String, RefOr<Schema>) {
    (ProblemBody::name().into_owned(), ProblemBody::schema())
}

/// Adds to `document`'s components the problems the router answers when no operation matches a
/// request: `NotFound` for a path that no route has, and `MethodNotAllowed`, with `Allow`, for a
/// method its path doesn't serve.
pub(super) fn fallbacks(document: &mut OpenApi) {
    let components = document.components.get_or_insert_with(Components::new);
    let (name, schema) = problem_schema();
    components.schemas.entry(name).or_insert(schema);
    for error in RequestError::every() {
        if !matches!(place(&error), Place::Fallback) {
            continue;
        }
        let mut response = problem_response(&[&error]);
        let name = match error {
            RequestError::MethodNotAllowed => {
                response.headers.insert(
                    "allow".to_owned(),
                    header("The methods the path serves.").into(),
                );
                "MethodNotAllowed"
            }
            _ => "NotFound",
        };
        components
            .responses
            .insert(name.to_owned(), RefOr::T(response));
    }
}

/// Each operation of `item`, with its method.
fn operations(item: &mut PathItem) -> Vec<(Method, &mut Operation)> {
    [
        (Method::GET, &mut item.get),
        (Method::PUT, &mut item.put),
        (Method::POST, &mut item.post),
        (Method::DELETE, &mut item.delete),
        (Method::OPTIONS, &mut item.options),
        (Method::HEAD, &mut item.head),
        (Method::PATCH, &mut item.patch),
        (Method::TRACE, &mut item.trace),
    ]
    .into_iter()
    .filter_map(|(method, operation)| operation.as_mut().map(|operation| (method, operation)))
    .collect()
}

fn query_names(operation: &Operation) -> HashSet<String> {
    operation
        .parameters
        .iter()
        .flatten()
        .filter_map(|parameter| match parameter {
            RefOr::T(parameter) if matches!(parameter.parameter_in, ParameterIn::Query) => {
                Some(parameter.name.clone())
            }
            _ => None,
        })
        .collect()
}

/// The problems any route of a group can answer, one response per status. Each response lists
/// its codes, so a code a route answers is in the document with its meaning (ERR-13). The 503
/// carries `Retry-After`, which both of its codes send (OPS-07).
fn group_problems() -> BTreeMap<String, Response> {
    let every = RequestError::every();
    let mut by_status: BTreeMap<String, Vec<&dyn ErrorCode>> = BTreeMap::new();
    for error in every
        .iter()
        .filter(|error| matches!(place(error), Place::RouteGroup))
    {
        by_status
            .entry(error.category().status().as_u16().to_string())
            .or_default()
            .push(error);
    }
    by_status
        .into_iter()
        .map(|(status, errors)| {
            let mut response = problem_response(&errors);
            if status == "503" {
                response.headers.insert(
                    "retry-after".to_owned(),
                    header("How many seconds to wait before trying again.").into(),
                );
            }
            (status, response)
        })
        .collect()
}

/// A problem response whose `code` is one of `errors`' codes.
fn problem_response(errors: &[&dyn ErrorCode]) -> Response {
    let codes: Vec<&str> = errors.iter().map(|error| error.code()).collect();
    let description = errors
        .iter()
        .map(|error| format!("`{}`: {}.", error.code(), error.title()))
        .collect::<Vec<_>>()
        .join(" ");
    let schema = AllOfBuilder::new()
        .item(Ref::from_schema_name(ProblemBody::name()))
        .item(
            ObjectBuilder::new().property(
                "code",
                ObjectBuilder::new()
                    .schema_type(Type::String)
                    .enum_values(Some(codes)),
            ),
        )
        .build();
    let mut response = Response::new(description);
    response.content.insert(
        PROBLEM_MEDIA_TYPE.to_owned(),
        Content::new(Some(Schema::AllOf(schema))).into(),
    );
    response
}

/// A response header whose value is text.
fn header(description: &str) -> utoipa::openapi::header::Header {
    HeaderBuilder::new()
        .schema(Some(ObjectBuilder::new().schema_type(Type::String)))
        .description(Some(description))
        .build()
}
