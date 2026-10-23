//! What a route builder adds to the documentation of each operation it registers: the access
//! level, and the problems the pipeline can answer on any route of the group (API-17, ERR-13).

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::http::Method;
use utoipa::{
    PartialSchema, ToSchema,
    openapi::{
        AllOfBuilder, Content, ObjectBuilder, Ref, RefOr, Response, Schema, Type,
        path::{Operation, ParameterIn, PathItem, Paths},
    },
};

use super::Access;
use crate::errors::{ErrorCode, ProblemBody, RequestError};

/// The query parameters each method of a path accepts (API-09).
pub(super) type QueryNames = HashMap<Method, HashSet<String>>;

/// The media type of every error response (API-07).
const PROBLEM_MEDIA_TYPE: &str = "application/problem+json";

/// Documents each operation's access level as the extension `x-access`, and the problems the
/// pipeline can answer on any route of the group. Returns the query parameters that each
/// method documents.
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
            queries.insert(method, query_names(operation));
        }
    }
    queries
}

/// The schema of every problem, as `Problem` in the document's components.
pub(super) fn problem_schema() -> (String, RefOr<Schema>) {
    (ProblemBody::name().into_owned(), ProblemBody::schema())
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
/// its codes, so a code a route answers is in the document with its meaning (ERR-13).
fn group_problems() -> BTreeMap<String, Response> {
    let errors: [&dyn ErrorCode; 5] = [
        &RequestError::InvalidQuery(Vec::new()),
        &RequestError::PayloadTooLarge,
        &RequestError::TimedOut,
        &RequestError::Overloaded,
        &RequestError::Panicked,
    ];
    let mut by_status: BTreeMap<String, Vec<&dyn ErrorCode>> = BTreeMap::new();
    for error in errors {
        by_status
            .entry(error.category().status().as_u16().to_string())
            .or_default()
            .push(error);
    }
    by_status
        .into_iter()
        .map(|(status, errors)| (status, problem_response(&errors)))
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
