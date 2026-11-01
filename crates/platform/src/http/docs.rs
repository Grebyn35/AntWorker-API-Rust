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
    /// On every operation: the route's own pipeline answers it.
    Every,
    /// On every operation whose method isn't safe: the cross-site check answers it (PLATFORM-053).
    Unsafe,
    /// On every operation that authenticates its caller (AUTH-009).
    Authenticated,
    /// On every operation that takes a body (PLATFORM-054, PLATFORM-056).
    Body,
    /// Among the components' responses: the router answers it when no operation matches.
    Fallback,
}

fn place(error: &RequestError) -> Place {
    match error {
        RequestError::NotFound | RequestError::MethodNotAllowed => Place::Fallback,
        RequestError::OriginNotAllowed => Place::Unsafe,
        RequestError::Unauthenticated | RequestError::AccessTokenExpired => Place::Authenticated,
        RequestError::InvalidBody(_) | RequestError::InvalidFields(_) => Place::Body,
        RequestError::InvalidQuery(_)
        | RequestError::PayloadTooLarge
        | RequestError::RateLimited
        | RequestError::TimedOut
        | RequestError::Overloaded
        | RequestError::Unavailable
        | RequestError::Panicked => Place::Every,
    }
}

/// Whether the problems of `place` apply to `operation`, served with `method` at `access`.
fn applies(place: &Place, method: &Method, access: Access, operation: &Operation) -> bool {
    match place {
        Place::Every => true,
        Place::Unsafe => !method.is_safe(),
        Place::Authenticated => !matches!(access, Access::Public),
        Place::Body => operation.request_body.is_some(),
        Place::Fallback => false,
    }
}

/// Documents each operation's access level as the extension `x-access`, the problems the
/// pipeline can answer on it, and the rate-limit headers of every response the limiter passes
/// back through (PLATFORM-050). A route that documents its own response for one of those
/// statuses keeps it, and lists the pipeline's codes in it itself (ERR-13). Returns the query
/// parameters that each method accepts, `HEAD` taking those of the `GET` that serves it.
pub(super) fn document(paths: &mut Paths, access: Access, errors: &[&dyn ErrorCode]) -> QueryNames {
    let mut queries = QueryNames::new();
    for item in paths.paths.values_mut() {
        for (method, operation) in operations(item) {
            operation.extensions = Some(
                [("x-access", serde_json::Value::from(access.name()))]
                    .into_iter()
                    .collect(),
            );
            for (status, response) in problems(&method, access, operation, errors) {
                operation
                    .responses
                    .responses
                    .entry(status)
                    .or_insert(RefOr::T(response));
            }
            rate_limit_headers(operation);
            let names = query_names(operation);
            if method == Method::GET {
                queries.insert(Method::HEAD, names.clone());
            }
            queries.insert(method, names);
        }
    }
    queries
}

/// Adds the rate-limit headers to each of `operation`'s responses but the 5xx: a request that
/// load shedding refuses never reaches the limiter, and a panic never passes back through it
/// (PLATFORM-050).
fn rate_limit_headers(operation: &mut Operation) {
    for (status, response) in &mut operation.responses.responses {
        if status.starts_with('5') {
            continue;
        }
        if let RefOr::T(response) = response {
            response.headers.insert(
                "ratelimit".to_owned(),
                header(
                    "The requests left in the bucket closest to its limit, and the seconds until                      its window ends.",
                )
                .into(),
            );
            response.headers.insert(
                "ratelimit-policy".to_owned(),
                header("That bucket's quota, and its window in seconds.").into(),
            );
        }
    }
}

/// The schema of every problem, as `Problem` in the document's components, with the schemas it
/// refers to, such as `FieldError`.
pub(super) fn problem_schemas() -> Vec<(String, RefOr<Schema>)> {
    let mut schemas = vec![(ProblemBody::name().into_owned(), ProblemBody::schema())];
    ProblemBody::schemas(&mut schemas);
    schemas
}

/// Adds to `document`'s components the problems the router answers when no operation matches a
/// request: `NotFound` for a path that no route has, and `MethodNotAllowed`, with `Allow`, for a
/// method its path doesn't serve.
pub(super) fn fallbacks(document: &mut OpenApi) {
    let components = document.components.get_or_insert_with(Components::new);
    for (name, schema) in problem_schemas() {
        components.schemas.entry(name).or_insert(schema);
    }
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

/// The problems the pipeline can answer on `operation`, and those of `errors`, which its module
/// answers, by status, each status with the headers its problems carry.
fn problems(
    method: &Method,
    access: Access,
    operation: &Operation,
    errors: &[&dyn ErrorCode],
) -> BTreeMap<String, Response> {
    let every = RequestError::every();
    let pipeline = every
        .iter()
        .filter(|error| applies(&place(error), method, access, operation))
        .map(|error| error as &dyn ErrorCode);
    let mut by_status: BTreeMap<String, Vec<&dyn ErrorCode>> = BTreeMap::new();
    for error in pipeline.chain(errors.iter().copied()) {
        by_status
            .entry(error.category().status().as_u16().to_string())
            .or_default()
            .push(error);
    }
    by_status
        .into_iter()
        .map(|(status, errors)| {
            let mut response = problem_response(&errors);
            if status == "503" || status == "429" {
                response.headers.insert(
                    "retry-after".to_owned(),
                    header("How many seconds to wait before trying again.").into(),
                );
            }
            (status, response)
        })
        .collect()
}

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
